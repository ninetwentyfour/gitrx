use std::path::Path;

use git2::{ErrorCode, IndexEntry, IndexTime, Repository, Status};

use crate::error::{AppError, AppResult};
use crate::git::apply::{apply_patch, checkout_index_path, ApplyTarget};
use crate::git::diff::{file_diff, DiffLineKind, FileDiff};
use crate::git::patch::{build_patch, HunkPatchPayload, PatchLineKind};
use crate::git::paths::validate_repo_relative_path;
use crate::git::repo::{build_status, open_repository, FileStatus};

/// Stage the whole-file change for `path` into the index.
///
/// For an existing (created/modified) file we `add_path`; for a file that was
/// removed from the working tree `add_path` would error, so we `remove_path`
/// instead. Either way the index is written back to disk.
pub fn stage_file(repo: &Repository, path: &str) -> AppResult<()> {
    let rel = Path::new(path);
    let mut index = repo.index()?;

    // H5: when `path` is the NEW side of a working-tree rename (the index still
    // holds the OLD path), stage the rename atomically — drop the old entry and
    // add the new one — so we never leave a half-staged rename (old still
    // tracked at HEAD content, new added as a fresh file).
    if let Some(old) = workdir_rename_source(repo, path)? {
        index.remove_path(Path::new(&old))?;
        index.add_path(rel)?;
        index.write()?;
        return Ok(());
    }

    // H4: use `symlink_metadata` (an lstat that does NOT follow the link) rather
    // than `Path::exists` (which follows it). A tracked symlink retargeted to a
    // missing path is still present as a link, so it must be re-staged (updating
    // the 120000 entry), not misread as a deletion. Only a genuinely absent
    // path routes to `remove_path`.
    let exists_in_workdir = repo
        .workdir()
        .is_some_and(|w| std::fs::symlink_metadata(w.join(path)).is_ok());

    if exists_in_workdir {
        index.add_path(rel)?;
    } else {
        index.remove_path(rel)?;
    }
    index.write()?;
    Ok(())
}

/// Unstage the change for `path`, resetting that index entry back to HEAD.
///
/// With commits present we `reset_default` the single path against the HEAD
/// commit. On an unborn HEAD (empty repository) there is nothing to reset to, so
/// we simply drop the path from the index, returning it to an untracked state.
pub fn unstage_file(repo: &Repository, path: &str) -> AppResult<()> {
    let rel = Path::new(path);

    // `Repository::reset_default` is implemented on top of pathspec matching and
    // does NOT set DISABLE_PATHSPEC_MATCH, so a literal filename like
    // `data[1].txt` would be fnmatch-interpreted as a character class — resetting
    // the wrong sibling (`data1.txt`) and missing the intended file. We instead
    // do literal index surgery against the HEAD tree, whose lookups
    // (`Tree::get_path`) and the explicit `Index::add`/`Index::remove_path`
    // entry APIs are all literal (no globbing).
    let head_tree = match repo.head() {
        Ok(head) => head.peel_to_tree()?,
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            // No HEAD to reset to — drop the path from the index entirely,
            // returning it to an untracked state.
            let mut index = repo.index()?;
            index.remove_path(rel)?;
            index.write()?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let mut index = repo.index()?;
    match head_tree.get_path(rel) {
        // Path exists at HEAD: rebuild its index entry from the HEAD blob so the
        // staged change is reverted to the committed version.
        Ok(tree_entry) => {
            let entry = head_tree_index_entry(path, &tree_entry);
            index.add(&entry)?;
        }
        // Absent from HEAD (added since HEAD, or otherwise not committed): drop
        // it from the index so it returns to untracked.
        Err(e) if e.code() == ErrorCode::NotFound => {
            index.remove_path(rel)?;
        }
        Err(e) => return Err(e.into()),
    }
    index.write()?;
    Ok(())
}

/// Build a stage-0 `IndexEntry` for `path` from its HEAD-tree entry.
///
/// Only `id`, `mode`, and `path` carry meaning for an index-vs-HEAD comparison;
/// the stat fields are zeroed. `Index::add` recomputes the path-length flags, so
/// `flags: 0` is safe. `path` is taken as the caller's literal `&str` bytes (no
/// pathspec interpretation).
fn head_tree_index_entry(path: &str, tree_entry: &git2::TreeEntry) -> IndexEntry {
    IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        // A git tree-entry filemode is always a valid positive POSIX mode
        // (e.g. 0o100644, 0o40000); libgit2 never yields a negative value, so
        // the sign is provably preserved.
        #[allow(clippy::cast_sign_loss)]
        mode: tree_entry.filemode() as u32,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: tree_entry.id(),
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    }
}

