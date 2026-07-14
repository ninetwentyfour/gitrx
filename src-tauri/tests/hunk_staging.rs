//! End-to-end oracle suite for the single-hunk staging engine.
//!
//! The engine now lives in the library crate, so this integration test imports
//! the real modules from `rust_gitx_lib::git` rather than re-including their
//! source files. It exercises the full production pipeline, pulling every payload
//! byte-for-byte from the real diff layer before handing it to `build_patch`.
//!
//! Pipeline under test: mutate workdir -> `diff::file_diff` -> map hunk to
//! `HunkPatchPayload` -> `patch::build_patch` -> `apply::apply_patch` -> assert
//! index / working-tree state via git2 and the `git` CLI.

use std::path::Path;

use git2::Repository;

use rust_gitx_lib::error::AppError;
use rust_gitx_lib::git::{apply, diff, patch, stage};

use apply::{apply_patch, ApplyTarget};
use patch::{build_patch, HunkPatchPayload, PatchLine, PatchLineKind};

// The shared git-layer fixtures. The library gates this module behind
// `#[cfg(test)]` (invisible to an integration crate), and its `tempfile`
// dependency stays dev-only, so we compile the SAME source directly into this
// crate rather than importing it from `rust_gitx_lib`. See `src/test_support.rs`.
#[path = "../src/test_support.rs"]
mod test_support;

use test_support::{commit_bytes, commit_file, git_cli, setup};

// ---------------------------------------------------------------------------
// Payload helpers
// ---------------------------------------------------------------------------

const fn map_kind(k: diff::DiffLineKind) -> PatchLineKind {
    match k {
        diff::DiffLineKind::Context => PatchLineKind::Context,
        diff::DiffLineKind::Add => PatchLineKind::Add,
        diff::DiffLineKind::Del => PatchLineKind::Del,
        diff::DiffLineKind::NoNewline => PatchLineKind::NoNewline,
    }
}

/// Map a single hunk of a `FileDiff` to a payload EXACTLY the way the UI layer
/// will: copy header + line kind/content straight across.
///
/// The tests using this helper drive `build_patch`/`apply_patch` directly, which
/// ignore `context_lines`; a fixed placeholder is fine. Tests that exercise the
/// freshness-verified pipeline use [`payload_from_hunk_ctx`] to pin the real
/// slider value so the re-diff reproduces the same hunks.
fn payload_from_hunk(fd: &diff::FileDiff, hunk: usize, staged: bool) -> HunkPatchPayload {
    payload_from_hunk_ctx(fd, hunk, staged, 3)
}

/// Like [`payload_from_hunk`] but with an explicit `context_lines`, matching the
/// slider value the diff was computed with.
fn payload_from_hunk_ctx(
    fd: &diff::FileDiff,
    hunk: usize,
    staged: bool,
    context_lines: u32,
) -> HunkPatchPayload {
    let h = &fd.hunks[hunk];
    HunkPatchPayload {
        path: fd.path.clone(),
        old_path: fd.old_path.clone(),
        staged,
        is_untracked: fd.is_untracked,
        context_lines,
        header: h.header.clone(),
        lines: h
            .lines
            .iter()
            .map(|l| PatchLine {
                kind: map_kind(l.kind),
                content: l.content.clone(),
            })
            .collect(),
    }
}

/// Raw bytes of the staged (index) blob for `name`, or `None` if absent.
fn index_blob(repo: &Repository, name: &str) -> Option<Vec<u8>> {
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let entry = index.get_path(Path::new(name), 0)?;
    let blob = repo.find_blob(entry.id).ok()?;
    Some(blob.content().to_vec())
}

// ---------------------------------------------------------------------------
// 1. THE core test: stage exactly one of three hunks.
// ---------------------------------------------------------------------------

/// Baseline of `n` numbered lines, LF terminated.
fn numbered(n: usize) -> String {
    (1..=n).fold(String::new(), |mut s, i| {
        use std::fmt::Write as _;
        let _ = writeln!(s, "line{i}");
        s
    })
}

