# gitrx

A lightweight, GitX-style git client for macOS built with [Tauri 2.x](https://tauri.app/), React, and TypeScript. It shows a repository's working-tree status and lets you stage, unstage, discard, and commit changes at the file or hunk level, with a live-updating view backed by a filesystem watcher.

## Development

Requires [Bun](https://bun.sh/) and a Rust toolchain.

```sh
bun install            # install frontend dependencies
bun run tauri dev      # run the app in development
bun run tauri build    # produce a release .app bundle
bun test               # run the frontend test suite (Vitest)
```

Rust tests live under `src-tauri`:

```sh
cd src-tauri && cargo test
```

## CLI

`gitrx` is a small launcher (in `scripts/`) that opens a repository in the app
straight from your terminal, GitX-style. The app accepts a directory as its
first argument and opens the enclosing git repository.

```sh
gitrx            # open the current directory (same as `gitrx .`)
gitrx .          # open the current directory
gitrx ../other   # relative paths are resolved against your shell's cwd
gitrx ~/code/app # ~ is expanded by your shell, not gitrx
gitrx --help     # show usage
```

The path may be a repository root or any subdirectory of one; the app walks up
to the enclosing working tree. The app runs as a single instance with one window
per repository: invoking `gitrx <path>` while it is already running forwards the
path to the running app — focusing the existing window for that repo, or opening
a new one — rather than spawning a second Dock icon. Closing and reopening the
app restores all previously open repo windows (geometry included), and the
Window menu lists them.

### Install

The easy path: from the running app, choose **gitrx ▸ Install Command Line
Tool…** in the menu bar. It writes `gitrx` to the first writable of
`/opt/homebrew/bin` or `/usr/local/bin`, prompting once for administrator
rights only if neither is writable, then confirms with a dialog. Cancelling the
authorization prompt does nothing.

The manual path, from a checkout of this repo:

```sh
scripts/install-cli.sh              # copy gitrx onto your PATH
scripts/install-cli.sh --uninstall  # remove it
```

Both approaches install the same launcher into the first writable of
`/opt/homebrew/bin` or `/usr/local/bin` (falling back to an elevated write to
`/usr/local/bin` if neither is writable). The script is copied rather than
symlinked, so the launcher keeps working even if this project directory moves.
The in-app installer embeds `gitrx` at build time, so it works from an installed
`.app` with no checkout present.

### App discovery

`gitrx` finds the app binary in this order (first match wins):

1. `$GITRX_APP` — override pointing at a `.app` bundle **or** a direct binary
2. `/Applications/gitrx.app`
3. `~/Applications/gitrx.app`
4. `/Applications/rust-gitx.app` (legacy — a pre-rename install still launches)
5. `~/Applications/rust-gitx.app` (legacy)
6. the local dev build under
   `src-tauri/target/release/bundle/macos/gitrx.app` — only when the running
   `gitrx` script still lives in this project's `scripts/` directory

If none is found, `gitrx` tells you to run `bun run tauri build` or set
`GITRX_APP`.
