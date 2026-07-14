//! Reconstruct a valid single-hunk unified diff from a frontend payload.
//!
//! The staging engine works by taking exactly one hunk (as the user sees it in
//! the diff view), rebuilding the minimal `git`-compatible patch text for it, and
//! piping that patch to `git apply` (see [`super`]'s `apply` module). This module
//! owns the "payload -> patch bytes" half of that pipeline.
//!
//! ## Byte-preservation contract
//!
//! [`build_patch`] performs **no normalization** of line content. Whatever bytes
//! arrive in [`PatchLine::content`] are emitted verbatim between the line prefix
//! (`' '`/`'+'`/`'-'`) and the trailing `\n`. This is load-bearing for CRLF
//! files: a line whose content ends in `\r` must round-trip through `git apply`
//! so the staged blob matches the working-tree bytes exactly. The producer of the
//! payload (the diff layer) is therefore responsible for retaining any trailing
//! `\r` in `content` — only the patch line's own `\n` terminator is synthesized
//! here.
//!
//! ## Untracked files are not handled here
//!
//! [`build_patch`] **rejects** untracked payloads. A hand-rolled
//! `new file mode 100644` / `--- /dev/null` patch cannot represent an executable
//! bit or a symlink, so staging an untracked file that way silently corrupts its
//! mode. Untracked files are staged whole-file via `index.add_path` (see
//! `git::stage::stage_file`), which preserves the real mode. The `is_untracked`
//! flag therefore survives on the payload only so this layer can refuse it.

use crate::error::{AppError, AppResult};

/// A single hunk, exactly as the frontend selected it, ready to be turned into a
/// patch. Field names / serde casing mirror the JSON the UI sends.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HunkPatchPayload {
    /// New-side path of the file (post-image).
    pub path: String,
    /// Old-side path when it differs (renames); `None` for the common case.
    pub old_path: Option<String>,
    /// Which diff the hunk was taken from. Does not affect the patch text; the
    /// caller uses it to pick the apply direction/target.
    pub staged: bool,
    /// True when the file is untracked. Such payloads are **rejected** by
    /// [`build_patch`] and by the hunk commands — untracked files are staged
    /// whole-file so their mode (exec bit / symlink) is preserved.
    pub is_untracked: bool,
    /// Context-line count the displayed diff was computed with (UI slider, 0-8).
    /// Used by the freshness re-verification to recompute the *same* diff before
    /// applying, so a stale payload cannot be matched against a differently
    /// sliced diff.
    pub context_lines: u32,
    /// Raw `@@ -a,b +c,d @@ ...` header line, without a trailing newline. Only
    /// the start line numbers are read from it; the counts are recomputed.
    pub header: String,
    /// The hunk's lines, in order, including any `NoNewline` marker lines.
    pub lines: Vec<PatchLine>,
}

/// One line of a hunk. `content` carries no trailing newline (the terminator is
/// added by [`build_patch`]); for [`PatchLineKind::NoNewline`] the content is
/// ignored and a fixed marker is emitted instead.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchLine {
    pub kind: PatchLineKind,
    pub content: String,
}

/// The role of a hunk line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchLineKind {
    Context,
    Add,
    Del,
    /// The `\ No newline at end of file` marker; carries no payload of its own.
    NoNewline,
}

/// The exact bytes `git` emits for a missing-final-newline marker line.
const NO_NEWLINE_MARKER: &[u8] = b"\\ No newline at end of file\n";