/// `numbered(n)` with the given 1-based lines replaced by `line{i}-changed`.
fn numbered_edited(n: usize, changed: &[usize]) -> String {
    (1..=n)
        .map(|i| {
            if changed.contains(&i) {
                format!("line{i}-changed\n")
            } else {
                format!("line{i}\n")
            }
        })
        .collect()
}

#[test]
fn stages_only_the_selected_middle_hunk() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(30));
    // Three far-apart edits -> three separate hunks at context 3.
    std::fs::write(dir.path().join("f.txt"), numbered_edited(30, &[3, 15, 27])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    assert_eq!(fd.hunks.len(), 3, "expected three separate hunks");

    let payload = payload_from_hunk(&fd, 1, false);
    let bytes = build_patch(&payload).unwrap();
    apply_patch(dir.path(), &bytes, ApplyTarget::Index).unwrap();

    // Only hunk 2 (line15) is staged.
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(cached.contains("+line15-changed"), "staged: {cached}");
    assert!(cached.contains("-line15"), "staged: {cached}");
    assert!(!cached.contains("line3-changed"), "hunk1 leaked: {cached}");
    assert!(!cached.contains("line27-changed"), "hunk3 leaked: {cached}");

    // Hunks 1 and 3 remain unstaged in the working tree.
    let (unstaged, _, _) = git_cli(dir.path(), &["diff", "--", "f.txt"]);
    assert!(unstaged.contains("+line3-changed"), "unstaged: {unstaged}");
    assert!(unstaged.contains("+line27-changed"), "unstaged: {unstaged}");
    assert!(
        !unstaged.contains("line15-changed"),
        "hunk2 still unstaged: {unstaged}"
    );
}

// ---------------------------------------------------------------------------
// 2. Round-trip: stage then unstage the same hunk.
// ---------------------------------------------------------------------------

#[test]
fn stage_then_unstage_leaves_index_clean() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(30));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(30, &[3, 15, 27])).unwrap();

    // Stage hunk 2 from the unstaged diff.
    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let stage_payload = payload_from_hunk(&fd, 1, false);
    apply_patch(
        dir.path(),
        &build_patch(&stage_payload).unwrap(),
        ApplyTarget::Index,
    )
    .unwrap();

    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(
        cached.contains("line15-changed"),
        "precondition: hunk staged"
    );

    // Unstage it, regenerating the payload from the STAGED diff.
    let staged_fd = diff::file_diff(&repo, "f.txt", true, 3).unwrap();
    assert_eq!(staged_fd.hunks.len(), 1, "one staged hunk");
    let unstage_payload = payload_from_hunk(&staged_fd, 0, true);
    apply_patch(
        dir.path(),
        &build_patch(&unstage_payload).unwrap(),
        ApplyTarget::IndexReverse,
    )
    .unwrap();

    let (cached_after, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(
        cached_after.trim().is_empty(),
        "index should be clean: {cached_after}"
    );
}

// ---------------------------------------------------------------------------
// 3. Discard a single hunk from the working tree.
// ---------------------------------------------------------------------------

#[test]
fn discard_reverts_only_the_selected_region() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(30));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(30, &[3, 15, 27])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let payload = payload_from_hunk(&fd, 0, false); // hunk 1 (line3)
    apply_patch(
        dir.path(),
        &build_patch(&payload).unwrap(),
        ApplyTarget::WorkdirReverse,
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert!(content.contains("line3\n"), "line3 reverted: {content}");
    assert!(
        !content.contains("line3-changed"),
        "line3 still changed: {content}"
    );
    // Other regions untouched.
    assert!(content.contains("line15-changed"), "line15 preserved");
    assert!(content.contains("line27-changed"), "line27 preserved");
}

// ---------------------------------------------------------------------------
// 4. First-line and last-line hunks at context 0 and 3.
// ---------------------------------------------------------------------------

