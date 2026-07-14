use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{Diff, DiffOptions, ErrorCode, Repository, Status, StatusEntry, StatusOptions};
use serde::Serialize;

use crate::error::{AppError, AppResult};

/// The kind of change affecting a single file, matching the frontend contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
    Typechange,
}

/// A single changed file, on either the staged or unstaged side.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub staged: bool,
    pub is_binary: bool,
    pub additions: usize,
    pub deletions: usize,
}

/// Full repository status snapshot returned to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    pub repo_name: String,
    /// Canonical working-tree path of the repository (the repo dir itself for a
    /// bare repo). This is the same value stored as the active repo path, and is
    /// what the frontend persists as `lastRepoPath` to reopen on next launch.
    pub repo_path: String,
    pub branch: String,
    pub unstaged: Vec<FileEntry>,
    pub staged: Vec<FileEntry>,
    pub head_has_commits: bool,
}

/// Per-path line statistics collected from a diff.
#[derive(Debug, Default, Clone, Copy)]
struct LineStats {
    additions: usize,
    deletions: usize,
    is_binary: bool,
}

/// Open and validate a git repository at (or above) `path`.
///
/// Uses `Repository::discover` so a path anywhere inside a working tree resolves
/// to the enclosing repository.
pub fn open_repository(path: &Path) -> AppResult<Repository> {
    Repository::discover(path).map_err(|e| {
        AppError::validation(format!(
            "Not a git repository: {} ({})",
            path.display(),
            e.message()
        ))
    })
}

