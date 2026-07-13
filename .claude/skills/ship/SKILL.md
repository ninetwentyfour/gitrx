---
name: ship
description: Release-build gitrx.app, install it to /Applications, and refresh the installed gitrx CLI. Use after changes are verified when the user wants the new build live.
disable-model-invocation: true
---

Ship the current tree as the installed app. Steps, one command at a time:

1. Run the full verification gate first (see /verify-all). Do not ship red.
2. Build: `export PATH="$HOME/.cargo/bin:$PATH" && bunx tauri build --bundles app` (run in background; takes minutes). NOT `bun run tauri build -- --bundles app` — the `--` breaks arg forwarding.
3. Confirm the artifact exists: `src-tauri/target/release/bundle/macos/gitrx.app`.
4. Retire the installed copy recoverably (never `rm -rf`):
   `mv /Applications/gitrx.app ~/.Trash/gitrx-old-$(date +%s).app`
   (Skip if not installed.)
5. Install: `cp -R src-tauri/target/release/bundle/macos/gitrx.app /Applications/`
6. If `/opt/homebrew/bin/gitrx` or `/usr/local/bin/gitrx` exists and `scripts/gitrx` changed, refresh it: `cp scripts/gitrx <installed path> && chmod +x <installed path>`.
7. Do NOT launch the app to verify (hard rule — see CLAUDE.md). Tell the user it's installed and remind them to quit running instances before relaunching.