#[test]
fn first_and_last_line_hunks_apply_at_ctx_0_and_3() {
    for ctx in [0u32, 3u32] {
        // First line edited.
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "f.txt", &numbered(10));
        std::fs::write(dir.path().join("f.txt"), numbered_edited(10, &[1])).unwrap();
        let fd = diff::file_diff(&repo, "f.txt", false, ctx).unwrap();
        let p = payload_from_hunk(&fd, 0, false);
        apply_patch(dir.path(), &build_patch(&p).unwrap(), ApplyTarget::Index)
            .unwrap_or_else(|e| panic!("first-line ctx={ctx} failed: {e}"));
        let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
        assert!(cached.contains("+line1-changed"), "ctx={ctx}: {cached}");

        // Last line edited (fresh repo).
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "g.txt", &numbered(10));
        std::fs::write(dir.path().join("g.txt"), numbered_edited(10, &[10])).unwrap();
        let fd = diff::file_diff(&repo, "g.txt", false, ctx).unwrap();
        let p = payload_from_hunk(&fd, 0, false);
        apply_patch(dir.path(), &build_patch(&p).unwrap(), ApplyTarget::Index)
            .unwrap_or_else(|e| panic!("last-line ctx={ctx} failed: {e}"));
        let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "g.txt"]);
        assert!(cached.contains("+line10-changed"), "ctx={ctx}: {cached}");
    }
}

// ---------------------------------------------------------------------------
// 5. Pure-addition and pure-deletion hunks (context 0 -> a ,0 side).
// ---------------------------------------------------------------------------

#[test]
fn pure_addition_hunk_stages_cleanly() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", "a\nb\nc\n");
    // Insert two lines between b and c -> pure addition.
    std::fs::write(dir.path().join("f.txt"), "a\nb\nNEW1\nNEW2\nc\n").unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 0).unwrap();
    let p = payload_from_hunk(&fd, 0, false);
    // Sanity: the recomputed header carries a ,0 old side.
    let text = String::from_utf8(build_patch(&p).unwrap()).unwrap();
    assert!(
        text.contains(",0 +"),
        "expected zero old-count header: {text}"
    );

    apply_patch(dir.path(), text.as_bytes(), ApplyTarget::Index).unwrap();
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(cached.contains("+NEW1"));
    assert!(cached.contains("+NEW2"));
}

#[test]
fn pure_deletion_hunk_stages_cleanly() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", "a\nb\nc\nd\ne\n");
    // Delete c and d -> pure deletion.
    std::fs::write(dir.path().join("f.txt"), "a\nb\ne\n").unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 0).unwrap();
    let p = payload_from_hunk(&fd, 0, false);
    let text = String::from_utf8(build_patch(&p).unwrap()).unwrap();
    assert!(
        text.contains(",0 @@"),
        "expected zero new-count header: {text}"
    );

    apply_patch(dir.path(), text.as_bytes(), ApplyTarget::Index).unwrap();
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(cached.contains("-c"));
    assert!(cached.contains("-d"));
}

// ---------------------------------------------------------------------------
// 6. Missing trailing newline, both directions.
// ---------------------------------------------------------------------------

#[test]
fn gaining_no_trailing_newline_round_trips() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "n.txt", "x\ny\n");
    // Edit last line AND drop the final newline.
    std::fs::write(dir.path().join("n.txt"), "x\nyEDIT").unwrap();

    let fd = diff::file_diff(&repo, "n.txt", false, 3).unwrap();
    assert!(
        fd.hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == diff::DiffLineKind::NoNewline),
        "diff should carry a no-newline marker"
    );
    let p = payload_from_hunk(&fd, 0, false);
    apply_patch(dir.path(), &build_patch(&p).unwrap(), ApplyTarget::Index).unwrap();

    // Staged blob has no trailing newline, matching the working tree bytes.
    assert_eq!(index_blob(&repo, "n.txt").unwrap(), b"x\nyEDIT");
}

