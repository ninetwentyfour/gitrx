use std::path::Path;

use git2::{Delta, Diff, DiffFindOptions, DiffLineType, DiffOptions, Patch, Repository};
use serde::Serialize;

use crate::error::AppResult;

/// The kind of a single diff line, matching the frontend contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
    NoNewline,
}

/// A single line within a hunk.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line_no: Option<u32>,
    pub new_line_no: Option<u32>,
    pub content: String,
}

/// A contiguous block of changes with its `@@ ... @@` header.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

/// A full single-file diff returned to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub language: Option<String>,
    pub is_binary: bool,
    pub is_untracked: bool,
    /// True when any line in any hunk contained bytes that are not valid UTF-8
    /// (they were rendered lossily with U+FFFD replacement characters). The
    /// frontend uses this to steer the user to whole-file staging, and
    /// `apply_hunk_verified` refuses hunk staging for such a file because the
    /// reconstructed patch would write the corrupted (replaced) bytes back.
    pub is_lossy: bool,
    pub hunks: Vec<Hunk>,
}

/// Compute the diff of a single file `path`.
///
/// - `staged = false` -> index-vs-workdir (unstaged edits + untracked content).
/// - `staged = true`  -> HEAD-tree-vs-index (staged edits). An unborn HEAD uses a
///   `None` old tree so every indexed line appears as an addition.
///
/// `context_lines` feeds `DiffOptions::context_lines` (UI slider range 0-8).
pub fn file_diff(
    repo: &Repository,
    path: &str,
    staged: bool,
    context_lines: u32,
) -> AppResult<FileDiff> {
    let mut opts = DiffOptions::new();
    opts.context_lines(context_lines)
        .pathspec(path)
        // Treat `path` as a literal filename, not a glob. Without this a file
        // named e.g. `data[1].txt` would be matched as a character class and pull
        // in an unrelated sibling like `data1.txt`.
        .disable_pathspec_match(true)
        .include_typechange(true)
        // Cap the blob size libgit2 will load; oversized files fall through to
        // the binary path (empty hunks) instead of being rendered into memory.
        .max_size(crate::git::MAX_DIFF_BYTES);

    let mut diff = if staged {
        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?
    } else {
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        repo.diff_index_to_workdir(None, Some(&mut opts))?
    };

    // Populate rename detection so `oldPath` surfaces on the staged side.
    if staged {
        let mut find = DiffFindOptions::new();
        find.renames(true);
        diff.find_similar(Some(&mut find))?;
    }

    let language = language_for_path(path);

    let Some(mut idx) = find_delta_index(&diff, path) else {
        // No delta touches this path -> nothing to show.
        return Ok(FileDiff {
            path: path.to_string(),
            old_path: None,
            language,
            is_binary: false,
            is_untracked: false,
            is_lossy: false,
            hunks: Vec::new(),
        });
    };

    // M1: staged rename detection. The single-path pathspec above filters the
    // old-side DELETED delta out of the diff *before* `find_similar` runs, so a
    // `git mv a b` (+ edit) staged as a pair surfaces here only as an `Added`
    // `b` — the rename can never be paired. When the staged delta comes back
    // `Added`, re-run the staged diff WITHOUT the pathspec so both sides are
    // present, run rename detection, and, if `path` is now the new side of a
    // rename/copy, switch to that (diff, delta) so `old_path` and the real
    // del/add body surface. Genuinely-new files stay `Added` and fall through.
    let mut rescan: Option<Diff<'_>> = None;
    if staged && diff.get_delta(idx).expect("delta index in range").status() == Delta::Added {
        let mut full_opts = DiffOptions::new();
        full_opts
            .context_lines(context_lines)
            .include_typechange(true)
            .max_size(crate::git::MAX_DIFF_BYTES);
        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        let mut full = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut full_opts))?;
        let mut find = DiffFindOptions::new();
        find.renames(true).copies(true);
        full.find_similar(Some(&mut find))?;

        if let Some(fi) = find_delta_by_new_path(&full, path) {
            if matches!(
                full.get_delta(fi).expect("delta index in range").status(),
                Delta::Renamed | Delta::Copied
            ) {
                idx = fi;
                rescan = Some(full);
            }
        }
    }
    let diff = rescan.as_ref().unwrap_or(&diff);

    let delta = diff.get_delta(idx).expect("delta index in range");
    let is_untracked = delta.status() == Delta::Untracked;
    let new_path = delta
        .new_file()
        .path()
        .map_or_else(|| path.to_string(), |p| p.to_string_lossy().into_owned());
    let old_path = match delta.status() {
        Delta::Renamed | Delta::Copied => delta
            .old_file()
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|op| op != &new_path),
        _ => None,
    };

    let patch = Patch::from_diff(diff, idx)?;

    // Binary files: libgit2 sets the binary flag once the patch is computed.
    if delta.new_file().is_binary() || delta.old_file().is_binary() || patch.is_none() {
        return Ok(FileDiff {
            path: new_path,
            old_path,
            language,
            is_binary: true,
            is_untracked,
            is_lossy: false,
            hunks: Vec::new(),
        });
    }

    let patch = patch.expect("patch present after None check");
    let (hunks, is_lossy) = collect_hunks(&patch)?;

    Ok(FileDiff {
        path: new_path,
        old_path,
        language,
        is_binary: false,
        is_untracked,
        is_lossy,
        hunks,
    })
}

