use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Application-level error used across the git layer and the Tauri command
/// boundary.
///
/// This is an *internally-tagged* discriminated union (`#[serde(tag = "name")]`):
/// every variant serializes to `{ "name": "<variant>", "message": "<text>" }`,
/// which the TypeScript frontend consumes as a discriminated union
/// (`src/types/ipc.ts`). The `name` tags are camelCase (`rename_all`), matching
/// the rest of the crate's serde surface (`RepoStatus`, `FileEntry`, …); a unit
/// test (`serde_tag_shape`) pins the exact wire shape the frontend mirrors.
///
/// `thiserror`'s `#[error("{message}")]` makes `Display` echo the `message`
/// field verbatim, so `to_string()` (used in Rust-side logging and tests) always
/// equals the serialized `message`. The `From<git2::Error>` / `From<io::Error>`
/// conversions carry the historical `"Git error:"` / `"IO error:"` prefixes in
/// that `message` so the text reaching the UI is unchanged by this migration.
///
/// Every variant carries a `message: String` (even the fixed-text ones) so the
/// wire shape is uniform: TypeScript can read `.message` on any variant without a
/// per-variant presence check.
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "camelCase")]
pub enum AppError {
    /// No repository is bound to the calling window (plain launch / nothing to
    /// restore). The frontend renders this silently rather than as a failure.
    #[error("{message}")]
    NoRepoOpen { message: String },

    /// A hunk (un)stage/discard was rejected because the file changed since the
    /// diff the payload was built from was displayed — the server-side freshness
    /// guard (`apply_hunk_verified`). The frontend re-syncs and re-renders.
    #[error("{message}")]
    StaleHunk { message: String },

    /// Per-hunk staging refused because the file contains non-UTF-8 bytes (the
    /// lossy text round-trip would corrupt it); whole-file staging stays allowed.
    #[error("{message}")]
    NonUtf8File { message: String },

    /// Committing failed because no git author/committer identity is configured.
    /// Carries the actionable "set your git user.name/email" instructions.
    #[error("{message}")]
    IdentityMissing { message: String },

    /// A non-amend commit was attempted with an empty index (nothing staged).
    #[error("{message}")]
    NothingStaged { message: String },

    /// A commit was attempted with an empty / whitespace-only message.
    #[error("{message}")]
    EmptyMessage { message: String },

    /// A window was closed mid-flight, so a repo binding must not be
    /// (re)established for its now-dead label.
    #[error("{message}")]
    WindowClosed { message: String },

    /// A caller-supplied path or payload was rejected (traversal, absolute path,
    /// injection, wrong hunk direction, unsupported image type, working-tree
    /// preconditions, …). The single bucket for "you sent something invalid".
    #[error("{message}")]
    Validation { message: String },

    /// A status/diff working-tree walk aborted because libgit2 (and every other
    /// Windows-native git tool) could not read a file — in practice a WSL-created
    /// symlink stored as a Linux reparse point (Windows error 1920). libgit2 fails
    /// the *entire* walk on the first such entry, so the repo is otherwise
    /// unusable; this variant replaces the cryptic raw error with a diagnosis and
    /// actionable guidance. `message` carries the offending path plus the raw
    /// libgit2 text (see [`AppError::unreadable_work_tree_file`]).
    #[error("{message}")]
    UnreadableWorkTreeFile { message: String },

    /// Catch-all for git failures: `git2::Error` conversions and `git` CLI
    /// invocation/apply failures, plus internal backend faults (poisoned locks,
    /// panicked blocking tasks) that are not clearly I/O.
    #[error("{message}")]
    Git { message: String },

    /// Catch-all for filesystem / I/O failures (`std::io::Error` conversions and
    /// equivalent filesystem operations).
    #[error("{message}")]
    Io { message: String },
}

/// Substring libgit2 emits when the OS rejects a working-tree path during a
/// status/diff walk. It comes from a single site — `git_error_set(GIT_ERROR_OS,
/// "invalid path for filesystem '%s'", path)` in libgit2's `util/fs_path.c`
/// (`git_fs_path_set_error`, the `EINVAL`/`ENAMETOOLONG` branch) — so the class
/// is always `Os` and the code `InvalidSpec`. The substring is the only
/// path-bearing, libgit2-version-stable signal, so it is the authoritative
/// check; the class/code merely corroborate it (see [`is_unreadable_work_tree_file`]).
///
/// On Windows a WSL-created symlink (a Linux reparse point) trips this: the OS
/// returns error 1920 ("The file cannot be accessed by the system", appended to
/// the message by libgit2's `GIT_ERROR_OS` formatting), no native process can
/// read it, and the whole walk aborts on that first entry.
const UNREADABLE_WORK_TREE_MARKER: &str = "invalid path for filesystem";