#[test]
fn editing_a_file_that_had_no_trailing_newline_round_trips() {
    let (dir, repo) = setup();
    // Baseline itself lacks a trailing newline.
    commit_bytes(&repo, dir.path(), "n.txt", b"x\ny");
    // Edit the last line, still no trailing newline.
    std::fs::write(dir.path().join("n.txt"), "x\nyEDIT").unwrap();

    let fd = diff::file_diff(&repo, "n.txt", false, 3).unwrap();
    let p = payload_from_hunk(&fd, 0, false);
    apply_patch(dir.path(), &build_patch(&p).unwrap(), ApplyTarget::Index).unwrap();

    assert_eq!(index_blob(&repo, "n.txt").unwrap(), b"x\nyEDIT");
}

// ---------------------------------------------------------------------------
// 7. CRLF. diff::file_diff preserves the trailing `\r` in line content (only
//    the `\n` is stripped), so a payload taken verbatim from it round-trips
//    CRLF files byte-exactly through build_patch + git apply.
// ---------------------------------------------------------------------------

#[test]
fn crlf_verbatim_payload_from_diff_layer_stages_cleanly() {
    // The production path: payload built directly from file_diff output, no
    // CR restoration. Guards against the diff layer ever stripping \r again.
    let (dir, repo) = setup();
    commit_bytes(&repo, dir.path(), "c.txt", b"a\r\nb\r\nc\r\nd\r\ne\r\n");
    let workdir_bytes = b"a\r\nb\r\nCHANGED\r\nd\r\ne\r\n";
    std::fs::write(dir.path().join("c.txt"), workdir_bytes).unwrap();

    let fd = diff::file_diff(&repo, "c.txt", false, 3).unwrap();
    assert!(
        fd.hunks[0].lines.iter().all(|l| l.content.ends_with('\r')),
        "diff layer must preserve trailing \\r in CRLF line content"
    );

    let payload = payload_from_hunk(&fd, 0, false);
    apply_patch(
        dir.path(),
        &build_patch(&payload).unwrap(),
        ApplyTarget::Index,
    )
    .unwrap();
    assert_eq!(
        index_blob(&repo, "c.txt").unwrap(),
        workdir_bytes.to_vec(),
        "staged CRLF bytes must match the working tree exactly"
    );
}

#[test]
fn crlf_round_trips_byte_exact_when_cr_preserved() {
    let (dir, repo) = setup();
    commit_bytes(&repo, dir.path(), "c.txt", b"a\r\nb\r\nc\r\nd\r\ne\r\n");
    let workdir_bytes = b"a\r\nb\r\nCHANGED\r\nd\r\ne\r\n";
    std::fs::write(dir.path().join("c.txt"), workdir_bytes).unwrap();

    let fd = diff::file_diff(&repo, "c.txt", false, 3).unwrap();
    let payload = payload_from_hunk(&fd, 0, false);

    // The rebuilt patch must contain CR-terminated body lines.
    let bytes = build_patch(&payload).unwrap();
    assert!(
        bytes.windows(10).any(|w| w == b"+CHANGED\r\n"),
        "patch lost CR: {:?}",
        String::from_utf8_lossy(&bytes)
    );

    apply_patch(dir.path(), &bytes, ApplyTarget::Index).unwrap();
    assert_eq!(
        index_blob(&repo, "c.txt").unwrap(),
        workdir_bytes.to_vec(),
        "staged CRLF bytes must match the working tree exactly"
    );
}

// ---------------------------------------------------------------------------
// 8. Untracked files stage WHOLE-FILE (not via a synthesized /dev/null patch).
//
// These two tests were repurposed: untracked files used to be turned into a
// hand-rolled `new file mode 100644` patch, which corrupted exec bits and
// symlinks. They now go through `stage::stage_file` (index.add_path), and both
// `build_patch` and `apply_hunk_verified` REFUSE untracked payloads. We keep the
// original content oracles (trailing-newline and no-trailing-newline) but assert
// against the correct code path, and add the rejection assertions.
// ---------------------------------------------------------------------------

