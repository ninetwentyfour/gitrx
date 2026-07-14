//! Pipe a reconstructed patch to the `git` CLI to (un)stage or discard a hunk.
//!
//! We shell out to `git apply` rather than using libgit2's `Diff::apply` because
//! `git apply --cached` is the battle-tested path for index-level patching and it
//! surfaces precise, user-facing errors ("patch does not apply") that we forward
//! straight to the UI.
//!
//! All calls are synchronous (`std::process`); callers that need to stay off the
//! UI thread should wrap [`apply_patch`] in `tokio::task::spawn_blocking`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::error::{AppError, AppResult};

/// Where and how a patch should be applied.
#[derive(Debug, Clone, Copy)]
pub enum ApplyTarget {
    /// Stage the hunk: `git apply --cached`.
    Index,
    /// Unstage the hunk: `git apply --cached --reverse` (patch built from the
    /// *staged* diff).
    IndexReverse,
    /// Discard the hunk from the working tree: `git apply --reverse` (patch built
    /// from the *unstaged* diff).
    WorkdirReverse,
}

impl ApplyTarget {
    /// The target-specific `git apply` flags (excluding the common trailing
    /// `--whitespace=nowarn`, optional `--unidiff-zero`, and `-`).
    const fn mode_flags(self) -> &'static [&'static str] {
        match self {
            Self::Index => &["--cached"],
            Self::IndexReverse => &["--cached", "--reverse"],
            Self::WorkdirReverse => &["--reverse"],
        }
    }
}

/// Assemble the full `git` argument list for `target` and `patch`.
///
/// A zero-context patch (context slider at 0) is rejected by `git apply`'s
/// default safety check, so `--unidiff-zero` is added only in that case; patches
/// that carry context keep the stricter (safer) matching.
fn build_args(target: ApplyTarget, patch: &[u8]) -> Vec<&'static str> {
    let mut args = vec!["apply"];
    args.extend_from_slice(target.mode_flags());
    args.push("--whitespace=nowarn");
    if !patch_has_context(patch) {
        args.push("--unidiff-zero");
    }
    args.push("-");
    args
}

/// True if any hunk-body line is a context line (starts with a space). Header
/// lines before the first `@@` are ignored so `--- `/`+++ ` don't count.
fn patch_has_context(patch: &[u8]) -> bool {
    let mut in_hunk = false;
    for line in patch.split(|&b| b == b'\n') {
        if line.starts_with(b"@@") {
            in_hunk = true;
            continue;
        }
        if in_hunk && line.first() == Some(&b' ') {
            return true;
        }
    }
    false
}

/// Apply `patch` in `workdir` for the given `target`.
///
/// On a non-zero exit the git process's stderr is included in the returned
/// [`AppError`] so the caller can show messages like "patch does not apply".
pub fn apply_patch(workdir: &Path, patch: &[u8], target: ApplyTarget) -> AppResult<()> {
    let git = git_executable()?;

    let mut child = Command::new(git)
        .args(build_args(target, patch))
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::git(format!("Failed to spawn git: {e}")))?;

    // Feed the patch from a dedicated writer thread while the calling thread
    // drains stdout/stderr via `wait_with_output`. Writing on the same thread we
    // wait on can deadlock: for a large patch git may block writing to its
    // (unread) stdout/stderr pipes before it finishes reading stdin, so neither
    // side makes progress.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::git("Failed to open git stdin"))?;
    let patch_owned = patch.to_vec();
    let writer = std::thread::spawn(move || {
        // Ignore write errors (e.g. a broken pipe when git rejects the patch and
        // exits early). The non-zero exit status below reports the real failure;
        // a closed pipe must never panic this worker and abort the process.
        let _ = stdin.write_all(&patch_owned);
        // Dropping `stdin` closes the pipe so git sees EOF.
    });

    let output = child
        .wait_with_output()
        .map_err(|e| AppError::git(format!("Failed to wait on git: {e}")))?;

    // Join the writer after git has exited. A panic in it is swallowed here for
    // the same reason: the process must not die because a pipe closed.
    let _ = writer.join();

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let detail = if stderr.is_empty() {
        format!("git apply exited with status {}", output.status)
    } else {
        format!("git apply failed: {stderr}")
    };
    Err(AppError::git(detail))
}