/// Resolve a CLI-provided path argument to an absolute, canonical directory.
///
/// Canonicalizing up front means relative arguments (".", "../foo") resolve
/// against the launch cwd regardless of where the app binary lives, and any
/// symlinks in the path are collapsed before the value is stored or watched.
/// A leading `~` is intentionally *not* expanded here: the invoking shell is
/// expected to expand it, matching how every other CLI behaves.
///
/// Returns `Err` when the path does not exist, cannot be accessed, or is not a
/// directory. Whether it is (inside) a git repository is decided separately by
/// [`open_repository`].
pub fn resolve_cli_repo_path(arg: &str) -> AppResult<PathBuf> {
    let canonical = std::fs::canonicalize(arg)
        .map_err(|e| AppError::validation(format!("Cannot resolve path '{arg}': {e}")))?;
    if !canonical.is_dir() {
        return Err(AppError::validation(format!(
            "Not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Build a `RepoStatus` snapshot for an already-opened repository.
pub fn build_status(repo: &Repository) -> AppResult<RepoStatus> {
    let repo_name = repo_name(repo);
    let repo_path = repo_path_string(repo);
    let (branch, head_has_commits) = branch_info(repo);

    // Line-stat / binary information joined per path from two diffs.
    let staged_stats = collect_stats(&staged_diff(repo)?);
    let unstaged_stats = collect_stats(&unstaged_diff(repo)?);

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut staged: Vec<FileEntry> = Vec::new();
    let mut unstaged: Vec<FileEntry> = Vec::new();

    for entry in statuses.iter() {
        let s = entry.status();

        if let Some((path, old_path, status)) = staged_descriptor(&entry, s) {
            let stats = staged_stats.get(&path).copied().unwrap_or_default();
            staged.push(FileEntry {
                path,
                old_path,
                status,
                staged: true,
                is_binary: stats.is_binary,
                additions: stats.additions,
                deletions: stats.deletions,
            });
        }

        if let Some((path, old_path, status)) = unstaged_descriptor(&entry, s) {
            let stats = unstaged_stats.get(&path).copied().unwrap_or_default();
            unstaged.push(FileEntry {
                path,
                old_path,
                status,
                staged: false,
                is_binary: stats.is_binary,
                additions: stats.additions,
                deletions: stats.deletions,
            });
        }
    }

    // Staged entries never include untracked files, so a plain path sort suffices.
    staged.sort_by(|a, b| a.path.cmp(&b.path));
    // Unstaged: untracked (new) files sort to the bottom as a distinct group. The
    // key is `(is_untracked, path)` — `false < true`, so tracked entries come first,
    // then untracked, each alphabetical within its own group.
    unstaged.sort_by(|a, b| {
        let a_untracked = a.status == FileStatus::Untracked;
        let b_untracked = b.status == FileStatus::Untracked;
        a_untracked
            .cmp(&b_untracked)
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(RepoStatus {
        repo_name,
        repo_path,
        branch,
        unstaged,
        staged,
        head_has_commits,
    })
}

/// Canonical working-tree path of the repository as a string.
///
/// Mirrors how `open_repo` and the CLI-arg startup derive the stored active repo
/// path: the working tree for a normal repo, falling back to the repo dir for a
/// bare repo (which has no working tree).
fn repo_path_string(repo: &Repository) -> String {
    repo.workdir()
        .unwrap_or_else(|| repo.path())
        .to_string_lossy()
        .into_owned()
}

/// Directory basename of the working tree (or of the repo dir for bare repos).
fn repo_name(repo: &Repository) -> String {
    repo.workdir()
        .and_then(|p| p.file_name())
        .or_else(|| repo.path().parent().and_then(|p| p.file_name()))
        .map_or_else(
            || "repository".to_string(),
            |s| s.to_string_lossy().into_owned(),
        )
}

/// Returns `(branch label, head_has_commits)`.
///
/// - Normal branch  -> shorthand ("main").
/// - Detached HEAD  -> "(detached: <7-char oid>)".
/// - Unborn HEAD    -> symbolic target ref name (empty repo), `head_has_commits=false`.
fn branch_info(repo: &Repository) -> (String, bool) {
    match repo.head() {
        Ok(head_ref) => {
            if repo.head_detached().unwrap_or(false) {
                let short = head_ref.target().map_or_else(
                    || "unknown".to_string(),
                    |oid| {
                        let full = oid.to_string();
                        full.chars().take(7).collect::<String>()
                    },
                );
                (format!("(detached: {short})"), true)
            } else {
                let name = head_ref.shorthand().unwrap_or("HEAD").to_string();
                (name, true)
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            let name = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target().map(str::to_string))
                .map_or_else(
                    || "HEAD".to_string(),
                    |t| t.strip_prefix("refs/heads/").unwrap_or(&t).to_string(),
                );
            (name, false)
        }
        Err(_) => ("HEAD".to_string(), false),
    }
}

/// Diff of HEAD tree -> index (staged changes). On an unborn HEAD the old tree
/// is `None`, so everything in the index appears as additions.
fn staged_diff(repo: &Repository) -> AppResult<Diff<'_>> {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = DiffOptions::new();
    opts.include_typechange(true)
        .max_size(crate::git::MAX_DIFF_BYTES);
    Ok(repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?)
}

/// Diff of index -> workdir (unstaged changes), including untracked content so
/// untracked files report their line count as additions.
fn unstaged_diff(repo: &Repository) -> AppResult<Diff<'_>> {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .include_typechange(true)
        .max_size(crate::git::MAX_DIFF_BYTES);
    Ok(repo.diff_index_to_workdir(None, Some(&mut opts))?)
}

/// Walk a diff once, collecting per-path additions/deletions and binary flags.
fn collect_stats(diff: &Diff) -> HashMap<String, LineStats> {
    let mut map = HashMap::new();
    for (i, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned());
        let Some(path) = path else { continue };

        let mut stats = LineStats::default();
        // Computing the patch also forces libgit2 to set the binary flags on
        // the underlying delta, which `delta` (a shared borrow) reflects.
        let patch = git2::Patch::from_diff(diff, i).ok().flatten();
        if delta.new_file().is_binary() || delta.old_file().is_binary() {
            stats.is_binary = true;
        } else if let Some(patch) = patch {
            if let Ok((_context, additions, deletions)) = patch.line_stats() {
                stats.additions = additions;
                stats.deletions = deletions;
            }
        }
        map.insert(path, stats);
    }
    map
}

/// Build the staged-side `(path, old_path, status)` for a status entry, or
/// `None` if there is no INDEX_* change.
fn staged_descriptor(
    entry: &StatusEntry,
    s: Status,
) -> Option<(String, Option<String>, FileStatus)> {
    if s.contains(Status::INDEX_RENAMED) {
        let (path, old) = rename_paths(entry.head_to_index(), entry.path());
        return Some((path, old, FileStatus::Renamed));
    }
    let status = if s.contains(Status::INDEX_NEW) {
        FileStatus::Added
    } else if s.contains(Status::INDEX_DELETED) {
        FileStatus::Deleted
    } else if s.contains(Status::INDEX_TYPECHANGE) {
        FileStatus::Typechange
    } else if s.contains(Status::INDEX_MODIFIED) {
        FileStatus::Modified
    } else {
        return None;
    };
    Some((entry_path(entry.path()), None, status))
}

/// Build the unstaged-side `(path, old_path, status)` for a status entry, or
/// `None` if there is no WT_* / conflict change.
///
/// Conflicted files are reported on the unstaged side (resolution happens in the
/// working tree), matching how they surface for staging UIs.
fn unstaged_descriptor(
    entry: &StatusEntry,
    s: Status,
) -> Option<(String, Option<String>, FileStatus)> {
    if s.contains(Status::CONFLICTED) {
        return Some((entry_path(entry.path()), None, FileStatus::Conflicted));
    }
    if s.contains(Status::WT_RENAMED) {
        let (path, old) = rename_paths(entry.index_to_workdir(), entry.path());
        return Some((path, old, FileStatus::Renamed));
    }
    let status = if s.contains(Status::WT_NEW) {
        FileStatus::Untracked
    } else if s.contains(Status::WT_DELETED) {
        FileStatus::Deleted
    } else if s.contains(Status::WT_TYPECHANGE) {
        FileStatus::Typechange
    } else if s.contains(Status::WT_MODIFIED) {
        FileStatus::Modified
    } else {
        return None;
    };
    Some((entry_path(entry.path()), None, status))
}

/// Extract `(new_path, Some(old_path))` from a rename delta, falling back to the
/// entry's raw path when delta data is unavailable.
fn rename_paths(
    delta: Option<git2::DiffDelta>,
    fallback: Option<&str>,
) -> (String, Option<String>) {
    delta.map_or_else(
        || (entry_path(fallback), None),
        |delta| {
            let new_path = delta.new_file().path().map_or_else(
                || entry_path(fallback),
                |p| p.to_string_lossy().into_owned(),
            );
            let old_path = delta
                .old_file()
                .path()
                .map(|p| p.to_string_lossy().into_owned());
            (new_path, old_path)
        },
    )
}

fn entry_path(path: Option<&str>) -> String {
    path.unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo;
    use git2::Signature;
    use std::fs;
    use tempfile::tempdir;

    fn commit_baseline(repo: &Repository, dir: &Path) {
        fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();
        fs::write(dir.join("b.txt"), "b1\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.add_path(Path::new("b.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "baseline", &tree, &[])
            .unwrap();
    }

    #[test]
    fn splits_staged_unstaged_and_untracked() {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());
        commit_baseline(&repo, dir.path());

        // Unstaged modification to a.txt.
        fs::write(dir.path().join("a.txt"), "line1\nline2\nline3\n").unwrap();

        // Staged modification to b.txt (write + add so index differs from HEAD
        // and workdir matches index -> staged only).
        fs::write(dir.path().join("b.txt"), "b1\nb2\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("b.txt")).unwrap();
        index.write().unwrap();

        // Untracked c.txt with three lines.
        fs::write(dir.path().join("c.txt"), "c1\nc2\nc3\n").unwrap();

        let status = build_status(&repo).unwrap();

        assert!(status.head_has_commits);
        assert_eq!(status.branch, "main");
        assert_eq!(
            status.repo_name,
            dir.path().file_name().unwrap().to_string_lossy()
        );
        // repo_path is the canonical working-tree path (git2 appends a trailing
        // separator to the workdir), so compare against the resolved workdir.
        assert_eq!(status.repo_path, repo.workdir().unwrap().to_string_lossy());

        let a = status
            .unstaged
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("a.txt unstaged");
        assert_eq!(a.status, FileStatus::Modified);
        assert!(!a.staged);

        let c = status
            .unstaged
            .iter()
            .find(|e| e.path == "c.txt")
            .expect("c.txt unstaged");
        assert_eq!(c.status, FileStatus::Untracked);
        assert_eq!(c.additions, 3);
        assert_eq!(c.deletions, 0);
        assert!(!c.is_binary);

        let b = status
            .staged
            .iter()
            .find(|e| e.path == "b.txt")
            .expect("b.txt staged");
        assert_eq!(b.status, FileStatus::Modified);
        assert!(b.staged);

        // b.txt must NOT appear unstaged (workdir == index).
        assert!(status.unstaged.iter().all(|e| e.path != "b.txt"));
    }

    #[test]
    fn unstaged_sorts_tracked_before_untracked_each_alphabetical() {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());
        // Baseline commits two tracked files (a.txt, b.txt).
        commit_baseline(&repo, dir.path());

        // Modify both tracked files so they appear unstaged, out of alpha order on
        // disk-write order to prove the sort — write b before a intentionally.
        fs::write(dir.path().join("b.txt"), "b1\nb2\n").unwrap();
        fs::write(dir.path().join("a.txt"), "line1\nline2\nline3\n").unwrap();

        // Two untracked files, again written out of order (z before m) to prove the
        // within-group alphabetical sort.
        fs::write(dir.path().join("z.txt"), "z\n").unwrap();
        fs::write(dir.path().join("m.txt"), "m\n").unwrap();

        let status = build_status(&repo).unwrap();

        // Tracked (alphabetical) first, then untracked (alphabetical).
        let paths: Vec<&str> = status.unstaged.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt", "m.txt", "z.txt"]);

        // The first two are tracked (Modified), the last two untracked.
        assert_eq!(status.unstaged[0].status, FileStatus::Modified);
        assert_eq!(status.unstaged[1].status, FileStatus::Modified);
        assert_eq!(status.unstaged[2].status, FileStatus::Untracked);
        assert_eq!(status.unstaged[3].status, FileStatus::Untracked);
    }

    #[test]
    fn resolve_cli_repo_path_canonicalizes_relative_and_dot() {
        let dir = tempdir().unwrap();
        // canonicalize the tempdir itself so we compare against the resolved
        // form (macOS /var -> /private/var symlink, etc.).
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let sub = expected.join("nested");
        fs::create_dir(&sub).unwrap();

        // Absolute path resolves to itself.
        assert_eq!(resolve_cli_repo_path(sub.to_str().unwrap()).unwrap(), sub);

        // Relative "." and ".." resolve against the process cwd. Restore the cwd
        // through a drop guard so a failing assert below can't leak the mutated
        // cwd into other tests in this binary.
        let _guard = CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(&sub).unwrap();
        assert_eq!(resolve_cli_repo_path(".").unwrap(), sub);
        assert_eq!(resolve_cli_repo_path("..").unwrap(), expected);
    }

    /// Restores the process current directory when dropped (including on panic),
    /// so a cwd-mutating test cannot contaminate sibling tests.
    struct CwdGuard(PathBuf);

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn resolve_cli_repo_path_rejects_missing_and_files() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(resolve_cli_repo_path(missing.to_str().unwrap()).is_err());

        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        assert!(resolve_cli_repo_path(file.to_str().unwrap()).is_err());
    }

    #[test]
    fn oversized_untracked_file_is_binary_with_zero_stats() {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());
        commit_baseline(&repo, dir.path());

        // 9 MiB of newlines — over the 8 MiB diff cap. Allocated once (not built
        // in a hot push loop) to keep the fixture memory-light.
        let big = "\n".repeat(9 * 1024 * 1024);
        fs::write(dir.path().join("big.txt"), &big).unwrap();

        let status = build_status(&repo).unwrap();
        let e = status
            .unstaged
            .iter()
            .find(|e| e.path == "big.txt")
            .expect("big.txt untracked");
        assert_eq!(e.status, FileStatus::Untracked);
        assert!(e.is_binary, "oversized untracked file must report binary");
        assert_eq!(e.additions, 0);
        assert_eq!(e.deletions, 0);
    }

    #[test]
    fn oversized_tracked_modification_is_binary_with_zero_stats() {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());
        commit_baseline(&repo, dir.path());

        // Grow tracked a.txt (small, non-binary at HEAD) past the cap in the
        // working tree: the workdir side of the unstaged diff exceeds max_size.
        let big = "\n".repeat(9 * 1024 * 1024);
        fs::write(dir.path().join("a.txt"), &big).unwrap();

        let status = build_status(&repo).unwrap();
        let e = status
            .unstaged
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("a.txt modified");
        assert_eq!(e.status, FileStatus::Modified);
        assert!(e.is_binary, "oversized modification must report binary");
        assert_eq!(e.additions, 0);
        assert_eq!(e.deletions, 0);
    }

    #[test]
    fn empty_repo_reports_unborn_head() {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());

        fs::write(dir.path().join("u.txt"), "u1\n").unwrap();

        let status = build_status(&repo).unwrap();

        assert!(!status.head_has_commits);
        assert_eq!(status.branch, "main");
        assert!(status
            .unstaged
            .iter()
            .any(|e| e.path == "u.txt" && e.status == FileStatus::Untracked));
        assert!(status.staged.is_empty());
    }
}