#[test]
fn untracked_file_stages_full_content_via_stage_file() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "seed.txt", "seed\n");
    std::fs::write(dir.path().join("u.txt"), "u1\nu2\nu3\n").unwrap();

    let fd = diff::file_diff(&repo, "u.txt", false, 3).unwrap();
    assert!(fd.is_untracked, "file is untracked");

    // The synthesized-patch path must refuse an untracked payload outright.
    let p = payload_from_hunk(&fd, 0, false);
    assert!(
        build_patch(&p).is_err(),
        "untracked payload must be rejected"
    );
    assert!(
        stage::apply_hunk_verified(dir.path(), &p, ApplyTarget::Index).is_err(),
        "verified pipeline must refuse untracked files"
    );

    // The correct path: whole-file staging preserves the exact bytes.
    stage::stage_file(&repo, "u.txt").unwrap();
    assert_eq!(index_blob(&repo, "u.txt").unwrap(), b"u1\nu2\nu3\n");
}

#[test]
fn untracked_file_without_trailing_newline_stages_full_content_via_stage_file() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "seed.txt", "seed\n");
    std::fs::write(dir.path().join("u.txt"), "u1\nu2\nu3").unwrap(); // no final newline

    let fd = diff::file_diff(&repo, "u.txt", false, 3).unwrap();
    assert!(fd.is_untracked);
    assert!(
        fd.hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == diff::DiffLineKind::NoNewline),
        "untracked no-newline file should carry the marker"
    );

    stage::stage_file(&repo, "u.txt").unwrap();
    assert_eq!(index_blob(&repo, "u.txt").unwrap(), b"u1\nu2\nu3");
}

// ---------------------------------------------------------------------------
// 9. Context-slider parity: a hunk taken from a context-0 diff still applies.
// ---------------------------------------------------------------------------

#[test]
fn context_zero_hunk_applies_like_context_eight() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(20));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(20, &[4, 16])).unwrap();

    // ctx 0 -> two tight hunks; ctx 8 -> regions merge into one.
    let fd0 = diff::file_diff(&repo, "f.txt", false, 0).unwrap();
    let fd8 = diff::file_diff(&repo, "f.txt", false, 8).unwrap();
    assert_eq!(fd0.hunks.len(), 2, "ctx0 keeps regions separate");
    assert_eq!(fd8.hunks.len(), 1, "ctx8 merges regions");

    // Stage the first small ctx-0 hunk.
    let p = payload_from_hunk(&fd0, 0, false);
    apply_patch(dir.path(), &build_patch(&p).unwrap(), ApplyTarget::Index).unwrap();
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(
        cached.contains("+line4-changed"),
        "ctx0 hunk staged: {cached}"
    );
    assert!(
        !cached.contains("line16-changed"),
        "second region leaked: {cached}"
    );
}

// ---------------------------------------------------------------------------
// 10. Byte-oracle: build_patch hunk output equals git's own hunk slice.
// ---------------------------------------------------------------------------

/// Truncate an `@@` header line just after its closing `@@`, dropping the
/// optional function-context section (which `build_patch` omits).
fn normalize_at_header(h: &str) -> String {
    if let Some(first) = h.find("@@") {
        if let Some(rel) = h[first + 2..].find("@@") {
            let end = first + 2 + rel + 2;
            return h[..end].to_string();
        }
    }
    h.to_string()
}

/// Normalize an entire hunk block: header truncated, body kept as-is.
fn normalize_block(block: &str) -> String {
    let mut lines = block.lines();
    let header = normalize_at_header(lines.next().unwrap_or(""));
    let mut out = header;
    for l in lines {
        out.push('\n');
        out.push_str(l);
    }
    out
}

/// Extract the `@@` blocks (hunk bodies) from a full `git diff` output.
fn hunk_blocks(diff_text: &str) -> Vec<String> {
    let lines: Vec<&str> = diff_text.lines().collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("@@ "))
        .map(|(i, _)| i)
        .collect();
    let mut blocks = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(lines.len());
        blocks.push(lines[s..end].join("\n"));
    }
    blocks
}