/// Restore a single working-tree file from the index, matching `path`
/// **literally** (no pathspec globbing).
///
/// git2 0.19's `CheckoutBuilder` exposes no way to disable pathspec matching, so
/// `Repository::checkout_index` would treat a path like `data[1].txt` as a glob
/// and revert the wrong file. `git checkout-index` takes literal filenames (not
/// pathspecs), applies the repo's smudge filters, and restores the recorded mode
/// — exactly the semantics we want for a partial-discard-safe whole-file revert.
///
/// `--` guards against a leading-dash path being read as an option; callers are
/// still expected to have run `validate_repo_relative_path` first.
pub fn checkout_index_path(workdir: &Path, path: &str) -> AppResult<()> {
    let git = git_executable()?;
    let output = Command::new(git)
        .args(["checkout-index", "--force", "--"])
        .arg(path)
        .current_dir(workdir)
        .output()
        .map_err(|e| AppError::git(format!("Failed to spawn git checkout-index: {e}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let detail = if stderr.is_empty() {
        format!("git checkout-index exited with status {}", output.status)
    } else {
        format!("git checkout-index failed: {stderr}")
    };
    Err(AppError::git(detail))
}

/// Locate a usable `git` executable, caching the result for the process lifetime.
///
/// Tries `git` on `PATH` first, then the common absolute fallbacks. A candidate
/// is accepted only if `git --version` runs successfully.
fn git_executable() -> AppResult<&'static Path> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(locate_git).as_deref().ok_or_else(|| {
        AppError::git(
            "git executable not found (checked PATH, /usr/bin/git, /opt/homebrew/bin/git)",
        )
    })
}

fn locate_git() -> Option<PathBuf> {
    // Resolve the bare `git` name to an ABSOLUTE path ourselves by scanning
    // `PATH`, skipping empty or relative entries. We must NOT hand a relative
    // `"git"` to `Command`: our `git apply` calls run with `current_dir(workdir)`
    // set, so a relative program name combined with an empty/relative `PATH`
    // entry (`""`, `"."`) could execute a repo-local `./git` — arbitrary code
    // from an untrusted repository. An absolute path removes that vector.
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.as_os_str().is_empty() || !dir.is_absolute() {
                continue;
            }
            let candidate = dir.join("git");
            if candidate.is_file() && is_runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    for candidate in ["/usr/bin/git", "/opt/homebrew/bin/git"] {
        let pb = PathBuf::from(candidate);
        if pb.exists() && is_runnable(&pb) {
            return Some(pb);
        }
    }
    None
}

fn is_runnable(git: &Path) -> bool {
    Command::new(git)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_file, setup};
    use std::fs;

    #[test]
    fn apply_to_index_stages_change() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "f.txt", "a\nb\nc\n");
        fs::write(dir.path().join("f.txt"), "a\nB\nc\n").unwrap();

        let patch = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
        apply_patch(dir.path(), patch, ApplyTarget::Index).unwrap();

        // Index blob now reflects the staged edit. Reload from disk since the
        // external `git apply --cached` wrote `.git/index` behind git2's back.
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index.get_path(Path::new("f.txt"), 0).unwrap();
        let idx_blob = repo.find_blob(entry.id).unwrap();
        assert_eq!(idx_blob.content(), b"a\nB\nc\n");
    }

    #[test]
    fn failed_apply_surfaces_git_stderr() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "f.txt", "a\nb\nc\n");

        // Context line "WRONG" does not match the file -> git refuses.
        let patch = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n WRONG\n-b\n+B\n c\n";
        let err = apply_patch(dir.path(), patch, ApplyTarget::Index).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("apply"),
            "message should mention git apply: {msg}"
        );
    }
}