/// If `path` is the NEW side of a working-tree rename, return its OLD path.
///
/// Detection is delegated to [`build_status`] so it agrees byte-for-byte with
/// what the UI displayed (its `renames_index_to_workdir` pass is the single
/// source of truth for rename pairing); matching is on the exact literal path.
/// Returns `None` for the common non-rename case.
fn workdir_rename_source(repo: &Repository, path: &str) -> AppResult<Option<String>> {
    let status = build_status(repo)?;
    Ok(status
        .unstaged
        .iter()
        .find(|e| e.path == path && e.status == FileStatus::Renamed)
        .and_then(|e| e.old_path.clone()))
}

/// Discard the working-tree changes for `path`.
///
/// - Untracked file: delete it from disk.
/// - Tracked file: restore the path from the **index** (not HEAD) with force, so
///   any already-staged portion of a partially-staged file is preserved while
///   the unstaged working-tree edits are thrown away.
///
/// The tracked restore goes through `git checkout-index` (literal filename)
/// rather than libgit2's `checkout_index`, whose pathspec matching would treat a
/// filename like `data[1].txt` as a glob and revert the wrong file. `status_file`
/// is itself a literal single-path lookup, so the untracked branch is safe.
pub fn discard_file(repo: &Repository, path: &str) -> AppResult<()> {
    let rel = Path::new(path);
    let status = repo.status_file(rel)?;

    // H5: `path` may be the NEW side of a working-tree rename (the index still
    // holds the OLD path). `status_file` is not rename-aware and reports it as
    // `WT_NEW`, which would fall into the untracked branch below and DELETE the
    // file — discarding the user's edits AND leaving the original gone. Detect
    // the rename first: restore the ORIGINAL from the index and remove the new
    // file, so the working tree returns to its pre-rename state.
    if let Some(old) = workdir_rename_source(repo, path)? {
        let workdir = repo
            .workdir()
            .ok_or_else(|| AppError::validation("Repository has no working tree"))?;
        checkout_index_path(workdir, &old)?;
        std::fs::remove_file(workdir.join(path))?;
        return Ok(());
    }

    // `WT_NEW` means present in the working tree but absent from the index, i.e.
    // an untracked file — there is no index content to restore, so remove it.
    // With no working tree there is nothing to remove and no index content to
    // fall back to, so surface that rather than silently succeeding.
    if status.contains(Status::WT_NEW) {
        let workdir = repo
            .workdir()
            .ok_or_else(|| AppError::validation("Repository has no working tree"))?;
        std::fs::remove_file(workdir.join(path))?;
        return Ok(());
    }

    let workdir = repo
        .workdir()
        .ok_or_else(|| AppError::validation("Repository has no working tree"))?;
    checkout_index_path(workdir, path)
}

