/// Byte cap applied to every diff the app builds (`DiffOptions::max_size`).
///
/// libgit2 marks any blob larger than this as binary automatically, so its
/// contents are never loaded: oversized files report `isBinary=true`, zero
/// additions/deletions, and empty hunks. This is the memory guard that stops a
/// multi-hundred-MB (or GB) file in `git status` / a single-file diff from being
/// text-rendered into RAM. 8 MiB.
pub const MAX_DIFF_BYTES: i64 = 8 * 1024 * 1024;

pub mod apply;
pub mod commit;
pub mod diff;
pub mod patch;
pub mod paths;
pub mod repo;
pub mod stage;

pub use apply::{apply_patch, ApplyTarget};
pub use commit::{commit, head_commit_message};
pub use diff::{file_diff, FileDiff};
pub use patch::{build_patch, HunkPatchPayload};
pub use paths::validate_repo_relative_path;
pub use repo::{build_status, open_repository, resolve_cli_repo_path, RepoStatus};
pub use stage::{apply_hunk_verified, discard_file, stage_file, unstage_file};
