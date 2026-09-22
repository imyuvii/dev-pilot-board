# Agent guide — Dev Pilot Board

Menu-bar status board for AI coding agents (Claude Code + GitHub Copilot CLI).
Read [docs/requirements.md](docs/requirements.md) for the full spec before larger changes.

## Layout

- `notify.sh` — bash hook target for BOTH tools. Usage: `notify.sh <event> [source]`
  (source: `claude` default | `copilot`). Receives hook JSON on stdin. Plays sound,
  shows banner, appends state to `~/.claude/notify-state.jsonl`, reads user prefs from
  `~/.claude/notify-config.json` on every invocation.
- `app/` — Tauri 2 + Vue 3 (TypeScript). `src/App.vue` dashboard, `src/Settings.vue`
  settings panel, `src-tauri/src/lib.rs` tray + window + `set_tray_title`/`play_sound`
  commands.

## Build & run

```bash
cd app
npm install
source "$HOME/.cargo/env"       # cargo is not on PATH in fresh shells
npm run tauri build -- --debug  # bundles .app under src-tauri/target/debug/bundle/
```

Relaunch after rebuild: `pkill -f "Dev Pilot Board.app/Contents/MacOS/app"`, then `open`
the bundle. The DMG bundling step fails if the app is running — the `.app` still builds;
check for the "Built application" line before assuming failure.

## Testing notify.sh

Pipe synthetic hook JSON:

```bash
echo '{"cwd":"/tmp/xyz-test","session_id":"t1"}' | ./notify.sh stop            # claude
echo '{"cwd":"/tmp/xyz-test","sessionId":"t2"}'  | ./notify.sh stop copilot    # copilot (camelCase!)
```

Verify suppression logic by counting side effects, not exit codes (script always exits 0):
`bash -x ./notify.sh stop 2>&1 | grep -c afplay`.

**ALWAYS clean test entries from the state file afterwards** — they show up in the user's
dashboard as fake sessions:
`grep -v '"cwd":"/tmp/xyz-test"' ~/.claude/notify-state.jsonl > tmp && mv tmp ~/.claude/notify-state.jsonl`

## Gotchas (learned the hard way)

- **jq `//` swallows `false`.** `.foo // "default"` returns "default" when foo is stored
  `false`. For tri-state config reads use explicit null checks:
  `if . == null then "d" else tostring end`. This bug shipped once already.
- **Field names differ per tool:** Claude Code sends `session_id`, Copilot CLI sends
  `sessionId`. Extract with a fallback chain.
- **Tray icons on macOS are template images** (`icon_as_template(true)`): only the alpha
  channel matters; colorful icons render as solid blobs. The tray glyph is a separate
  black-on-transparent PNG (`src-tauri/icons/tray.png` from `assets/tray.svg`).
- **Icon regeneration:** edit `app/assets/*.svg`, render PNG with sharp
  (`node -e "require('sharp')('assets/icon.svg').resize(1024,1024).png().toFile('assets/icon-1024.png')"`),
  then `npm run tauri icon assets/icon-1024.png`.
- **Hook config changes are loaded at session start** for both tools; a settings.json
  hook edit mid-session may need `/hooks` (Claude) or a CLI restart (Copilot) to fire.
- **Never break a hook:** notify.sh must exit 0 on every path; missing jq/config/icons
  degrade silently. Hooks are registered `async` with 10s timeouts — keep it that way.

## Conventions

- State file schema: `{ts, event, project, cwd, session, msg, source}` — one JSON object
  per line, UTC ISO-8601 `ts`. New fields are fine; removing/renaming breaks the app's
  parser and old logs.
- Event vocabulary: `stop | waiting | question | failure | task-done | compact |
  session-start | working | session-end`. `working`/`session-end` are silent (log-only).
- Config lives at `~/.claude/notify-config.json`; the app writes it (debounced), the
  script reads it per event. Keep both sides backward compatible with older config keys
  (e.g. legacy `master_mute`).
- The app must stay optional: sounds/banners work with the script alone, and the app
  must not error when the state file is missing.
