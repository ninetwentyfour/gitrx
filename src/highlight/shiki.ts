import type { Highlighter, ThemedToken } from "shiki";
import type { FileDiff } from "../types/ipc";
import { logDebug } from "../lib/log";
import { tokyowhale } from "./tokyowhale";

/** The two themes the app can render diffs with. */
export type DiffTheme = "tokyowhale" | "one-light";

/** A single highlighted span: text plus its resolved colour and optional style. */
export type HlToken = {
  content: string;
  color: string;
  fontStyle?: "italic" | "bold";
};

/** Tokens for one diff line, or `null` when the line has no highlighting. */
export type DiffLineTokens = HlToken[] | null;

/** Per-hunk, per-line token map. Indices mirror `diff.hunks[h].lines[l]`. */
export type DiffTokens = DiffLineTokens[][];

// Size guards: highlighting a giant blob blocks the main thread and the payoff
// is nil (nobody reads a 5k-line diff token-by-token). A single pathological
// line (minified bundle) also tanks the tokenizer.
const MAX_TOTAL_LINES = 5000;
const MAX_LINE_LENGTH = 1000;

// FontStyle is a bitmask in vscode-textmate: Italic = 1, Bold = 2.
const FONT_STYLE_ITALIC = 1;
const FONT_STYLE_BOLD = 2;

// Lazy singleton. The whole `shiki` module (core engine + language/theme maps)
// is dynamically imported on first use so it stays OUT of the initial app
// bundle; the highlighter is then built once and reused for the app's lifetime.
let highlighterPromise: Promise<Highlighter> | null = null;
const loadedLangs = new Set<string>();

function getHighlighter(): Promise<Highlighter> {
  highlighterPromise ??= import("shiki").then((shiki) =>
    shiki.createHighlighter({
      themes: [tokyowhale, "one-light"],
      // Start with no languages; each is dynamically imported on first sight
      // so the initial bundle stays small (grammars are separate chunks).
      langs: [],
    }),
  );
  return highlighterPromise;
}

/** Lower-case a non-null language; the real validity check is `ensureLang`. */
function resolveLang(language: string | null): string | null {
  return language ? language.toLowerCase() : null;
}

/**
 * Ensure a grammar is loaded into the singleton highlighter (idempotent).
 * Returns `false` when shiki has no grammar for `lang` — `loadLanguage` throws
 * for an unknown bundled id — so the caller falls back to plain text. This
 * doubles as the "unknown language" guard, avoiding a static import of shiki's
 * full language map just to validate a name.
 */
async function ensureLang(hl: Highlighter, lang: string): Promise<boolean> {
  if (loadedLangs.has(lang)) return true;
  try {
    await hl.loadLanguage(lang as Parameters<Highlighter["loadLanguage"]>[0]);
    loadedLangs.add(lang);
    return true;
  } catch {
    return false;
  }
}

/** Strip a single trailing CRLF `\r` for display/highlighting only. */
function stripCr(content: string): string {
  return content.endsWith("\r") ? content.slice(0, -1) : content;
}

/** Where a diff line's content lives in the reconstructed blobs. */
type LineRef = {
  /** 0-based index into the OLD blob's lines, or -1 if not on the old side. */
  old: number;
  /** 0-based index into the NEW blob's lines, or -1 if not on the new side. */
  new: number;
};

type Reconstructed = {
  oldText: string;
  newText: string;
  /** refs[hunkIdx][lineIdx] → blob positions (or -1/-1 for noNewline markers). */
  refs: LineRef[][];
};

/**
 * Rebuild OLD (context + deletions) and NEW (context + additions) text blobs
 * from a diff, concatenating all hunks in order, and record for every diff line
 * which blob line it maps to. `noNewline` markers contribute no blob line.
 *
 * Highlighting per whole-blob (rather than per fragment) is what lets shiki see
 * multi-line constructs — strings, block comments, JSX — that a single `+`/`-`
 * line would truncate.
 */
