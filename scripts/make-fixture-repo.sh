#!/usr/bin/env bash
#
# make-fixture-repo.sh — build a throwaway git repository that exercises every
# GitX-staging edge case gitrx cares about, print its path, and show how to
# open it. The repo lives in a mktemp directory; delete it whenever you like.
#
#   ./scripts/make-fixture-repo.sh
#
set -euo pipefail

repo="$(mktemp -d "${TMPDIR:-/tmp}/rust-gitx-fixture.XXXXXX")"

# All repo-scoped git calls go through this wrapper; `command git` (below) is the
# real binary for the one-time init.
git() { command git -C "$repo" "$@"; }

# Portable in-place single-line replace: edit_line <file> <lineno> <text>
edit_line() {
  awk -v n="$2" -v t="$3" 'NR==n { $0 = t } { print }' "$1" >"$1.tmp" && mv "$1.tmp" "$1"
}

command git init -q "$repo"
# Deterministic, unsigned commits; keep CRLF bytes verbatim (no autocrlf munging).
git config user.name "Fixture Bot"
git config user.email "fixture@example.com"
git config commit.gpgsign false
git config core.autocrlf false

# ---------------------------------------------------------------------------
# Baseline: ~ a handful of committed files that the mutations below act upon.
# ---------------------------------------------------------------------------

# multi-hunk target: 60 numbered lines.
for i in $(seq 1 60); do echo "line $i"; done >"$repo/multi_hunk.txt"

# partially-staged target.
printf 'alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\n' >"$repo/partial.txt"

# file to be deleted.
printf 'this file will be removed\n' >"$repo/to_delete.txt"

# file to be renamed.
printf 'rename me\nkeep this line\n' >"$repo/old_name.txt"

# binary file (~4 KB of randomness).
dd if=/dev/urandom of="$repo/asset.bin" bs=1024 count=4 status=none

# CRLF file (carriage returns preserved by core.autocrlf=false).
printf 'crlf one\r\ncrlf two\r\ncrlf three\r\n' >"$repo/crlf.txt"

# file with NO trailing newline.
printf 'no newline at eof' >"$repo/no_newline.txt"

# bracket-named file AND a plain sibling that could be confused for a glob.
printf 'bracket original\n' >"$repo/data[1].txt"
printf 'plain original\n' >"$repo/data1.txt"

# filename with spaces + a non-ASCII character.
printf 'unicode original\n' >"$repo/my filé.txt"

git add -A
git commit -qm "Baseline fixture commit"

# ---------------------------------------------------------------------------
# Mutations producing the working-tree / index states under test.
# ---------------------------------------------------------------------------

# Multi-hunk: three separated edit regions in the 60-line file.
edit_line "$repo/multi_hunk.txt" 8 "line 8 CHANGED"
edit_line "$repo/multi_hunk.txt" 30 "line 30 CHANGED"
edit_line "$repo/multi_hunk.txt" 52 "line 52 CHANGED"

# Partially staged: edit region A and stage it, then edit region B (left unstaged).
edit_line "$repo/partial.txt" 1 "ALPHA staged edit"
git add partial.txt
edit_line "$repo/partial.txt" 6 "FOXTROT unstaged edit"

# Untracked plain file.
printf 'i am untracked\n' >"$repo/untracked.txt"

# Untracked executable script.
printf '#!/usr/bin/env bash\necho "untracked script"\n' >"$repo/run.sh"
chmod +x "$repo/run.sh"

# Deleted tracked file (unstaged working-tree deletion).
rm "$repo/to_delete.txt"

# Renamed file with a small edit, staged.
git mv old_name.txt new_name.txt
printf 'rename me\nkeep this line\nplus an edit\n' >"$repo/new_name.txt"
git add new_name.txt

# Binary file modified after commit.
dd if=/dev/urandom of="$repo/asset.bin" bs=1024 count=4 status=none

# CRLF file modified (append another CRLF line).
printf 'crlf one\r\ncrlf two\r\ncrlf three\r\ncrlf four\r\n' >"$repo/crlf.txt"

# No-trailing-newline file modified at EOF (still no trailing newline).
printf 'no newline at eof — now edited' >"$repo/no_newline.txt"

# Bracket-named file AND its plain sibling both modified.
printf 'bracket original\nbracket edit\n' >"$repo/data[1].txt"
printf 'plain original\nplain edit\n' >"$repo/data1.txt"

# Spaces + unicode filename modified.
printf 'unicode original\nunicode edit\n' >"$repo/my filé.txt"

# ---------------------------------------------------------------------------
# Report.
# ---------------------------------------------------------------------------
echo
echo "=== git status --short ==="
git status --short
echo
echo "Fixture repository: $repo"
echo "open with: bun run tauri dev -- -- $repo"