/// Find the delta whose new- or old-side path matches `path` exactly.
///
/// Returns `None` when no delta matches. There is deliberately **no** fallback
/// to the first delta: with a literal pathspec a mismatch means "this file has
/// no change", and returning an unrelated file's diff would be a data-integrity
/// hazard (the caller would render — and potentially stage — the wrong file).
fn find_delta_index(diff: &Diff, path: &str) -> Option<usize> {
    let target = Path::new(path);
    for (i, delta) in diff.deltas().enumerate() {
        let matches =
            delta.new_file().path() == Some(target) || delta.old_file().path() == Some(target);
        if matches {
            return Some(i);
        }
    }
    None
}

/// Find the delta whose **new-side** path matches `path` exactly.
///
/// Used by the staged rename rescan (M1): after re-running the diff without a
/// pathspec, we must locate the rename by its target (new) path, not its source,
/// so we don't accidentally pick up the old-side DELETED delta.
fn find_delta_by_new_path(diff: &Diff, path: &str) -> Option<usize> {
    let target = Path::new(path);
    diff.deltas()
        .position(|delta| delta.new_file().path() == Some(target))
}

/// Walk every hunk/line of `patch` into the serializable `Hunk` shape.
///
/// Returns `(hunks, is_lossy)` where `is_lossy` is `true` if **any** body line
/// carried bytes that are not valid UTF-8 (see [`line_content`]).
fn collect_hunks(patch: &Patch) -> AppResult<(Vec<Hunk>, bool)> {
    let mut hunks = Vec::with_capacity(patch.num_hunks());
    let mut is_lossy = false;

    for h in 0..patch.num_hunks() {
        let (hunk, _line_count) = patch.hunk(h)?;
        let header = trim_line(&String::from_utf8_lossy(hunk.header()));

        let num_lines = patch.num_lines_in_hunk(h)?;
        let mut lines = Vec::with_capacity(num_lines);

        for l in 0..num_lines {
            let line = patch.line_in_hunk(h, l)?;
            let (converted, lossy) = convert_line(&line);
            is_lossy |= lossy;
            lines.push(converted);
        }

        hunks.push(Hunk {
            header,
            old_start: hunk.old_start(),
            old_lines: hunk.old_lines(),
            new_start: hunk.new_start(),
            new_lines: hunk.new_lines(),
            lines,
        });
    }

    Ok((hunks, is_lossy))
}

/// Map one libgit2 `DiffLine` to our contract line, including the special
/// end-of-file "no newline" markers.
///
/// Returns `(line, is_lossy)`; the no-newline marker is synthesized and never
/// lossy.
fn convert_line(line: &git2::DiffLine) -> (DiffLine, bool) {
    match line.origin_value() {
        DiffLineType::ContextEOFNL | DiffLineType::AddEOFNL | DiffLineType::DeleteEOFNL => (
            DiffLine {
                kind: DiffLineKind::NoNewline,
                old_line_no: None,
                new_line_no: None,
                content: "\\ No newline at end of file".to_string(),
            },
            false,
        ),
        DiffLineType::Addition => {
            let (content, lossy) = line_content(line);
            (
                DiffLine {
                    kind: DiffLineKind::Add,
                    old_line_no: None,
                    new_line_no: line.new_lineno(),
                    content,
                },
                lossy,
            )
        }
        DiffLineType::Deletion => {
            let (content, lossy) = line_content(line);
            (
                DiffLine {
                    kind: DiffLineKind::Del,
                    old_line_no: line.old_lineno(),
                    new_line_no: None,
                    content,
                },
                lossy,
            )
        }
        // Context and any other origin (file/hunk headers never appear in
        // `line_in_hunk`) are treated as context.
        _ => {
            let (content, lossy) = line_content(line);
            (
                DiffLine {
                    kind: DiffLineKind::Context,
                    old_line_no: line.old_lineno(),
                    new_line_no: line.new_lineno(),
                    content,
                },
                lossy,
            )
        }
    }
}