#[test]
fn build_patch_matches_git_hunk_slice_byte_for_byte() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(30));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(30, &[3, 15, 27])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let (git_diff, _, _) = git_cli(dir.path(), &["diff", "--", "f.txt"]);
    let git_hunks = hunk_blocks(&git_diff);
    assert_eq!(git_hunks.len(), fd.hunks.len(), "hunk counts agree");

    for (i, git_hunk) in git_hunks.iter().enumerate() {
        let p = payload_from_hunk(&fd, i, false);
        let mine = String::from_utf8(build_patch(&p).unwrap()).unwrap();
        // Isolate the single @@ block from my patch (everything from `@@ ` on).
        let mine_block = hunk_blocks(&mine).pop().expect("my patch has a hunk");
        assert_eq!(
            normalize_block(&mine_block),
            normalize_block(git_hunk),
            "hunk {i} must match git's own slice"
        );
    }
}

// ---------------------------------------------------------------------------
// 11. Failure surfaces git's stderr.
// ---------------------------------------------------------------------------

#[test]
fn corrupt_payload_returns_git_error() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(10));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(10, &[5])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let mut p = payload_from_hunk(&fd, 0, false);
    // Corrupt a context line so it no longer matches the blob.
    for line in &mut p.lines {
        if line.kind == PatchLineKind::Context {
            line.content = "totally-wrong-context".to_string();
            break;
        }
    }

    let err = apply_patch(dir.path(), &build_patch(&p).unwrap(), ApplyTarget::Index)
        .expect_err("corrupt patch must fail");
    assert!(matches!(err, AppError::Git { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains("apply"),
        "message should carry git apply stderr: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 12. Server-side freshness verification (apply_hunk_verified).
// ---------------------------------------------------------------------------

/// A happy-path round-trip through the verified pipeline, to anchor the negative
/// tests below.
#[test]
fn verified_apply_stages_the_selected_hunk() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(30));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(30, &[15])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let payload = payload_from_hunk_ctx(&fd, 0, false, 3);

    stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index).unwrap();
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(cached.contains("+line15-changed"), "staged: {cached}");
}

/// The nastiest case: a `context_lines=0` PURE-INSERTION hunk, then the file shifts
/// (a line is prepended) so the captured hunk's header no longer lines up. The
/// verified pipeline must reject it and leave the index completely untouched.
#[test]
fn stale_zero_context_insertion_is_rejected_and_index_untouched() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(20));

    // Pure insertion after line 10 (no context at ctx 0).
    let mut inserted: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
    inserted.insert(10, "INSERTED".to_string());
    std::fs::write(dir.path().join("f.txt"), inserted.join("\n") + "\n").unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 0).unwrap();
    let payload = payload_from_hunk_ctx(&fd, 0, false, 0);

    // Now the file shifts under us: prepend a line so every line number moves.
    let mut shifted: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
    shifted.insert(0, "PREPENDED".to_string());
    shifted.insert(11, "INSERTED".to_string());
    std::fs::write(dir.path().join("f.txt"), shifted.join("\n") + "\n").unwrap();

    let err = stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index)
        .expect_err("stale hunk must be rejected");
    assert!(matches!(err, AppError::StaleHunk { .. }), "{err:?}");
    assert!(
        err.to_string().contains("changed since"),
        "expected a freshness error, got: {err}"
    );

    // Index untouched — nothing was staged.
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(
        cached.trim().is_empty(),
        "index must be untouched: {cached}"
    );
}

/// Applying the same hunk twice: the second call's freshness re-diff no longer
/// finds the hunk (it is already staged), so it errors cleanly and the index is
/// byte-identical to after the first, successful apply.
#[test]
fn double_apply_second_call_is_rejected_by_freshness() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(30));
    std::fs::write(dir.path().join("f.txt"), numbered_edited(30, &[15])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let payload = payload_from_hunk_ctx(&fd, 0, false, 3);

    stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index).unwrap();
    let (cached_after_first, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert!(cached_after_first.contains("+line15-changed"));

    let err = stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index)
        .expect_err("second apply must be rejected by the freshness check");
    assert!(matches!(err, AppError::StaleHunk { .. }), "{err:?}");
    assert!(
        err.to_string().contains("changed since"),
        "expected a freshness error, got: {err}"
    );

    let (cached_after_second, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", "f.txt"]);
    assert_eq!(
        cached_after_first, cached_after_second,
        "a rejected second apply must not change the index"
    );
}