/// Whether a libgit2 error message is the "OS could not read this working-tree
/// path" failure class.
///
/// Matches [`UNREADABLE_WORK_TREE_MARKER`]. A pure predicate on the message text,
/// so it is unit-testable without a live filesystem.
#[must_use]
pub fn is_unreadable_work_tree_file(message: &str) -> bool {
    message.contains(UNREADABLE_WORK_TREE_MARKER)
}

/// Extract the first single-quoted path from a libgit2 message, e.g. the
/// `C:/Users/.../guidelines.md` in `invalid path for filesystem '...': ...`.
/// Returns `None` when the message carries no `'...'` pair.
fn quoted_path(message: &str) -> Option<&str> {
    let start = message.find('\'')? + 1;
    let rest = message.get(start..)?;
    let end = rest.find('\'')?;
    rest.get(..end)
}

impl From<git2::Error> for AppError {
    fn from(err: git2::Error) -> Self {
        let message = err.message();
        // Classify the unreadable-worktree-file failure *before* the generic
        // catch-all so the UI gets a diagnosis instead of the raw libgit2 text.
        if is_unreadable_work_tree_file(message) {
            return Self::unreadable_work_tree_file(message);
        }
        // Preserve the historical "Git error: <libgit2 message>" text so the
        // string reaching the UI is byte-identical to the pre-migration Display.
        Self::Git {
            message: format!("Git error: {message}"),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        // Preserve the historical "IO error: <message>" text (see above).
        Self::Io {
            message: format!("IO error: {err}"),
        }
    }
}

impl AppError {
    /// No repository is open for the calling window.
    #[must_use]
    pub fn no_repo_open() -> Self {
        Self::NoRepoOpen {
            message: "No repository open".to_string(),
        }
    }

    /// The displayed diff went stale before the hunk could be applied.
    #[must_use]
    pub fn stale_hunk() -> Self {
        Self::StaleHunk {
            message: "The file changed since this diff was displayed — refresh and try again."
                .to_string(),
        }
    }

    /// The file is non-UTF-8, so per-hunk patch staging is unsafe.
    #[must_use]
    pub fn non_utf8_file() -> Self {
        Self::NonUtf8File {
            message: "File contains non-UTF-8 text — use whole-file staging".to_string(),
        }
    }

    /// No git identity is configured; carries the actionable fix.
    #[must_use]
    pub fn identity_missing() -> Self {
        Self::IdentityMissing {
            message: "Git identity is not configured. Set it with:\n  \
                 git config --global user.name \"Your Name\"\n  \
                 git config --global user.email \"you@example.com\""
                .to_string(),
        }
    }

    /// Nothing is staged for a non-amend commit.
    #[must_use]
    pub fn nothing_staged() -> Self {
        Self::NothingStaged {
            message: "No staged changes to commit".to_string(),
        }
    }

    /// The commit message is empty or whitespace-only.
    #[must_use]
    pub fn empty_message() -> Self {
        Self::EmptyMessage {
            message: "Commit message cannot be empty".to_string(),
        }
    }

    /// A window closed before its repo binding could be established.
    #[must_use]
    pub fn window_closed() -> Self {
        Self::WindowClosed {
            message: "window closed".to_string(),
        }
    }

    /// A working-tree file could not be read during a status/diff walk. Builds a
    /// diagnosis from the raw libgit2 message: the offending path (extracted from
    /// the quoted portion) plus actionable guidance, with the raw message appended
    /// in parentheses for fidelity. Platform-neutral phrasing — the detection is
    /// message-based, and macOS cannot produce this libgit2 error in practice.
    #[must_use]
    pub fn unreadable_work_tree_file(raw: &str) -> Self {
        let subject = quoted_path(raw).map_or_else(
            || "a file in this repository".to_string(),
            |p| format!("'{p}'"),
        );
        Self::UnreadableWorkTreeFile {
            message: format!(
                "gitrx can't read {subject} — on Windows this usually means a WSL-created \
                 symlink, which Windows programs (git included) cannot follow. Replace it with a \
                 regular file, or recreate it as a Windows symlink (requires Developer Mode), then \
                 retry. ({raw})"
            ),
        }
    }

    /// A caller-supplied path/payload was rejected.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    /// A git operation failed (CLI invocation, apply, or an internal backend
    /// fault used as the generic catch-all).
    pub fn git(message: impl Into<String>) -> Self {
        Self::Git {
            message: message.into(),
        }
    }