/// Line content with only the trailing `\n` stripped, plus a `lossy` flag.
///
/// Fast path: valid UTF-8 bytes are decoded losslessly (`lossy = false`).
/// Otherwise the bytes are rendered with [`String::from_utf8_lossy`] (U+FFFD
/// replacements) and `lossy = true` — a signal callers use to refuse hunk
/// staging, because the replaced bytes would corrupt the file if written back.
///
/// A trailing `\r` is preserved on purpose: hunk staging round-trips this
/// content back into a patch (`git/patch.rs`), and CRLF files only apply
/// cleanly when the `\r` survives. Display layers strip it themselves.
fn line_content(line: &git2::DiffLine) -> (String, bool) {
    let bytes = line.content();
    std::str::from_utf8(bytes).map_or_else(
        |_| {
            let s = String::from_utf8_lossy(bytes);
            (s.strip_suffix('\n').unwrap_or(&s).to_string(), true)
        },
        |s| (s.strip_suffix('\n').unwrap_or(s).to_string(), false),
    )
}

/// Strip one trailing `\n` and, if present, the `\r` that precedes it.
fn trim_line(s: &str) -> String {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let s = s.strip_suffix('\r').unwrap_or(s);
    s.to_string()
}

/// Map a file path to a Shiki language id, or `None` when unknown.
///
/// Checks special filenames first (Dockerfile, Makefile), then the extension.
#[must_use]
pub fn language_for_path(path: &str) -> Option<String> {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    match name.as_str() {
        "Dockerfile" => return Some("dockerfile".to_string()),
        "Makefile" | "makefile" | "GNUmakefile" => return Some("make".to_string()),
        _ => {}
    }

    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())?;

    let lang = match ext.as_str() {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "py" => "python",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "sh" | "bash" | "zsh" => "shellscript",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "xml" => "xml",
        "vue" => "vue",
        "svelte" => "svelte",
        _ => return None,
    };
    Some(lang.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_file, setup};
    use std::fs;
    use std::path::Path as StdPath;

    #[test]
    fn two_regions_split_by_context() {
        let (dir, repo) = setup();
        // 20-line baseline.
        let base: String = (1..=20).fold(String::new(), |mut s, n| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line{n}");
            s
        });
        commit_file(&repo, dir.path(), "f.txt", &base);

        // Edit line 3 and line 17 (far apart) -> two separated regions.
        let mut lines: Vec<String> = (1..=20).map(|n| format!("line{n}")).collect();
        lines[2] = "line3-changed".to_string();
        lines[16] = "line17-changed".to_string();
        let edited = lines.join("\n") + "\n";
        fs::write(dir.path().join("f.txt"), &edited).unwrap();

        let d3 = file_diff(&repo, "f.txt", false, 3).unwrap();
        assert_eq!(d3.hunks.len(), 2, "far-apart edits stay separate at ctx=3");
        assert!(!d3.is_binary);
        assert!(!d3.is_untracked);
        assert_eq!(d3.language.as_deref(), None); // .txt unknown

        // First hunk starts near line 3 (3 lines of leading context => start 1 or so).
        let first = &d3.hunks[0];
        assert!(first.old_start <= 3 && first.new_start <= 3);

        // A del line has new_line_no null + old set; add line the inverse; context both.
        let del = d3
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.kind == DiffLineKind::Del)
            .expect("a deletion");
        assert!(del.new_line_no.is_none() && del.old_line_no.is_some());

        let add = d3
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.kind == DiffLineKind::Add)
            .expect("an addition");
        assert!(add.old_line_no.is_none() && add.new_line_no.is_some());

        let ctx = d3
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.kind == DiffLineKind::Context)
            .expect("a context line");
        assert!(ctx.old_line_no.is_some() && ctx.new_line_no.is_some());

        // With 8 lines of context the two regions merge into one hunk.
        let d8 = file_diff(&repo, "f.txt", false, 8).unwrap();
        assert_eq!(d8.hunks.len(), 1, "wide context merges the regions");
        assert_ne!(d3.hunks.len(), d8.hunks.len());
    }

    #[test]
    fn untracked_file_is_all_additions() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");

        fs::write(dir.path().join("new.rs"), "a\nb\nc\n").unwrap();
        let d = file_diff(&repo, "new.rs", false, 3).unwrap();

        assert!(d.is_untracked);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.language.as_deref(), Some("rust"));

        let adds = d.hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Add)
            .count();
        assert_eq!(adds, 3, "one add per file line");
        assert!(d.hunks[0].lines.iter().all(|l| l.kind == DiffLineKind::Add));
    }

    #[test]
    fn staged_vs_unstaged_separation() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "s.txt", "one\ntwo\n");

        // Modify + stage.
        fs::write(dir.path().join("s.txt"), "one\ntwo\nthree\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(StdPath::new("s.txt")).unwrap();
        index.write().unwrap();

        let staged = file_diff(&repo, "s.txt", true, 3).unwrap();
        assert!(!staged.hunks.is_empty(), "staged diff has the change");

        let unstaged = file_diff(&repo, "s.txt", false, 3).unwrap();
        assert!(
            unstaged.hunks.is_empty(),
            "workdir matches index -> no unstaged hunks"
        );
    }

    #[test]
    fn missing_trailing_newline_emits_marker() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "n.txt", "alpha\nbeta\n");

        // Rewrite last line without a trailing newline.
        fs::write(dir.path().join("n.txt"), "alpha\nbeta-edited").unwrap();

        let d = file_diff(&repo, "n.txt", false, 3).unwrap();
        let has_marker = d
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.kind == DiffLineKind::NoNewline);
        assert!(has_marker, "expected a noNewline marker line");

        let marker = d
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .find(|l| l.kind == DiffLineKind::NoNewline)
            .unwrap();
        assert_eq!(marker.content, "\\ No newline at end of file");
        assert!(marker.old_line_no.is_none() && marker.new_line_no.is_none());
    }

    #[test]
    fn zero_context_has_no_context_lines() {
        let (dir, repo) = setup();
        let base: String = (1..=10).fold(String::new(), |mut s, n| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "l{n}");
            s
        });
        commit_file(&repo, dir.path(), "z.txt", &base);

        let mut lines: Vec<String> = (1..=10).map(|n| format!("l{n}")).collect();
        lines[4] = "l5-changed".to_string();
        fs::write(dir.path().join("z.txt"), lines.join("\n") + "\n").unwrap();

        let d = file_diff(&repo, "z.txt", false, 0).unwrap();
        let ctx_count = d
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Context)
            .count();
        assert_eq!(ctx_count, 0, "context_lines=0 yields no context lines");
    }

    #[test]
    fn bracket_filename_returns_only_its_own_delta() {
        // Regression: a literal filename containing glob metacharacters must not
        // pull in a sibling, and querying it must return ITS diff (not a fallback
        // to some unrelated delta at index 0).
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "data[1].txt", "orig-bracket\n");
        commit_file(&repo, dir.path(), "data1.txt", "orig-plain\n");
        fs::write(dir.path().join("data[1].txt"), "mod-bracket\n").unwrap();
        fs::write(dir.path().join("data1.txt"), "mod-plain\n").unwrap();

        let bracket = file_diff(&repo, "data[1].txt", false, 3).unwrap();
        assert_eq!(bracket.path, "data[1].txt");
        let bracket_content: Vec<_> = bracket
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .map(|l| l.content.as_str())
            .collect();
        assert!(
            bracket_content.contains(&"mod-bracket"),
            "{bracket_content:?}"
        );
        assert!(
            !bracket_content.contains(&"mod-plain"),
            "sibling leaked into bracket diff: {bracket_content:?}"
        );

        let plain = file_diff(&repo, "data1.txt", false, 3).unwrap();
        assert_eq!(plain.path, "data1.txt");
        let plain_content: Vec<_> = plain
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .map(|l| l.content.as_str())
            .collect();
        assert!(plain_content.contains(&"mod-plain"), "{plain_content:?}");
        assert!(
            !plain_content.contains(&"mod-bracket"),
            "bracket leaked into plain diff: {plain_content:?}"
        );
    }

    #[test]
    fn nonmatching_path_yields_empty_diff_not_a_fallback() {
        // find_delta_index must return None (empty hunks) rather than the first
        // delta when the queried path has no change.
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "a.txt", "one\n");
        fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();

        let d = file_diff(&repo, "b.txt", false, 3).unwrap();
        assert!(d.hunks.is_empty(), "unrelated path must not borrow a diff");
    }

    #[test]
    fn oversized_untracked_file_diffs_as_binary_with_no_hunks() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");

        // 9 MiB of newlines — over the 8 MiB cap; allocated once.
        let big = "\n".repeat(9 * 1024 * 1024);
        fs::write(dir.path().join("big.txt"), &big).unwrap();

        let d = file_diff(&repo, "big.txt", false, 3).unwrap();
        assert!(d.is_binary, "oversized file must render as binary");
        assert!(d.hunks.is_empty(), "no text hunks for an oversized file");
    }

    #[test]
    fn oversized_tracked_modification_diffs_as_binary_with_no_hunks() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "grow.txt", "small\n");

        let big = "\n".repeat(9 * 1024 * 1024);
        fs::write(dir.path().join("grow.txt"), &big).unwrap();

        let d = file_diff(&repo, "grow.txt", false, 3).unwrap();
        assert!(d.is_binary, "oversized workdir side must render as binary");
        assert!(d.hunks.is_empty());
    }

    // H3: a line carrying a non-UTF-8 byte (0xE9, Latin-1 'é') must set
    // `is_lossy` so callers can steer to whole-file staging. The hunk itself
    // still renders (no NUL -> not binary), but its bytes went through the lossy
    // U+FFFD path.
    #[test]
    fn non_utf8_content_marks_diff_lossy() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "latin1.txt", "cafe\n");
        fs::write(dir.path().join("latin1.txt"), b"caf\xe9\n").unwrap();

        let d = file_diff(&repo, "latin1.txt", false, 3).unwrap();
        assert!(d.is_lossy, "non-UTF-8 bytes must flag isLossy");
        assert!(!d.is_binary, "a lone high byte is text, not binary");
        assert!(!d.hunks.is_empty());
    }

    #[test]
    fn valid_utf8_content_is_not_lossy() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "u.txt", "one\n");
        fs::write(dir.path().join("u.txt"), "one\ntwö\n").unwrap();

        let d = file_diff(&repo, "u.txt", false, 3).unwrap();
        assert!(
            !d.is_lossy,
            "valid UTF-8 (incl. multibyte) must not be lossy"
        );
    }

    // M1: the single-path pathspec filters the old-side DELETED delta out before
    // find_similar can pair it, so a staged `git mv a b` (+ edit) used to surface
    // as a pure ADD of `b`. The rescan must re-detect the rename: old_path
    // populated, and a real del/add body (not pure adds).
    #[test]
    fn staged_rename_reports_old_path_and_edit_body() {
        let (dir, repo) = setup();
        let body: String = (1..=20).fold(String::new(), |mut s, n| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "line{n}");
            s
        });
        commit_file(&repo, dir.path(), "a.txt", &body);

        // Rename on disk + one-line edit, then stage the rename in the index.
        let edited: String = (1..=20)
            .map(|n| {
                if n == 5 {
                    "line5-changed\n".to_string()
                } else {
                    format!("line{n}\n")
                }
            })
            .collect();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        fs::write(dir.path().join("b.txt"), &edited).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(StdPath::new("a.txt")).unwrap();
        index.add_path(StdPath::new("b.txt")).unwrap();
        index.write().unwrap();

        let d = file_diff(&repo, "b.txt", true, 3).unwrap();
        assert_eq!(
            d.old_path.as_deref(),
            Some("a.txt"),
            "staged rename must surface old_path"
        );
        let has_del = d
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.kind == DiffLineKind::Del);
        let has_add = d
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| l.kind == DiffLineKind::Add);
        assert!(
            has_del && has_add,
            "expected a del/add edit body, not a pure-add file"
        );
    }

    // A genuinely new staged file must stay Added (no false rename from the
    // rescan) and carry no old_path.
    #[test]
    fn staged_new_file_has_no_old_path() {
        let (dir, repo) = setup();
        commit_file(&repo, dir.path(), "seed.txt", "seed\n");
        fs::write(dir.path().join("brand-new.txt"), "hello\nworld\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(StdPath::new("brand-new.txt")).unwrap();
        index.write().unwrap();

        let d = file_diff(&repo, "brand-new.txt", true, 3).unwrap();
        assert!(d.old_path.is_none(), "a new file must not report a rename");
        assert!(!d.hunks.is_empty());
    }

    #[test]
    fn language_mapping() {
        assert_eq!(language_for_path("a/b.rs").as_deref(), Some("rust"));
        assert_eq!(language_for_path("x.tsx").as_deref(), Some("tsx"));
        assert_eq!(
            language_for_path("Dockerfile").as_deref(),
            Some("dockerfile")
        );
        assert_eq!(language_for_path("Makefile").as_deref(), Some("make"));
        assert_eq!(language_for_path("h.hpp").as_deref(), Some("cpp"));
        assert_eq!(language_for_path("s.unknownext"), None);
    }
}
