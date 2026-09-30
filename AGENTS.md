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
- LED ring (optional hardware, see [`led-iot`](../led-iot)): `src-tauri/src/led.rs` owns
  the whole pipeline — reads the state log + config, maps status → colour/pattern,
  drives USB serial or WebSocket, exposes `led_status`. `src/led.ts` is only the config
  types for Settings.vue.

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
- **The LED pipeline lives entirely in Rust, never the webview.** Two reasons, both hit
  for real: (1) this is a menu-bar app whose window is hidden nearly always, and WebKit
  suspends timers in a hidden webview — a frontend-driven version pushed exactly one
  update after launch and then went silent; (2) `tauri://localhost` is a secure context,
  so plain `ws://` from JS is mixed content, and the ring can't do `wss://`. The
  status ladder (`STATUS_MAP`, ranking, 12h stale cutoff) is therefore **duplicated** in
  `led.rs` (`LADDER`, `standing_of`) and pinned by its unit tests, and the per-event
  labels/defaults are duplicated once more in `led.ts` (`LED_EVENTS`) for the settings
  panel — change one and change all three.
- **The ring browns out on USB power above roughly brightness 80–100.** Reproduced with
  the app disabled: `BRIGHT 140` applied, then the board dropped into a continuous reset
  loop (`rst:ets Jul 29 2019` repeating on serial) that only a power cycle clears. The
  firmware's 700mA FastLED budget assumes a supply that a thin cable/weak port doesn't
  deliver. Symptom as a user sees it: "brightness does nothing" — the board reboots to
  its rainbow@40 default. `led.rs` backs brightness off after 3 reconnects in 90s so the
  app never holds the board in that loop; Settings says so when it happens. Seen at
  brightness 30 too — the boot sequence alone (rainbow@40 + WiFi bring-up) trips a
  marginal supply, so once looping only a power cycle (ideally a better port/cable)
  fixes it. Diagnose over serial at 115200 with the baud set on the open fd (`stty -f`
  on `/dev/cu.*` does not survive a separate `open()`): repeating `mmu set` / `rst:`
  lines = boot loop.
- **Opening the USB serial port resets an ESP32** (DTR/RTS → auto-reset). `connect_usb`
  uses `preserve_dtr_on_open()` so the app's periodic probe doesn't reboot the ring;
  without it, `auto` mode with WiFi down would keep kicking a recovering board.
- **`MODE` restarts the effect** (the firmware zeroes its step counter and blanks the
  ring), so `emit()` diffs against the last sent state. Re-sending it every loop freezes
  every animation on frame one.
- **A status light must never show a stale colour.** `led.rs` sends `STATE?` every 5s and
  compares the reply, because the ring drifts for reasons no write error reveals: it
  reboots onto firmware defaults, or LED Lab changes the mode underneath. Consequence to
  know: with LED output enabled, Dev Pilot Board wins any tug-of-war with LED Lab within
  ~5s — turn the toggle off to play in LED Lab.

## Conventions

- State file schema: `{ts, event, project, cwd, session, msg, source}` — one JSON object
  per line, UTC ISO-8601 `ts`. New fields are fine; removing/renaming breaks the app's
  parser and old logs.
- Event vocabulary: `stop | waiting | question | failure | task-done | compact |
  session-start | working | session-end`. `working`/`session-end` are silent (log-only).
- Config lives at `~/.claude/notify-config.json`; the app writes it (debounced), the
  script reads it per event. Keep both sides backward compatible with older config keys
  (e.g. legacy `master_mute`). Settings.vue spreads stored per-event keys over its
  defaults, so script-only keys like `throttle_seconds` survive the app's save round-trip
  without needing UI.
- **Throttling:** events may set a per-session cooldown; `failure` defaults to 300s
  (`PostToolUseFailure` fires on every failed tool call, subagents included). Stamps live
  in `~/.claude/notify-throttle/<event>-<session>` and are pruned after a day. Throttling
  suppresses only sound/banner — the state log always gets the event.
