# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

gitrx: a macOS git staging GUI (GitX replacement). Tauri 2.x — Rust backend (`src-tauri/`), React 19 + TS frontend (`src/`). One app instance, multiple repo windows.

## Hard rules

- **NEVER launch the app, a dev server, or built binaries** (`tauri dev`, `bun run dev`, `preview`, executing `.app`/binaries) — a stray instance once consumed 80GB RAM. Verify via test suites and builds only. Subagent briefs must repeat this prohibition.
- **Claude never commits.** The user commits themselves.
- **Delegate implementation to subagents running Opus** (`model: "opus"`); Claude orchestrates: writes briefs, reviews results, runs verification between phases.
- Never change `identifier` in `tauri.conf.json` (`com.travisberry.rust-gitx`) — it keys persisted settings/window-state. Product name is `gitrx`; internal crate names stay `rust_gitx`.

## Toolchain & commands

- **bun only** — never npm/node. Frontend: `bun run typecheck` (tsc 7, max-strict tsconfig) / `lint` (oxlint `--type-aware --max-warnings 0`, tsgolint backend — aggressive categories + type-checked rules) / `test` (vitest) / `build` (rolldown-vite) / `fmt` + `fmt:check` (oxfmt). No eslint/prettier.
- **Zero-warnings policy (frontend lint):** every oxlint tier including `style` is at `"error"`, never `"warn"`, and `--max-warnings 0` means ANY warning-level finding fails the run (no `--quiet` masking). New rules are either fixed in code or given an individually justified per-rule `"off"` in `.oxlintrc.json` — never a blanket category `"off"`.
- Rust: `export PATH="$HOME/.cargo/bin:$PATH"` first (cargo not on default PATH); run `cargo build` / `cargo test` / `cargo fmt` from `src-tauri/`.
- Release bundle: `bunx tauri build --bundles app` (NOT `bun run tauri build -- --bundles app` — the `--` forwards flags to cargo and breaks).
- Full verification gate after any change: cargo build (zero warnings) + `cargo clippy --all-targets -- -D warnings` (zero warnings, now enforced by clippy — aggressive all/pedantic/nursery lints live in `src-tauri/Cargo.toml` `[lints]`) + cargo test + bun typecheck/lint/test/build/fmt:check, all green. `/verify-all` runs it.
- Test fixture repo with every edge case: `scripts/make-fixture-repo.sh` (prints path).
- Diagnostic logs (tauri-plugin-log): `~/Library/Logs/com.travisberry.rust-gitx/gitrx.log` (rotated; frontend + backend share it; RSS watchdog line `rss_mb=` every 60s is the correlation anchor). Raise verbosity with `RUST_LOG=debug`.

## Invariants (violating these corrupts user data)

- `DiffLine.content` preserves a trailing `\r` (strip only `\n`) — CRLF hunk staging breaks otherwise. Display layers strip it for rendering only.
- Untracked files are staged via whole-file `index.add_path`, never via synthesized patches (exec-bit/symlink corruption). `stage_hunk`/`build_patch` reject untracked payloads.
- Hunk commands re-verify the payload against a fresh diff before applying (`apply_hunk_verified`) — never remove this freshness check; it's the guard against stale clicks and lying IPC payloads.
- All path inputs validated via `validate_repo_relative_path`; pathspec matching is literal (non-glob) everywhere (`disable_pathspec_match`).
- Every diff carries `DiffOptions::max_size(MAX_DIFF_BYTES)` (8 MiB); the watcher drops gitignored paths — both are memory-blowup guards.

## Architecture gotchas

- Per-window repo state: `AppState.windows: HashMap<label, WindowRepo>`; repo commands take injected `window: WebviewWindow` and resolve by label. Watcher events use `emit_to(label, …)`; frontend listens via `getCurrentWebviewWindow().listen`.
- New windows use labels `repo-<hash>`; `capabilities/default.json` must keep `"windows": ["main", "repo-*"]` or new windows are denied plugin access.
- macOS menu: install via Builder `.menu(...)` (synchronous `init_for_nsapp`), then `menu::set_windows_menu` in `setup` — calling `set_as_windows_menu_for_nsapp` during menu construction is a silent no-op. The Edit submenu is required for copy/paste in the webview.
- Persistence (`settings.json` via plugin-store): Rust-side writes need explicit `store.save()` (auto-save debounce loses to quit).
- `tauri-plugin-single-instance` must stay registered before all other plugins.