/// H2: a forged `old_path` on an otherwise-valid hunk payload — pointing at a
/// clean, tracked victim — is rejected before any patch is built. Without the
/// guard, `git apply` reads `diff --git a/victim b/f` as an implicit rename and
/// stage-deletes the victim. The victim's index entry must be untouched.
#[test]
fn forged_old_path_is_rejected_and_victim_untouched() {
    let (dir, repo) = setup();
    commit_file(&repo, dir.path(), "f.txt", &numbered(10));
    commit_file(&repo, dir.path(), "victim.txt", "keep me\n");
    std::fs::write(dir.path().join("f.txt"), numbered_edited(10, &[5])).unwrap();

    let fd = diff::file_diff(&repo, "f.txt", false, 3).unwrap();
    let mut payload = payload_from_hunk_ctx(&fd, 0, false, 3);
    payload.old_path = Some("victim.txt".to_string());

    let err = stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index)
        .expect_err("forged old_path must be rejected");
    assert!(matches!(err, AppError::StaleHunk { .. }), "{err:?}");
    assert!(
        err.to_string().contains("changed since"),
        "expected a freshness-style rejection, got: {err}"
    );

    // Victim still holds its committed blob (not stage-deleted).
    assert_eq!(index_blob(&repo, "victim.txt").unwrap(), b"keep me\n");
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.trim().is_empty(),
        "index must be untouched: {cached}"
    );
}

/// H3: a hunk from a file containing non-UTF-8 bytes is refused (the
/// reconstructed patch would write the U+FFFD-replaced bytes back and corrupt
/// the file), while whole-file `stage_file` stays byte-exact.
#[test]
fn non_utf8_hunk_apply_is_rejected() {
    let (dir, repo) = setup();
    commit_bytes(&repo, dir.path(), "l.txt", b"cafe\n");
    std::fs::write(dir.path().join("l.txt"), b"caf\xe9\n").unwrap();

    let fd = diff::file_diff(&repo, "l.txt", false, 3).unwrap();
    assert!(fd.is_lossy, "diff must flag non-UTF-8 as lossy");
    let payload = payload_from_hunk_ctx(&fd, 0, false, 3);

    let err = stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index)
        .expect_err("lossy hunk must be rejected");
    assert!(matches!(err, AppError::NonUtf8File { .. }), "{err:?}");
    assert!(
        err.to_string().contains("non-UTF-8"),
        "expected a non-UTF-8 rejection, got: {err}"
    );

    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached"]);
    assert!(
        cached.trim().is_empty(),
        "index must be untouched: {cached}"
    );
}

#[test]
fn non_utf8_file_stages_whole_file_byte_exact() {
    let (dir, repo) = setup();
    commit_bytes(&repo, dir.path(), "l.txt", b"cafe\n");
    let raw: &[u8] = b"caf\xe9\nsecond\xff line\n";
    std::fs::write(dir.path().join("l.txt"), raw).unwrap();

    stage::stage_file(&repo, "l.txt").unwrap();
    assert_eq!(
        index_blob(&repo, "l.txt").unwrap(),
        raw.to_vec(),
        "whole-file staging must preserve the exact bytes"
    );
}

/// A filename with spaces AND a non-ASCII character round-trips through the full
/// verified pipeline.
#[test]
fn spaces_and_unicode_filename_round_trips_via_verified() {
    let (dir, repo) = setup();
    let name = "my file é.txt";
    commit_file(&repo, dir.path(), name, &numbered(10));
    std::fs::write(dir.path().join(name), numbered_edited(10, &[5])).unwrap();

    let fd = diff::file_diff(&repo, name, false, 3).unwrap();
    assert_eq!(fd.path, name);
    let payload = payload_from_hunk_ctx(&fd, 0, false, 3);

    stage::apply_hunk_verified(dir.path(), &payload, ApplyTarget::Index).unwrap();
    let (cached, _, _) = git_cli(dir.path(), &["diff", "--cached", "--", name]);
    assert!(cached.contains("+line5-changed"), "staged: {cached}");
}
