---
name: verify-all
description: Run gitrx's full verification gate — Rust and frontend suites in order. Use after any code change, before declaring work done or shipping.
---

Run each command separately (never chained into one line), all must pass:

1. `export PATH="$HOME/.cargo/bin:$PATH"; cd src-tauri && cargo build` — must finish with ZERO warnings, not just success.
2. `cargo clippy --all-targets -- -D warnings` (from src-tauri) — must exit 0; the aggressive all/pedantic/nursery policy lives in `Cargo.toml` `[lints]`, so any new warning fails the gate.
3. `cargo test` (from src-tauri) — lib + integration suites all green.
4. `cargo fmt --check` (from src-tauri).
5. `bun run typecheck` (tsc 7)
6. `bun run lint` (oxlint)
7. `bun run test` (vitest)
8. `bun run build` (rolldown-vite; the >500 kB chunk advisory for shiki grammars is pre-existing and acceptable)
9. `bun run fmt:check` (oxfmt)

Report pass/fail per step with test counts. Any failure: stop, report, fix before re-running. NEVER "verify" by launching the app, a dev server, or a built binary (see CLAUDE.md hard rules).