/// Build a single-hunk unified diff for `payload`.
///
/// The output is a complete, self-contained patch: a `diff --git` header, the
/// `---`/`+++` file lines, one recomputed `@@ ... @@` hunk header, and the hunk
/// body. It is suitable for `git apply [--cached] [--reverse]`.
///
/// The hunk header counts are recomputed from the payload lines so that an
/// arbitrary user-selected slice is always internally consistent:
/// `old_count = context + del`, `new_count = context + add`. The start line
/// numbers are parsed out of `payload.header` and trusted as-is (libgit2 already
/// reports the git-conventional "line before" for zero-count sides).
///
/// # Rejections (defense in depth)
///
/// - Untracked payloads (a synthesized `/dev/null` patch cannot carry the file's
///   mode; stage untracked files whole-file instead).
/// - An empty `lines` array (nothing to apply).
/// - Any path or line content carrying an embedded `\n` or `\0` (patch-stream
///   injection). A trailing `\r` in content stays legal — it is load-bearing for
///   CRLF files.
pub fn build_patch(payload: &HunkPatchPayload) -> AppResult<Vec<u8>> {
    if payload.is_untracked {
        return Err(AppError::validation(
            "Refusing to synthesize a patch for an untracked file; stage it whole-file instead",
        ));
    }
    if payload.lines.is_empty() {
        return Err(AppError::validation("Hunk payload has no lines to apply"));
    }

    let new_path = payload.path.as_str();
    let old_path = payload.old_path.as_deref().unwrap_or(new_path);

    // Reject path/content injection. Newline or NUL in a path would forge extra
    // patch-header lines; the same in line content would forge extra body lines.
    // A trailing (or embedded) `\r` is deliberately allowed for CRLF fidelity.
    for p in [new_path, old_path] {
        if p.contains('\n') || p.contains('\0') {
            return Err(AppError::validation(
                "Path must not contain newline or NUL characters",
            ));
        }
    }
    for line in &payload.lines {
        if line.content.contains('\n') || line.content.contains('\0') {
            return Err(AppError::validation(
                "Line content must not contain newline or NUL characters",
            ));
        }
    }

    let (old_start, new_start) = parse_starts(&payload.header)?;

    let mut old_count: u64 = 0;
    let mut new_count: u64 = 0;
    for line in &payload.lines {
        match line.kind {
            PatchLineKind::Context => {
                old_count += 1;
                new_count += 1;
            }
            PatchLineKind::Add => new_count += 1,
            PatchLineKind::Del => old_count += 1,
            PatchLineKind::NoNewline => {}
        }
    }

    let mut out: Vec<u8> = Vec::new();

    // File header.
    push_str(&mut out, &format!("diff --git a/{old_path} b/{new_path}\n"));
    push_str(&mut out, &format!("--- a/{old_path}\n"));
    push_str(&mut out, &format!("+++ b/{new_path}\n"));

    // Recomputed hunk header (the trailing section text is intentionally omitted;
    // git ignores it when applying).
    push_str(
        &mut out,
        &format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"),
    );

    // Hunk body — content emitted verbatim, only the terminator is synthesized.
    for line in &payload.lines {
        match line.kind {
            PatchLineKind::Context => push_body_line(&mut out, b' ', &line.content),
            PatchLineKind::Add => push_body_line(&mut out, b'+', &line.content),
            PatchLineKind::Del => push_body_line(&mut out, b'-', &line.content),
            PatchLineKind::NoNewline => out.extend_from_slice(NO_NEWLINE_MARKER),
        }
    }

    Ok(out)
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// Emit `<prefix><content>\n`, preserving `content` bytes exactly.
fn push_body_line(out: &mut Vec<u8>, prefix: u8, content: &str) {
    out.push(prefix);
    out.extend_from_slice(content.as_bytes());
    out.push(b'\n');
}

/// Parse the old/new start line numbers out of an `@@ -a,b +c,d @@` header.
///
/// Tolerant of the git shorthand where a count of 1 is omitted (`-a +c`) and of
/// any trailing function-context text after the closing `@@`.
fn parse_starts(header: &str) -> AppResult<(u64, u64)> {
    let mut old_start: Option<u64> = None;
    let mut new_start: Option<u64> = None;

    for tok in header.split_whitespace() {
        if let Some(rest) = tok.strip_prefix('-') {
            if old_start.is_none() {
                old_start = rest.split(',').next().and_then(|n| n.parse().ok());
            }
        } else if let Some(rest) = tok.strip_prefix('+') {
            if new_start.is_none() {
                new_start = rest.split(',').next().and_then(|n| n.parse().ok());
            }
        }
    }

    match (old_start, new_start) {
        (Some(o), Some(n)) => Ok((o, n)),
        _ => Err(AppError::validation(format!(
            "Malformed hunk header: {header}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: PatchLineKind, content: &str) -> PatchLine {
        PatchLine {
            kind,
            content: content.to_string(),
        }
    }

    fn payload(header: &str, lines: Vec<PatchLine>) -> HunkPatchPayload {
        HunkPatchPayload {
            path: "f.txt".to_string(),
            old_path: None,
            staged: false,
            is_untracked: false,
            context_lines: 3,
            header: header.to_string(),
            lines,
        }
    }

    fn as_str(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn modify_hunk_recomputes_counts() {
        use PatchLineKind::*;
        let p = payload(
            "@@ -1,5 +1,5 @@ fn context()",
            vec![
                line(Context, "a"),
                line(Context, "b"),
                line(Del, "c"),
                line(Add, "CHANGED"),
                line(Context, "d"),
                line(Context, "e"),
            ],
        );
        let out = as_str(&build_patch(&p).unwrap());
        let expected = "diff --git a/f.txt b/f.txt\n\
             --- a/f.txt\n\
             +++ b/f.txt\n\
             @@ -1,5 +1,5 @@\n\
             \x20a\n b\n-c\n+CHANGED\n d\n e\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn untracked_payload_is_rejected() {
        use PatchLineKind::*;
        // Repurposed from the former `untracked_becomes_new_file_patch`: untracked
        // files no longer go through a synthesized `/dev/null` patch (that path
        // corrupted exec bits / symlinks). build_patch must now refuse them.
        let mut p = payload("@@ -0,0 +1,2 @@", vec![line(Add, "x1"), line(Add, "x2")]);
        p.is_untracked = true;
        let err = build_patch(&p).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("untracked"),
            "expected an untracked rejection, got: {err}"
        );
    }

    #[test]
    fn empty_lines_is_rejected() {
        let p = payload("@@ -1,0 +1,0 @@", vec![]);
        assert!(build_patch(&p).is_err(), "empty hunk must be rejected");
    }

    #[test]
    fn embedded_newline_in_content_is_rejected() {
        use PatchLineKind::*;
        // A `\n` smuggled into content would forge extra patch body lines.
        let p = payload("@@ -1,1 +1,1 @@", vec![line(Add, "a\nb")]);
        assert!(
            build_patch(&p).is_err(),
            "embedded newline must be rejected"
        );
    }

    #[test]
    fn embedded_nul_in_content_is_rejected() {
        use PatchLineKind::*;
        let p = payload("@@ -1,1 +1,1 @@", vec![line(Add, "a\0b")]);
        assert!(build_patch(&p).is_err(), "embedded NUL must be rejected");
    }

    #[test]
    fn trailing_cr_in_content_stays_legal() {
        use PatchLineKind::*;
        // CRLF fidelity: a trailing (or embedded) `\r` must NOT be rejected.
        let p = payload("@@ -1,1 +1,1 @@", vec![line(Del, "a\r"), line(Add, "b\r")]);
        assert!(build_patch(&p).is_ok(), "trailing CR must remain legal");
    }

    #[test]
    fn pure_addition_has_zero_old_count() {
        use PatchLineKind::*;
        // Adding two lines after line 5, zero context.
        let p = payload("@@ -5,0 +6,2 @@", vec![line(Add, "n1"), line(Add, "n2")]);
        let out = as_str(&build_patch(&p).unwrap());
        assert!(out.contains("@@ -5,0 +6,2 @@\n"), "got: {out}");
    }

    #[test]
    fn pure_deletion_has_zero_new_count() {
        use PatchLineKind::*;
        let p = payload("@@ -6,2 +5,0 @@", vec![line(Del, "n1"), line(Del, "n2")]);
        let out = as_str(&build_patch(&p).unwrap());
        assert!(out.contains("@@ -6,2 +5,0 @@\n"), "got: {out}");
    }

    #[test]
    fn no_newline_marker_is_emitted_verbatim_and_uncounted() {
        use PatchLineKind::*;
        let p = payload(
            "@@ -1,2 +1,2 @@",
            vec![
                line(Context, "p"),
                line(Del, "q"),
                line(Add, "qEDIT"),
                // The marker's own content is ignored; a fixed marker is emitted.
                line(NoNewline, "\\ No newline at end of file"),
            ],
        );
        let out = as_str(&build_patch(&p).unwrap());
        assert!(
            out.ends_with("+qEDIT\n\\ No newline at end of file\n"),
            "got: {out}"
        );
        // Counts ignore the marker: 1 context + 1 del = 2 old; 1 context + 1 add = 2 new.
        assert!(out.contains("@@ -1,2 +1,2 @@\n"), "got: {out}");
    }

    #[test]
    fn crlf_content_round_trips_byte_exact() {
        use PatchLineKind::*;
        // Content carries the trailing \r; build_patch must not strip it.
        let p = payload(
            "@@ -1,3 +1,3 @@",
            vec![
                line(Context, "a\r"),
                line(Del, "c\r"),
                line(Add, "CHANGED\r"),
            ],
        );
        let bytes = build_patch(&p).unwrap();
        // The changed line must be exactly `+CHANGED\r\n` in the patch stream.
        let needle = b"+CHANGED\r\n";
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "expected CR-preserved add line in {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(bytes.windows(4).any(|w| w == b" a\r\n"));
    }

    #[test]
    fn renamed_old_path_is_emitted() {
        use PatchLineKind::*;
        let mut p = payload("@@ -1,1 +1,1 @@", vec![line(Del, "x"), line(Add, "y")]);
        p.old_path = Some("old.txt".to_string());
        let out = as_str(&build_patch(&p).unwrap());
        assert!(
            out.starts_with("diff --git a/old.txt b/f.txt\n"),
            "got: {out}"
        );
        assert!(out.contains("--- a/old.txt\n"), "got: {out}");
        assert!(out.contains("+++ b/f.txt\n"), "got: {out}");
    }

    #[test]
    fn header_shorthand_count_of_one_parses() {
        use PatchLineKind::*;
        // git omits ",1" when a side has exactly one line.
        let p = payload("@@ -3 +3 @@", vec![line(Del, "x"), line(Add, "y")]);
        let out = as_str(&build_patch(&p).unwrap());
        assert!(out.contains("@@ -3,1 +3,1 @@\n"), "got: {out}");
    }

    #[test]
    fn malformed_header_errors() {
        use PatchLineKind::*;
        let p = payload("not a hunk header", vec![line(Add, "x")]);
        assert!(build_patch(&p).is_err());
    }
}
