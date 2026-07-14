//! Shared test fixtures for the git-layer unit tests and the `hunk_staging`
//! integration test.
//!
//! # Why a file module, not a feature-gated `pub mod`
//!
//! These helpers depend on `tempfile`, which stays a **dev-dependency** (the
//! project mandate: never pull a test-only crate into a normal/release build).
//! Dev-dependencies are visible to `#[cfg(test)]` unit tests AND to integration
//! test crates, but NOT to the library when it is compiled as a dependency of an
//! integration crate. A `#[cfg(feature = "test-support")] pub mod` would therefore
//! force `tempfile` to become an optional *normal* dependency — exactly what we
//! must avoid.
//!
//! So instead:
//! - `lib.rs` declares `#[cfg(test)] mod test_support;` — unit tests reach it via
//!   `crate::test_support::*`.
//! - `tests/hunk_staging.rs` pulls the SAME source in with
//!   `#[path = "../src/test_support.rs"] mod test_support;`, compiling it into the
//!   integration crate (where the dev-dependency `tempfile` IS available).
//!
//! One source of truth, zero release-build cost, `tempfile` stays a dev-dep.
//!
//! Not every consumer uses every helper (e.g. only the integration test shells out
//! via [`git_cli`] or seeds raw bytes via [`commit_bytes`]), so the module allows
//! dead code rather than sprinkling per-fn attributes across the two build views.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use git2::{Repository, RepositoryInitOptions, Signature};
use tempfile::{tempdir, TempDir};

/// Initialise a repo at `dir` on branch `main` with a per-repo test identity.
///
/// The identity is written to the **repo-local** config so commits succeed
/// regardless of the host's global git identity.
pub fn init_repo(dir: &Path) -> Repository {
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

/// Commit `content` (raw bytes, no autocrlf munging) to `name`, chaining onto
/// HEAD when it exists.
pub fn commit_bytes(repo: &Repository, dir: &Path, name: &str, content: &[u8]) {
    std::fs::write(dir.join(name), content).unwrap();
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

/// Commit UTF-8 `content` to `name` (thin wrapper over [`commit_bytes`]).
pub fn commit_file(repo: &Repository, dir: &Path, name: &str, content: &str) {
    commit_bytes(repo, dir, name, content.as_bytes());
}

/// A fresh temp repo: `(dir, repo)`. Keep `dir` alive for the test's duration —
/// dropping it deletes the working tree.
pub fn setup() -> (TempDir, Repository) {
    let dir = tempdir().unwrap();
    let repo = init_repo(dir.path());
    (dir, repo)
}

/// Run the `git` CLI in `dir`, returning `(stdout, stderr, success)`.
///
/// The child is isolated from the host's git configuration
/// (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`) so settings like
/// `core.autocrlf` or a global identity cannot perturb assertions. Per-repo
/// config (identity written by [`init_repo`]) still applies. This isolation is
/// per-child-process — not the process-global git2 env — so it is safe under the
/// parallel test runner.
pub fn git_cli(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git CLI runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}