/// Apply exactly one hunk (`payload`) to `workdir`.
///
/// The application happens only after re-verifying, from a freshly recomputed
/// diff, that the hunk is still exactly what the UI displayed. This is the
/// server-side guard that makes the frontend's flags advisory-only.
///
/// Steps:
/// 1. Lexically validate the payload's path(s).
/// 2. Recompute `file_diff` with the payload's own `context_lines` and `staged`
///    side, straight from disk.
/// 3. Re-derive `is_untracked` from that fresh diff (never trust the payload's
///    flag) and refuse untracked files — they are staged whole-file.
/// 4. Require the payload's hunk to still be present verbatim (header string AND
///    the exact `(kind, content)` line sequence). A miss means the file moved
///    under us; error out without touching the index/working tree.
/// 5. Only then build the patch and shell it to `git apply`.
///
/// Factored out of the command layer so it is directly unit/integration
/// testable. It opens its own `Repository` from `workdir`, so it can run inside a
/// `spawn_blocking` closure without holding a non-`Send` handle across `.await`.
pub fn apply_hunk_verified(
    workdir: &Path,
    payload: &HunkPatchPayload,
    target: ApplyTarget,
) -> AppResult<()> {
    validate_repo_relative_path(workdir, &payload.path)?;
    if let Some(old) = payload.old_path.as_deref() {
        validate_repo_relative_path(workdir, old)?;
    }

    let repo = open_repository(workdir)?;
    let fresh = file_diff(&repo, &payload.path, payload.staged, payload.context_lines)?;

    if fresh.is_untracked {
        return Err(AppError::validation(
            "Cannot hunk-stage an untracked file; stage the whole file instead",
        ));
    }

    // H3: a file with non-UTF-8 bytes is rendered lossily (U+FFFD) by the diff
    // layer, so a reconstructed hunk patch would write the replaced bytes back
    // and corrupt the file. Refuse hunk staging; whole-file staging stays
    // byte-exact (index/checkout APIs) and remains allowed.
    if fresh.is_lossy {
        return Err(AppError::non_utf8_file());
    }

    // H2: a forged `payload.old_path` becomes `diff --git a/<forged> b/<path>`,
    // which `git apply` treats as an implicit rename — stage-deleting the forged
    // path. The old_path the UI captured must match the freshly recomputed one
    // exactly (both `None` for the common, non-rename case); otherwise the diff
    // moved under us or the payload is lying.
    if payload.old_path != fresh.old_path {
        return Err(AppError::stale_hunk());
    }

    if !hunk_still_present(&fresh, payload) {
        return Err(AppError::stale_hunk());
    }

    let patch = build_patch(payload)?;
    apply_patch(workdir, &patch, target)
}

/// True when `payload`'s hunk still exists verbatim in the freshly computed
/// `fresh` diff: same header string, same number of lines, and the same
/// `(kind, content)` for every line, in order.
fn hunk_still_present(fresh: &FileDiff, payload: &HunkPatchPayload) -> bool {
    fresh.hunks.iter().any(|h| {
        h.header == payload.header
            && h.lines.len() == payload.lines.len()
            && h.lines
                .iter()
                .zip(&payload.lines)
                .all(|(fresh_line, payload_line)| {
                    same_kind(fresh_line.kind, payload_line.kind)
                        && fresh_line.content == payload_line.content
                })
    })
}

