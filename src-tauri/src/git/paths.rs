//! Shared validation for repository-relative paths supplied by the frontend.
//!
//! Every command that accepts a caller-controlled path funnels through
//! [`validate_repo_relative_path`] before the path reaches git, the filesystem,
//! or a synthesized patch. This is the single choke point that rejects directory
//! traversal (`..`), absolute paths, and control-character injection
//! (newline / NUL) — the building blocks of "make me stage/discard a file
//! outside the repo" attacks.

use std::path::{Component, Path};

use crate::error::{AppError, AppResult};

/// Reject any path that is not a safe repository-relative path.
///
/// The rules (all rejections):
/// - empty path,
/// - any absolute path (leading `/`, or a drive/UNC prefix on Windows),
/// - any `..` (`ParentDir`) component — even mid-path (`a/../../etc`),
/// - any embedded `\n` or `\0` byte.
///
/// `workdir` is accepted for symmetry with the call sites (they always have the
/// repository working directory in hand) and to leave room for future
/// within-tree canonicalization; the current rules are purely lexical, so a
/// malformed path is rejected without touching the filesystem.
pub fn validate_repo_relative_path(_workdir: &Path, path: &str) -> AppResult<()> {
    if path.is_empty() {
        return Err(AppError::validation("Path must not be empty"));
    }
    if path.contains('\n') || path.contains('\0') {
        return Err(AppError::validation(
            "Path must not contain newline or NUL characters",
        ));
    }

    let p = Path::new(path);
    for component in p.components() {
        match component {
            Component::ParentDir => {
                return Err(AppError::validation(format!(
                    "Path must not contain a '..' component: {path}"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::validation(format!(
                    "Path must be relative: {path}"
                )));
            }
            _ => {}
        }
    }

    // Belt-and-suspenders: catch any platform-specific absolute form the
    // component scan above might not have flagged.
    if p.is_absolute() {
        return Err(AppError::validation(format!(
            "Path must be relative: {path}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wd() -> &'static Path {
        Path::new("/repo")
    }

    #[test]
    fn rejects_empty_path() {
        assert!(validate_repo_relative_path(wd(), "").is_err());
    }

    #[test]
    fn rejects_absolute_path() {
        assert!(validate_repo_relative_path(wd(), "/etc/passwd").is_err());
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(validate_repo_relative_path(wd(), "../secret").is_err());
        assert!(validate_repo_relative_path(wd(), "a/../../b").is_err());
        assert!(validate_repo_relative_path(wd(), "sub/..").is_err());
    }

    #[test]
    fn rejects_embedded_newline_and_nul() {
        assert!(validate_repo_relative_path(wd(), "a\nb").is_err());
        assert!(validate_repo_relative_path(wd(), "a\0b").is_err());
        assert!(validate_repo_relative_path(wd(), "trailing\n").is_err());
    }

    #[test]
    fn accepts_ordinary_relative_paths() {
        assert!(validate_repo_relative_path(wd(), "src/main.rs").is_ok());
        assert!(validate_repo_relative_path(wd(), "a.txt").is_ok());
        // Glob metacharacters are legal in a filename — they are literal here.
        assert!(validate_repo_relative_path(wd(), "data[1].txt").is_ok());
        // Spaces and unicode are legal.
        assert!(validate_repo_relative_path(wd(), "my file é.txt").is_ok());
        // A single leading `./` component is harmless.
        assert!(validate_repo_relative_path(wd(), "./a.txt").is_ok());
    }
}