    /// A filesystem / I/O operation failed.
    pub fn io(message: impl Into<String>) -> Self {
        Self::Io {
            message: message.into(),
        }
    }
}

/// Convenience result alias for the git layer.
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the exact serialized wire shape (`{ "name", "message" }` with
    /// camelCase tags) the TypeScript `AppError` union in `src/types/ipc.ts`
    /// mirrors. If a tag changes here, the frontend type must change in lockstep.
    #[test]
    fn serde_tag_shape() {
        let cases = [
            (AppError::no_repo_open(), "noRepoOpen"),
            (AppError::stale_hunk(), "staleHunk"),
            (AppError::non_utf8_file(), "nonUtf8File"),
            (AppError::identity_missing(), "identityMissing"),
            (AppError::nothing_staged(), "nothingStaged"),
            (AppError::empty_message(), "emptyMessage"),
            (AppError::window_closed(), "windowClosed"),
            (
                AppError::unreadable_work_tree_file("invalid path for filesystem 'x'"),
                "unreadableWorkTreeFile",
            ),
            (AppError::validation("bad path"), "validation"),
            (AppError::git("boom"), "git"),
            (AppError::io("disk"), "io"),
        ];
        for (err, expected_name) in cases {
            let json = serde_json::to_value(&err).unwrap();
            assert_eq!(
                json.get("name").and_then(|v| v.as_str()),
                Some(expected_name),
                "tag mismatch for {err:?}",
            );
            // Every variant serializes a `message` string, and `Display` echoes it.
            assert_eq!(
                json.get("message").and_then(|v| v.as_str()),
                Some(err.to_string().as_str()),
                "message field must equal Display for {err:?}",
            );
        }
    }

    #[test]
    fn git_and_io_from_preserve_historical_prefix() {
        let io = AppError::from(std::io::Error::other("nope"));
        assert_eq!(io.to_string(), "IO error: nope");
        assert!(matches!(io, AppError::Io { .. }));
    }

    /// The pure predicate fires on the libgit2 marker substring and only on it.
    #[test]
    fn classifier_matches_only_the_filesystem_marker() {
        assert!(is_unreadable_work_tree_file(
            "invalid path for filesystem 'C:/x': The file cannot be accessed by the system."
        ));
        assert!(!is_unreadable_work_tree_file(
            "Git error: some other failure"
        ));
    }

    /// The offending path is lifted out of the quoted portion of the raw message.
    #[test]
    fn unreadable_variant_carries_path_and_raw_message() {
        let raw = "invalid path for filesystem 'C:/Users/x/.ai/guidelines/guidelines.md': \
             The file cannot be accessed by the system.";
        let err = AppError::unreadable_work_tree_file(raw);
        assert!(matches!(err, AppError::UnreadableWorkTreeFile { .. }));
        let msg = err.to_string();
        // Carries the extracted path, actionable guidance, and the raw text.
        assert!(msg.contains("guidelines.md"));
        assert!(msg.contains("WSL-created"));
        assert!(msg.contains("Developer Mode"));
        assert!(
            msg.contains(raw),
            "raw libgit2 message appended for fidelity"
        );
    }

    /// A message with no quoted path still classifies, with a generic subject.
    #[test]
    fn unreadable_variant_without_quoted_path_falls_back() {
        let err = AppError::unreadable_work_tree_file("invalid path for filesystem");
        let msg = err.to_string();
        assert!(msg.contains("a file in this repository"));
    }

    /// A constructed git2 error carrying the real class/code/message (as libgit2
    /// raises it in `git_fs_path_set_error`) routes through `From` to the new
    /// variant, not the generic `Git` catch-all.
    #[test]
    fn from_git2_error_classifies_unreadable_worktree_file() {
        let raw = "invalid path for filesystem 'C:/repo/link': \
                   The file cannot be accessed by the system.";
        let git_err = git2::Error::new(git2::ErrorCode::InvalidSpec, git2::ErrorClass::Os, raw);
        let app: AppError = git_err.into();
        assert!(matches!(app, AppError::UnreadableWorkTreeFile { .. }));
        assert!(app.to_string().contains("C:/repo/link"));
    }

    /// An unrelated git2 error still folds into the generic `Git` variant with the
    /// historical prefix — the classifier must not over-match.
    #[test]
    fn from_git2_error_leaves_unrelated_errors_generic() {
        let git_err = git2::Error::from_str("reference not found");
        let app: AppError = git_err.into();
        assert!(matches!(app, AppError::Git { .. }));
        assert_eq!(app.to_string(), "Git error: reference not found");
    }
}
