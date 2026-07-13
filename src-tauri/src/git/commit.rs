//! Create commits (and amend the tip) from the current index.
//!
//! GitX commits whatever is staged in the index: we write the index tree and
//! attach it to HEAD (or amend HEAD in place). The commit author/committer is
//! resolved from the repository's configuration, and a missing identity is
//! turned into an actionable "set your git user.name/email" message rather than
//! libgit2's terse "config value 'user.name' was not found".

use git2::{Commit, DiffOptions, ErrorCode, Repository, Signature};

use crate::error::{AppError, AppResult};

/// Create a commit from the current index, returning the new commit's hex oid.
///
/// - Non-amend: writes the index tree and commits it on top of HEAD (or as the
///   root commit on an unborn HEAD). Rejected when nothing is staged, matching
///   GitX ("No staged changes to commit").
/// - Amend: rewrites the HEAD commit's message and swaps in the current index
///   tree, keeping the original parents. Allowed with an empty staged set so the
///   user can edit just the message. Errors clearly on an unborn HEAD.
///
/// An empty or whitespace-only `message` is always rejected.
pub fn commit(repo: &Repository, message: &str, amend: bool) -> AppResult<String> {
    if message.trim().is_empty() {
        return Err(AppError::msg("Commit message cannot be empty"));
    }

    // Snapshot the index into a tree object for either path.
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let oid = if amend {
        let head_commit = match repo.head() {
            Ok(head) => head.peel_to_commit()?,
            Err(e) if e.code() == ErrorCode::UnbornBranch => {
                return Err(AppError::msg(
                    "Nothing to amend: this branch has no commits yet",
                ));
            }
            Err(e) => return Err(e.into()),
        };
        head_commit.amend(Some("HEAD"), None, None, None, Some(message), Some(&tree))?
    } else {
        if !has_staged_changes(repo)? {
            return Err(AppError::msg("No staged changes to commit"));
        }
        let sig = signature(repo)?;
        let parent = match repo.head() {
            Ok(head) => Some(head.peel_to_commit()?),
            Err(e) if e.code() == ErrorCode::UnbornBranch => None,
            Err(e) => return Err(e.into()),
        };
        let parents: Vec<&Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?
    };

    Ok(oid.to_string())
}

