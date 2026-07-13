use std::path::Path;

use git2::{ErrorCode, ObjectType, Repository, Status};

use crate::error::{AppError, AppResult};
use crate::git::apply::{apply_patch, checkout_index_path, ApplyTarget};
use crate::git::diff::{file_diff, DiffLineKind, FileDiff};
use crate::git::patch::{build_patch, HunkPatchPayload, PatchLineKind};
use crate::git::paths::validate_repo_relative_path;
use crate::git::repo::open_repository;

/// Stage the whole-file change for `path` into the index.
///
/// For an existing (created/modified) file we `add_path`; for a file that was
/// removed from the working tree `add_path` would error, so we `remove_path`
/// instead. Either way the index is written back to disk.
pub fn stage_file(repo: &Repository, path: &str) -> AppResult<()> {
    let rel = Path::new(path);
    let mut index = repo.index()?;

    let exists_in_workdir = repo
        .workdir()
        .map(|w| w.join(path).exists())
        .unwrap_or(false);

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

    match repo.head() {
        Ok(head) => {
            let head_obj = head.peel(ObjectType::Commit)?;
            repo.reset_default(Some(&head_obj), [rel])?;
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            let mut index = repo.index()?;
            index.remove_path(rel)?;
            index.write()?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
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

    // `WT_NEW` means present in the working tree but absent from the index, i.e.
    // an untracked file — there is no index content to restore, so remove it.
    if status.contains(Status::WT_NEW) {
        if let Some(workdir) = repo.workdir() {
            let full = workdir.join(path);
            std::fs::remove_file(&full)?;
        }
        return Ok(());
    }

    let workdir = repo
        .workdir()
        .ok_or_else(|| AppError::msg("Repository has no working tree"))?;
    checkout_index_path(workdir, path)
}

/// Apply exactly one hunk (`payload`) to `workdir`, but only after re-verifying,
/// from a freshly recomputed diff, that the hunk is still exactly what the UI
/// displayed. This is the server-side guard that makes the frontend's flags
/// advisory-only.
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
        return Err(AppError::msg(
            "Cannot hunk-stage an untracked file; stage the whole file instead",
        ));
    }

    if !hunk_still_present(&fresh, payload) {
        return Err(AppError::msg(
            "The file changed since this diff was displayed — refresh and try again.",
        ));
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
fn same_kind(fresh: DiffLineKind, payload: PatchLineKind) -> bool {
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
    use git2::{RepositoryInitOptions, Signature};
    use std::fs;
    use tempfile::{tempdir, TempDir};

    fn init_repo(dir: &Path) -> Repository {
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(dir, &opts).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test User").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        repo
    }

    fn commit_file(repo: &Repository, dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parents)
            .unwrap();
    }

    fn setup() -> (TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());
        (dir, repo)
    }

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
            entry.mode, 0o100755,
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
            entry.mode, 0o120000,
            "symlink must be staged as a symlink, not a text blob"
        );
    }
}