/// Do a fresh-diff line kind and a payload line kind denote the same role?
const fn same_kind(fresh: DiffLineKind, payload: PatchLineKind) -> bool {
    matches!(
        (fresh, payload),
        (DiffLineKind::Context, PatchLineKind::Context)
            | (DiffLineKind::Add, PatchLineKind::Add)
            | (DiffLineKind::Del, PatchLineKind::Del)
            | (DiffLineKind::NoNewline, PatchLineKind::NoNewline)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::build_status;
    use crate::git::repo::FileStatus;
    use crate::test_support::{commit_file, setup};
    use git2::Signature;
    use std::fs;

    #[test]
    fn stage_modified_file_moves_it_to_staged() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "line1\n");

        fs::write(dir.path().join("a.txt"), "line1\nline2\n").unwrap();

        stage_file(&repo, "a.txt").unwrap();

        let status = build_status(&repo).unwrap();
        let staged = status
            .staged
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("a.txt staged");
        assert_eq!(staged.status, FileStatus::Modified);
        assert!(status.unstaged.iter().all(|e| e.path != "a.txt"));
    }

    #[test]
    fn stage_deleted_file_records_deletion() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "line1\n");

        fs::remove_file(dir.path().join("a.txt")).unwrap();

        stage_file(&repo, "a.txt").unwrap();

        let status = build_status(&repo).unwrap();
        let staged = status
            .staged
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("a.txt staged as deleted");
        assert_eq!(staged.status, FileStatus::Deleted);
        assert!(status.unstaged.iter().all(|e| e.path != "a.txt"));
    }

    #[test]
    fn unstage_returns_change_to_unstaged() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "line1\n");

        // Modify and stage.
        fs::write(dir.path().join("a.txt"), "line1\nline2\n").unwrap();
        stage_file(&repo, "a.txt").unwrap();

        unstage_file(&repo, "a.txt").unwrap();

        let status = build_status(&repo).unwrap();
        assert!(status.staged.iter().all(|e| e.path != "a.txt"));
        let unstaged = status
            .unstaged
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("a.txt back in unstaged");
        assert_eq!(unstaged.status, FileStatus::Modified);
    }

    #[test]
    fn unstage_on_unborn_head_untracks_file() {
        let (dir, repo) = setup();

        fs::write(dir.path().join("new.txt"), "hello\n").unwrap();
        stage_file(&repo, "new.txt").unwrap();

        // Confirm it staged as new before unstaging.
        let staged = build_status(&repo).unwrap();
        assert!(staged.staged.iter().any(|e| e.path == "new.txt"));

        unstage_file(&repo, "new.txt").unwrap();

        let status = build_status(&repo).unwrap();
        assert!(status.staged.iter().all(|e| e.path != "new.txt"));
        let unstaged = status
            .unstaged
            .iter()
            .find(|e| e.path == "new.txt")
            .expect("new.txt untracked");
        assert_eq!(unstaged.status, FileStatus::Untracked);
    }

    #[test]
    fn discard_tracked_reverts_workdir_but_keeps_staged() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "l1\nl2\nl3\n");

        // Stage a change to line 1.
        fs::write(dir.path().join("a.txt"), "L1\nl2\nl3\n").unwrap();
        stage_file(&repo, "a.txt").unwrap();

        // Then make a further, unstaged change to line 3.
        fs::write(dir.path().join("a.txt"), "L1\nl2\nL3\n").unwrap();

        discard_file(&repo, "a.txt").unwrap();

        // Working tree reverts to the *index* content (staged line 1 kept, the
        // unstaged line 3 edit discarded).
        let on_disk = fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(on_disk, "L1\nl2\nl3\n");

        let status = build_status(&repo).unwrap();
        // Staged half survives.
        let staged = status
            .staged
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("staged change survives discard");
        assert_eq!(staged.status, FileStatus::Modified);
        // No unstaged change remains.
        assert!(status.unstaged.iter().all(|e| e.path != "a.txt"));
    }

    #[test]
    fn discard_untracked_deletes_from_disk() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");

        let untracked = dir.path().join("u.txt");
        fs::write(&untracked, "temp\n").unwrap();
        assert!(untracked.exists());

        discard_file(&repo, "u.txt").unwrap();

        assert!(!untracked.exists());
        let status = build_status(&repo).unwrap();
        assert!(status.unstaged.iter().all(|e| e.path != "u.txt"));
    }

    #[test]
    fn discard_bracket_filename_reverts_only_that_file() {
        // Regression: a filename with glob metacharacters must be discarded
        // literally. The sibling `data1.txt` (which the class `[1]` would match)
        // must be left untouched.
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "data[1].txt", "orig-bracket\n");
        commit_file(&repo, dir.path(), "data1.txt", "orig-plain\n");

        fs::write(dir.path().join("data[1].txt"), "mod-bracket\n").unwrap();
        fs::write(dir.path().join("data1.txt"), "mod-plain\n").unwrap();

        discard_file(&repo, "data[1].txt").unwrap();

        // Only the bracket file was reverted (to its index/HEAD content).
        assert_eq!(
            fs::read_to_string(dir.path().join("data[1].txt")).unwrap(),
            "orig-bracket\n"
        );
        // The sibling that a glob would have swept up is untouched.
        assert_eq!(
            fs::read_to_string(dir.path().join("data1.txt")).unwrap(),
            "mod-plain\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stage_untracked_executable_records_exec_mode() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");

        let script = dir.path().join("run.sh");
        fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        stage_file(&repo, "run.sh").unwrap();

        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index.get_path(Path::new("run.sh"), 0).unwrap();
        assert_eq!(
            entry.mode, 0o100_755,
            "exec bit must survive whole-file staging"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stage_untracked_symlink_records_symlink_mode() {
        use std::os::unix::fs::symlink;

        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "target.txt", "hello\n");
        symlink("target.txt", dir.path().join("link.txt")).unwrap();

        stage_file(&repo, "link.txt").unwrap();

        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index.get_path(Path::new("link.txt"), 0).unwrap();
        assert_eq!(
            entry.mode, 0o120_000,
            "symlink must be staged as a symlink, not a text blob"
        );
    }

    /// Commit whatever is currently staged in the index, on top of HEAD.
    fn commit_index(repo: &Repository, message: &str) {
        let mut index = repo.index().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap();
    }

    /// A 20-line body, LF terminated (enough for reliable rename detection).
    fn twenty_lines() -> String {
        (1..=20).fold(String::new(), |mut s, n| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line{n}");
            s
        })
    }

    /// `twenty_lines()` with line 5 edited (still ~95% similar -> a rename).
    fn twenty_lines_edited() -> String {
        (1..=20)
            .map(|n| {
                if n == 5 {
                    "line5-changed\n".to_string()
                } else {
                    format!("line{n}\n")
                }
            })
            .collect()
    }

    // H1: `unstage_file` must reset the LITERAL path via HEAD-tree index surgery,
    // not `reset_default` (whose pathspec fnmatch would treat `data[1].txt` as a
    // character class and reset the sibling `data1.txt` instead). Mirrors the
    // `discard_bracket_filename_reverts_only_that_file` oracle.
    #[test]
    fn unstage_bracket_filename_unstages_only_that_file() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "data[1].txt", "orig-bracket\n");
        commit_file(&repo, dir.path(), "data1.txt", "orig-plain\n");

        // Modify + stage both.
        fs::write(dir.path().join("data[1].txt"), "mod-bracket\n").unwrap();
        fs::write(dir.path().join("data1.txt"), "mod-plain\n").unwrap();
        stage_file(&repo, "data[1].txt").unwrap();
        stage_file(&repo, "data1.txt").unwrap();
        let before = build_status(&repo).unwrap();
        assert!(before.staged.iter().any(|e| e.path == "data[1].txt"));
        assert!(before.staged.iter().any(|e| e.path == "data1.txt"));

        unstage_file(&repo, "data[1].txt").unwrap();

        let status = build_status(&repo).unwrap();
        // Only the bracket file returned to unstaged.
        assert!(status.staged.iter().all(|e| e.path != "data[1].txt"));
        let unstaged = status
            .unstaged
            .iter()
            .find(|e| e.path == "data[1].txt")
            .expect("bracket file back in unstaged");
        assert_eq!(unstaged.status, FileStatus::Modified);
        // The sibling a glob would have swept up stays staged.
        assert!(
            status.staged.iter().any(|e| e.path == "data1.txt"),
            "sibling must remain staged"
        );
        assert!(status.unstaged.iter().all(|e| e.path != "data1.txt"));
    }

    // H1: unstaging a file that was ADDED since HEAD (no HEAD-tree entry) drops it
    // from the index back to untracked.
    #[test]
    fn unstage_added_since_head_returns_to_untracked() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");
        fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
        stage_file(&repo, "fresh.txt").unwrap();

        unstage_file(&repo, "fresh.txt").unwrap();

        let status = build_status(&repo).unwrap();
        assert!(status.staged.iter().all(|e| e.path != "fresh.txt"));
        let unstaged = status
            .unstaged
            .iter()
            .find(|e| e.path == "fresh.txt")
            .expect("fresh.txt back to untracked");
        assert_eq!(unstaged.status, FileStatus::Untracked);
    }

    // H4(a): a tracked symlink retargeted to a MISSING path must be re-staged
    // (its 120000 entry updated to the new target), not misread as a deletion.
    #[cfg(unix)]
    #[test]
    fn stage_tracked_symlink_retargeted_to_missing_updates_link() {
        use std::os::unix::fs::symlink;

        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");
        symlink("seed.txt", dir.path().join("link.txt")).unwrap();
        stage_file(&repo, "link.txt").unwrap();
        commit_index(&repo, "add link");

        // Retarget the link at a path that does not exist.
        fs::remove_file(dir.path().join("link.txt")).unwrap();
        symlink("does-not-exist", dir.path().join("link.txt")).unwrap();

        stage_file(&repo, "link.txt").unwrap();

        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index
            .get_path(Path::new("link.txt"), 0)
            .expect("link still tracked, not staged as a deletion");
        assert_eq!(entry.mode, 0o120_000, "must remain a symlink entry");
        let blob = repo.find_blob(entry.id).unwrap();
        assert_eq!(
            blob.content(),
            b"does-not-exist",
            "new (broken) link target must be staged"
        );
    }

    // H4(b): an UNTRACKED broken symlink must be added (120000), not silently
    // skipped as if it did not exist.
    #[cfg(unix)]
    #[test]
    fn stage_untracked_broken_symlink_adds_link_entry() {
        use std::os::unix::fs::symlink;

        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");
        symlink("nowhere", dir.path().join("brk.txt")).unwrap();

        stage_file(&repo, "brk.txt").unwrap();

        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index
            .get_path(Path::new("brk.txt"), 0)
            .expect("broken symlink must be added, not a no-op");
        assert_eq!(entry.mode, 0o120_000);
        let blob = repo.find_blob(entry.id).unwrap();
        assert_eq!(blob.content(), b"nowhere");
    }

    // H5: discarding the NEW side of a working-tree rename restores the ORIGINAL
    // from the index and deletes the new file (rather than deleting the new file
    // and losing the original, as the WT_NEW branch alone would).
    #[test]
    fn discard_workdir_rename_restores_original_and_removes_new() {
        let (dir, repo) = setup();
        let body = twenty_lines();
        commit_file(&repo, dir.path(), "a.txt", &body);

        // Working-tree rename a.txt -> b.txt with an edit; index still has a.txt.
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        fs::write(dir.path().join("b.txt"), twenty_lines_edited()).unwrap();

        // Precondition: the UI sees this as a rename on the new side.
        let before = build_status(&repo).unwrap();
        let ren = before
            .unstaged
            .iter()
            .find(|e| e.path == "b.txt")
            .expect("b.txt present");
        assert_eq!(ren.status, FileStatus::Renamed);
        assert_eq!(ren.old_path.as_deref(), Some("a.txt"));

        discard_file(&repo, "b.txt").unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            body,
            "original restored to its index content"
        );
        assert!(
            !dir.path().join("b.txt").exists(),
            "the renamed file must be removed"
        );
    }

    // H5: staging the NEW side of a working-tree rename stages it ATOMICALLY —
    // the old index entry is dropped and the new one added in one shot, with the
    // working tree left untouched — so no half-staged rename can occur.
    #[test]
    fn stage_workdir_rename_stages_atomically() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", &twenty_lines());

        fs::remove_file(dir.path().join("a.txt")).unwrap();
        let new_body = twenty_lines_edited();
        fs::write(dir.path().join("b.txt"), &new_body).unwrap();

        stage_file(&repo, "b.txt").unwrap();

        // Index: a.txt gone, b.txt present with the new content.
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        assert!(
            index.get_path(Path::new("a.txt"), 0).is_none(),
            "old path must be removed from the index"
        );
        let entry = index
            .get_path(Path::new("b.txt"), 0)
            .expect("new path staged");
        let blob = repo.find_blob(entry.id).unwrap();
        assert_eq!(blob.content(), new_body.as_bytes());

        // Working tree untouched by the staging.
        assert!(dir.path().join("b.txt").exists());
        assert!(
            !dir.path().join("a.txt").exists(),
            "staging a rename must not touch the working tree"
        );

        // build_status now reports a staged rename.
        let status = build_status(&repo).unwrap();
        let staged = status
            .staged
            .iter()
            .find(|e| e.path == "b.txt")
            .expect("b.txt staged");
        assert_eq!(staged.status, FileStatus::Renamed);
        assert_eq!(staged.old_path.as_deref(), Some("a.txt"));
    }
}