/// Return HEAD commit's full message, or `""` on an unborn HEAD (empty repo).
pub fn head_commit_message(repo: &Repository) -> AppResult<String> {
    match repo.head() {
        Ok(head) => {
            let commit = head.peel_to_commit()?;
            Ok(commit.message().unwrap_or("").to_string())
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Resolve the commit signature from repo/global config, mapping a missing
/// identity to an actionable message.
fn signature(repo: &Repository) -> AppResult<Signature<'static>> {
    repo.signature().map_err(|e| {
        if e.code() == ErrorCode::NotFound || e.message().contains("was not found") {
            AppError::msg(
                "Git identity is not configured. Set it with:\n  \
                 git config --global user.name \"Your Name\"\n  \
                 git config --global user.email \"you@example.com\"",
            )
        } else {
            AppError::from(e)
        }
    })
}

/// Are there staged changes (HEAD-tree vs index differs)? On an unborn HEAD the
/// old tree is `None`, so every index entry counts as an addition.
fn has_staged_changes(repo: &Repository) -> AppResult<bool> {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = DiffOptions::new();
    opts.include_typechange(true);
    let diff = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?;
    Ok(diff.deltas().len() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::RepositoryInitOptions;
    use std::fs;
    use std::path::Path;
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

    fn setup() -> (TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = init_repo(dir.path());
        (dir, repo)
    }

    /// Stage `name` with `content` into the index.
    fn stage(repo: &Repository, dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
    }

    fn head_commit(repo: &Repository) -> Commit<'_> {
        repo.head().unwrap().peel_to_commit().unwrap()
    }

    fn index_is_clean(repo: &Repository) -> bool {
        !has_staged_changes(repo).unwrap()
    }

    #[test]
    fn commits_staged_changes_and_leaves_index_clean() {
        let (dir, repo) = setup();
        // Root commit so HEAD exists, then stage a second file.
        stage(&repo, dir.path(), "a.txt", "a\n");
        commit(&repo, "root", false).unwrap();
        stage(&repo, dir.path(), "b.txt", "b\n");

        let oid = commit(&repo, "add b", false).unwrap();

        let head = head_commit(&repo);
        assert_eq!(head.id().to_string(), oid);
        assert_eq!(head.message(), Some("add b"));
        assert_eq!(head.parent_count(), 1);
        // Nothing left staged relative to the new HEAD.
        assert!(index_is_clean(&repo));
    }

    #[test]
    fn first_commit_on_unborn_head_works() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");

        let oid = commit(&repo, "initial", false).unwrap();

        let head = head_commit(&repo);
        assert_eq!(head.id().to_string(), oid);
        assert_eq!(head.parent_count(), 0);
        assert_eq!(head.message(), Some("initial"));
        assert!(index_is_clean(&repo));
    }

    #[test]
    fn amend_rewrites_message_and_includes_newly_staged_file() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");
        commit(&repo, "first", false).unwrap();
        let before = head_commit(&repo);
        let parents_before = before.parent_count();

        // Stage a new file, then amend with a new message.
        stage(&repo, dir.path(), "b.txt", "b\n");
        let oid = commit(&repo, "first (amended)", true).unwrap();

        let head = head_commit(&repo);
        assert_eq!(head.id().to_string(), oid);
        assert_eq!(head.message(), Some("first (amended)"));
        // Parent count unchanged (root commit still has zero parents).
        assert_eq!(head.parent_count(), parents_before);
        // The newly staged file is part of the amended tree.
        let tree = head.tree().unwrap();
        assert!(tree.get_name("b.txt").is_some());
        assert!(index_is_clean(&repo));
    }

    #[test]
    fn amend_allows_message_only_edit_with_nothing_new_staged() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");
        commit(&repo, "typo", false).unwrap();

        // No new staging — amend just the message.
        let oid = commit(&repo, "fixed message", true).unwrap();

        assert_eq!(head_commit(&repo).id().to_string(), oid);
        assert_eq!(head_commit(&repo).message(), Some("fixed message"));
    }

    #[test]
    fn empty_message_is_rejected() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");

        let err = commit(&repo, "   \n\t", false).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("empty"));
    }

    #[test]
    fn nothing_staged_is_rejected_for_non_amend() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");
        commit(&repo, "root", false).unwrap();

        // Index now matches HEAD — nothing staged.
        let err = commit(&repo, "empty", false).unwrap_err();
        assert!(err.to_string().contains("No staged changes"));
    }

    #[test]
    fn amend_on_unborn_head_errors_clearly() {
        let (_dir, repo) = setup();

        let err = commit(&repo, "cannot", true).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("amend"));
    }

    #[test]
    fn head_commit_message_returns_full_message() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");
        commit(&repo, "subject line\n\nbody paragraph\n", false).unwrap();

        assert_eq!(
            head_commit_message(&repo).unwrap(),
            "subject line\n\nbody paragraph\n"
        );
    }

    #[test]
    fn head_commit_message_is_empty_on_unborn_head() {
        let (_dir, repo) = setup();
        assert_eq!(head_commit_message(&repo).unwrap(), "");
    }

    #[test]
    fn missing_identity_yields_actionable_message() {
        let dir = tempdir().unwrap();
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &opts).unwrap();
        // Deliberately no user.name/email configured. Guard against a global
        // identity leaking in from the test host by only asserting when the
        // signature really is unavailable.
        if repo.signature().is_ok() {
            return;
        }
        stage(&repo, dir.path(), "a.txt", "a\n");

        let err = commit(&repo, "hello", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("user.name"));
        assert!(msg.contains("user.email"));
    }
}