- The app must stay optional: sounds/banners work with the script alone, and the app
  must not error when the state file is missing.
- **The LED ring is optional in the same way**, one layer further out: `notify.sh` knows
  nothing about it, and with `led.enabled` false (the default) the worker never resolves
  mDNS and never opens a serial port. No ring, no network, a port held by another app —
  every path is a silent retry with backoff. Nothing about the ring may surface an error
  or affect sounds, banners or the dashboard.
- **LED config** lives under `led` in the same config file:
  `{enabled, transport: auto|wifi|usb, host, brightness, dim_in_quiet_hours,
  events: {<event>: {enabled, color, pattern}}}` — per-event on/off, colour and pattern,
  the same shape of control as sound/banner. Events: `question waiting failure task-done
  compact session-start working stop`; `compact` and `session-start` default off like
  their banners, and `working` defaults off because it is a silent status event, not a
  notification — the ring is dark unless something happened in the last 30s or an agent
  is waiting on / asking the user (those stay lit until answered). Settings.vue carries unknown top-level keys through its save round-trip,
  so a script-only key added next to `led` survives.
- **Firmware is never modified from this repo.** The ring speaks one line protocol over
  both transports (`MODE`/`COLOR`/`COLOR2`/`SPEED`/`BRIGHT`), and `led.rs` only ever sends
  a pattern from the firmware's own effect list. Adding an agent source or an event is a
  change to `LADDER`/`standing_of` in `led.rs` plus `LED_EVENTS` in `led.ts` — never a
  reflash. An unknown pattern name just earns
  an `ERR unknown mode` and leaves the ring as it was.
- **Transport preference is WiFi first, USB as fallback.** Only one process can hold a
  serial port and this app runs all day, so squatting on `/dev/cu.usbserial-*` would lock
  out LED Lab, the Arduino IDE and `arduino-cli upload`. In `auto` the worker re-checks
  WiFi every 15s while on serial and hands the port back when the ring appears on the
  network.

## Testing the LED path

`DPB_LED_DEBUG=1` makes `led.rs` log target changes, emits, drift and brownout backoff
to stderr — otherwise the whole path is deliberately silent. `cargo test --lib` in
`src-tauri` runs the ladder tests without hardware. Launch the binary directly to see it:

```bash
DPB_LED_DEBUG=1 "…/bundle/macos/Dev Pilot Board.app/Contents/MacOS/app" 2>&1 | grep led
```

Verify against the hardware, not the log: ask the ring what it is actually showing over
the *other* transport. Any WebSocket client works; `STATE?` replies with the live mode,
brightness, speed and colours. Two traps found the hard way — the `STATE` reply exceeds
125 bytes so a hand-rolled client must handle 2-byte extended frame lengths, and Node's
DNS does not resolve `ledring.local` (use the IP; Rust's `getaddrinfo` path is fine).

## Release process

`scripts/build-release.sh` (or the `/build-release` skill) does steps 2–3 and prints the
zip path + sha256; it refuses to build if the three version files disagree.

1. Bump the version in `app/src-tauri/tauri.conf.json`, `app/package.json`, and `app/src-tauri/Cargo.toml`.
2. `scripts/build-release.sh` — universal build (`--target universal-apple-darwin`, verified with `lipo -archs`).
3. Package: `dist/dev-pilot-board-<v>/` = the `.app` (via `ditto`) + `notify.sh` + `scripts/{install.sh,uninstall.sh,INSTALL.md}`; zipped with `ditto -c -k --norsrc --keepParent`. The folder name inside the zip must match the cask's `app` stanza path.
4. `git tag v<v>`, push, `gh release create v<v> <zip> ...` (absolute asset path — a relative one has failed before).
5. Update `Casks/dev-pilot-board.rb` in the `imyuvii/homebrew-tap` repo: bump `version`, set `sha256` from the script output, push.
6. Version rule: packaging/docs-only fixes may replace the release asset in place (`gh release upload --clobber`); any app or notify.sh behavior change gets a version bump.