export function reconstructBlobs(diff: FileDiff): Reconstructed {
  const oldLines: string[] = [];
  const newLines: string[] = [];
  const refs: LineRef[][] = [];

  // Accepted trade-off: hunks are concatenated back-to-back into single OLD/NEW
  // blobs with no gap for the (elided) unchanged lines between them. The grammar
  // tokenizer therefore carries state ACROSS hunk boundaries — e.g. an unterminated
  // block comment or template literal opened in one hunk bleeds into the next. In
  // real diffs the elided context between hunks almost always closes such
  // constructs, so cross-hunk bleed is rare and the multi-line highlighting win
  // (strings/comments/JSX spanning many lines WITHIN a hunk) far outweighs it. The
  // alternative — tokenizing each hunk in isolation — truncates those constructs
  // on every hunk, which looks worse far more often.
  for (const hunk of diff.hunks) {
    const hunkRefs: LineRef[] = [];
    for (const line of hunk.lines) {
      const ref: LineRef = { old: -1, new: -1 };
      const text = stripCr(line.content);
      if (line.kind === "context") {
        ref.old = oldLines.push(text) - 1;
        ref.new = newLines.push(text) - 1;
      } else if (line.kind === "del") {
        ref.old = oldLines.push(text) - 1;
      } else if (line.kind === "add") {
        ref.new = newLines.push(text) - 1;
      }
      // noNewline: leave both at -1 (the marker is not real source).
      hunkRefs.push(ref);
    }
    refs.push(hunkRefs);
  }

  return { oldText: oldLines.join("\n"), newText: newLines.join("\n"), refs };
}

/** Would this diff blow a size guard? (line count / single-line length) */
function exceedsSizeGuards(diff: FileDiff): boolean {
  let total = 0;
  for (const hunk of diff.hunks) {
    total += hunk.lines.length;
    if (total > MAX_TOTAL_LINES) return true;
    for (const line of hunk.lines) {
      if (line.content.length > MAX_LINE_LENGTH) return true;
    }
  }
  return false;
}

/** Map a shiki font-style bitmask to our narrow style union. */
function toFontStyle(fontStyle: number | undefined): "italic" | "bold" | undefined {
  if (fontStyle === undefined) return undefined;
  if (fontStyle & FONT_STYLE_BOLD) return "bold";
  if (fontStyle & FONT_STYLE_ITALIC) return "italic";
  return undefined;
}

/** Convert a shiki token line into our lightweight token line. */
function toHlLine(tokens: ThemedToken[], defaultFg: string): HlToken[] {
  return tokens.map((t) => {
    const style = toFontStyle(t.fontStyle);
    const token: HlToken = { content: t.content, color: t.color ?? defaultFg };
    if (style) token.fontStyle = style;
    return token;
  });
}

/**
 * Highlight a whole file diff and return a per-hunk / per-line token map, or
 * `null` when highlighting is impossible or unwise (unknown language, size
 * guard, or a shiki failure). The caller renders plain text on `null`.
 *
 * Additions take NEW-side tokens, deletions OLD-side, context either (NEW).
 */
export async function highlightDiff(diff: FileDiff, theme: DiffTheme): Promise<DiffTokens | null> {
  if (diff.isBinary || diff.hunks.length === 0) return null;
  if (exceedsSizeGuards(diff)) {
    // Skipping tokenization for an oversized/pathological diff — noted because a
    // guard hit correlates with the large payloads we're hunting for.
    logDebug(`highlight skipped (size guard): path=${diff.path} hunks=${diff.hunks.length}`);
    return null;
  }

  const lang = resolveLang(diff.language);
  if (!lang) return null;

  let hl: Highlighter;
  try {
    hl = await getHighlighter();
  } catch {
    return null;
  }
  if (!(await ensureLang(hl, lang))) return null;

  const { oldText, newText, refs } = reconstructBlobs(diff);

  // `lang` is a validated runtime string and `theme` is the registered custom
  // theme name; both are looser than shiki's bundled literal unions, so cast.
  const tokenOpts = { lang, theme } as Parameters<Highlighter["codeToTokensBase"]>[1];
  let oldTokens: ThemedToken[][];
  let newTokens: ThemedToken[][];
  try {
    oldTokens = oldText ? hl.codeToTokensBase(oldText, tokenOpts) : [];
    newTokens = newText ? hl.codeToTokensBase(newText, tokenOpts) : [];
  } catch {
    return null;
  }

  const defaultFg = hl.getTheme(theme).fg ?? "#000000";
  const oldHl = oldTokens.map((line) => toHlLine(line, defaultFg));
  const newHl = newTokens.map((line) => toHlLine(line, defaultFg));

  return refs.map((hunkRefs) =>
    hunkRefs.map((ref) => {
      if (ref.new >= 0) return newHl[ref.new] ?? null;
      if (ref.old >= 0) return oldHl[ref.old] ?? null;
      return null;
    }),
  );
}
