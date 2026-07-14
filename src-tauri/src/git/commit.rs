//! Create commits (and amend the tip) from the current index.
//!
//! `GitX` commits whatever is staged in the index: we write the index tree and
//! attach it to HEAD (or amend HEAD in place). The commit author/committer is
//! resolved from the repository's configuration, and a missing identity is
//! turned into an actionable "set your git user.name/email" message rather than
//! libgit2's terse "config value 'user.name' was not found".

use git2::{Commit, DiffOptions, ErrorClass, ErrorCode, Repository, Signature};

use crate::error::{AppError, AppResult};

/// Create a commit from the current index, returning the new commit's hex oid.
///
/// - Non-amend: writes the index tree and commits it on top of HEAD (or as the
///   root commit on an unborn HEAD). Rejected when nothing is staged, matching
///   `GitX` ("No staged changes to commit").
/// - Amend: rewrites the HEAD commit's message and swaps in the current index
///   tree, keeping the original parents. Allowed with an empty staged set so the
///   user can edit just the message. Errors clearly on an unborn HEAD.
///
/// An empty or whitespace-only `message` is always rejected.
pub fn commit(repo: &Repository, message: &str, amend: bool) -> AppResult<String> {
    if message.trim().is_empty() {
        return Err(AppError::empty_message());
    }

    // Snapshot the index into a tree object for either path.
    //
    // Known limitation: `write_tree` serializes the ENTIRE index, including any
    // submodule (gitlink) entries. `build_status` and `has_staged_changes` hide
    // submodule deltas from the UI, so a staged submodule bump is invisible — but
    // if the user commits some other change, that hidden bump rides along in this
    // tree. Excluding it would require rewriting the tree entry-by-entry, out of
    // scope here; documented so the coupling is explicit.
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let oid = if amend {
        let head_commit = match repo.head() {
            Ok(head) => head.peel_to_commit()?,
            Err(e) if e.code() == ErrorCode::UnbornBranch => {
                return Err(AppError::validation(
                    "Nothing to amend: this branch has no commits yet",
                ));
            }
            Err(e) => return Err(e.into()),
        };
        // Pass a fresh committer signature (with the current timestamp) rather
        // than `None`, which would reuse the original commit's committer — so an
        // amend is correctly recorded as a new committer action. The author
        // (2nd arg) stays `None` to preserve the original authorship.
        let committer = signature(repo)?;
        head_commit.amend(
            Some("HEAD"),
            None,
            Some(&committer),
            None,
            Some(message),
            Some(&tree),
        )?
    } else {
        if !has_staged_changes(repo)? {
            return Err(AppError::nothing_staged());
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
        // A missing user.name/user.email surfaces from libgit2's
        // `git_signature_default` as a NotFound code in the Config class. Inspect
        // that structured error code/class rather than sniffing the message text
        // (the old `contains("was not found")` heuristic) — the code is the
        // contract, the English message is not.
        if e.code() == ErrorCode::NotFound && e.class() == ErrorClass::Config {
            AppError::identity_missing()
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
    // M2: ignore submodule (gitlink) deltas so this stays consistent with
    // `build_status`, which excludes submodules from the UI. A staged submodule
    // bump alone must NOT enable the commit button when the user sees nothing
    // stageable. `DiffOptions` has no submodule exclusion, so filter by mode.
    Ok(diff.deltas().any(|d| !is_submodule_delta(&d)))
}

/// True when either side of `delta` is a submodule (gitlink) entry.
fn is_submodule_delta(delta: &git2::DiffDelta) -> bool {
    delta.new_file().mode() == git2::FileMode::Commit
        || delta.old_file().mode() == git2::FileMode::Commit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::setup;
    use git2::RepositoryInitOptions;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

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
        assert!(matches!(err, AppError::EmptyMessage { .. }), "{err:?}");
        assert!(err.to_string().to_lowercase().contains("empty"));
    }

    #[test]
    fn nothing_staged_is_rejected_for_non_amend() {
        let (dir, repo) = setup();
        stage(&repo, dir.path(), "a.txt", "a\n");
        commit(&repo, "root", false).unwrap();

        // Index now matches HEAD — nothing staged.
        let err = commit(&repo, "empty", false).unwrap_err();
        assert!(matches!(err, AppError::NothingStaged { .. }), "{err:?}");
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

    // Ignored by default: it asserts the "set your git identity" error, which only
    // fires when `repo.signature()` finds NO identity. A repo-local config cannot
    // hide a global `user.name`/`user.email`, and isolating git2 from the global
    // config is a process-global, parallel-racy operation (`git2::opts::
    // set_search_path`), so it must not run inside the shared suite. Rather than
    // silently self-skip on any host that has a global identity (i.e. always, in
    // practice), it is explicit: run it with git2's global config search path
    // redirected to an empty dir. libgit2 discovers the global config via HOME/XDG
    // (it does NOT honour git's own `GIT_CONFIG_GLOBAL`), so isolate those while
    // preserving CARGO_HOME/RUSTUP_HOME:
    //   EMPTY=$(mktemp -d); CARGO_HOME="$HOME/.cargo" RUSTUP_HOME="$HOME/.rustup" \
    //     HOME="$EMPTY" XDG_CONFIG_HOME="$EMPTY" GIT_CONFIG_NOSYSTEM=1 \
    //     cargo test -p rust-gitx missing_identity_yields_actionable_message -- --ignored
    #[test]
    #[ignore = "requires an env with no global git identity; see the comment above"]
    fn missing_identity_yields_actionable_message() {
        let dir = tempdir().unwrap();
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &opts).unwrap();
        // Precondition for the assertion: no identity is resolvable. If the host
        // still exposes one (the env vars above were not set), fail loudly rather
        // than pass vacuously — the whole point of --ignored is a real run.
        assert!(
            repo.signature().is_err(),
            "run with GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 so no identity leaks in"
        );
        stage(&repo, dir.path(), "a.txt", "a\n");

        let err = commit(&repo, "hello", false).unwrap_err();
        assert!(matches!(err, AppError::IdentityMissing { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("user.name"));
        assert!(msg.contains("user.email"));
    }
}
