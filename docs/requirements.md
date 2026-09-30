# Dev Pilot Board — Requirements

## 1. Overview

Dev Pilot Board is a cross-platform desktop status board for AI coding agents. It watches
Claude Code and GitHub Copilot CLI sessions on the local machine and tells the developer —
by sound, native notification banner, menu-bar glyph, and a dashboard window — what each
agent is doing and, more importantly, when one is blocked waiting for human input.

**Problem it solves:** agentic CLI sessions run for minutes at a time. Developers switch
away, miss the moment an agent finishes or asks a question, and lose that time. A missed
permission prompt can silently stall a session for an hour.

## 2. Architecture

```
Claude Code hooks ─┐
                   ├─> notify.sh ──┬─> sound (afplay) + banner (terminal-notifier/osascript)
Copilot CLI hooks ─┘               └─> ~/.claude/notify-state.jsonl (append-only JSONL)
                                                     │ polled every 2s
                                        Tauri app (Vue 3 frontend, Rust shell)
                                          ├─ menu-bar tray icon + status glyph
                                          ├─ dashboard window (sessions + history)
                                          ├─ LED ring worker ──> USB serial | WebSocket :81
                                          │                        (optional hardware)
                                          └─ settings panel ──> ~/.claude/notify-config.json
                                                                       │ read on every event
                                                                  notify.sh (loop closes)
```

Design rule: the contract between the agents and the app is **files, not IPC**. Hook
scripts append JSON lines to a state file; the app polls it. The app writes a config file;
the hook script reads it. Either side works when the other is absent.

## 3. Functional requirements

### 3.1 Event capture

- **FR-1** Capture Claude Code lifecycle events via its hooks system (`~/.claude/settings.json`):
  Stop, Notification, PreToolUse (AskUserQuestion), PostToolUseFailure, TaskCompleted,
  PreCompact, SessionStart, UserPromptSubmit, SessionEnd.
- **FR-2** Capture GitHub Copilot CLI lifecycle events via its hooks system
  (`~/.copilot/hooks/notify.json`): sessionStart, sessionEnd, userPromptSubmitted,
  agentStop, postToolUseFailure, preCompact, notification.
- **FR-3** Normalize both tools' events into a shared event vocabulary:
  `stop`, `waiting`, `question`, `failure`, `task-done`, `compact`, `session-start`,
  `working` (silent), `session-end` (silent). Tag every entry with its `source`
  (`claude` | `copilot`).
- **FR-4** Handle both tools' payload field conventions (`session_id` vs `sessionId`).

### 3.2 Notification delivery (notify.sh)

- **FR-5** Play a distinct, per-event system sound; each event has a built-in default tone.
- **FR-6** Show a native banner with the project name and, where available, the actual
  message (e.g. the permission prompt text). Banner title and logo identify the source
  tool (✳️ Claude Code / 🤖 GitHub Copilot).
- **FR-7** Prefer `terminal-notifier` (per-notification logo image, per-session banner
  grouping); fall back to `osascript` when it is not installed. Degrade gracefully:
  missing config file, missing jq, or missing icons must never break a hook.
- **FR-8** Honor user preferences on every event without restart (config re-read per event).

### 3.3 State log

- **FR-9** Append every event as one JSON line to `~/.claude/notify-state.jsonl`:
  `{ts, event, project, cwd, session, msg, source}` (UTC ISO-8601 timestamps).
- **FR-10** Self-trim: probabilistically truncate the log to its most recent 1000 lines
  once it exceeds 2000 so it never grows unbounded.

### 3.4 Dashboard app

- **FR-11** Menu-bar-only presence: tray icon, no Dock icon (macOS accessory policy).
  Left-click toggles the dashboard window; right-click offers Open Dashboard / Quit.
  Closing the window hides it; the app keeps running.
- **FR-12** Sessions view: one row per active session showing status
  (❓ question / 🟡 waiting / 🟢 working / ⚪ done-idle), project name, source badge
  (Claude / Copilot), detail message, and time since last event. Sessions idle for more
  than 12 hours are hidden; `session-end` removes a session.
- **FR-13** Recent-events feed (last 25, `working` events excluded).
- **FR-14** Tray status glyph reflecting the most urgent state across all sessions:
  `?` question > `!` waiting > `…` working > (none) idle.
- **FR-15** Poll the state file every 2 seconds; malformed lines are skipped, and a
  missing state file is not an error.

### 3.5 Settings

- **FR-16** Per-source mute: "Mute Claude notifications" and "Mute Copilot notifications"
  as independent switches (muted events are still logged for the dashboard).
- **FR-17** Quiet hours: a configurable time range (overnight ranges supported) during
  which sounds are suppressed; banners remain.
- **FR-18** Per-event customization: sound on/off, banner on/off, and tone selection from
  the 14 macOS system sounds, with an in-app preview button.
- **FR-19** Settings persist to `~/.claude/notify-config.json`, saved automatically
  (debounced), applied by notify.sh on the very next event. Config is merged over
  defaults so partial or legacy files stay valid (legacy `master_mute` maps to both
  per-source mutes).
- **FR-20** Sound precedence: `CLAUDE_NOTIFY_SOUND_<EVENT>` env var > config file >
  built-in default.
- **FR-20a** Per-event throttling: an event may declare a cooldown so a burst collapses
  into a single alert per session (leading edge — the first event in the window rings,
  the rest are silent). `failure` defaults to 300s because the host fires it on *every*
  failed tool call, subagents included, and routine failures (blocked WebFetch, non-zero
  grep exits) can produce dozens per session. Override with
  `.events.<event>.throttle_seconds`; `0` disables. Throttled events are still written to
  the state log, so the dashboard shows every occurrence.

### 3.6 LED ring output (optional hardware)

An external 16-LED WS2812B ring on an ESP32 (the `led-iot` project) can mirror the board's
busiest session as one colour and one pattern.

- **FR-20b** Both links are supported, as in LED Lab: USB serial at 115200 baud, or a
  WebSocket on port 81 (`ledring.local`). `transport` selects `auto` | `wifi` | `usb`.
  The host is found without relying on the Mac's resolver: getaddrinfo, then the app's own
  mDNS query, then the last known address, then a sweep of the local /24 for a device that
  answers the ring protocol — so a machine whose `.local` lookups are broken still connects.
  Settings → LED offers **Find my ring**, which runs every step on demand and shows what
  each one found (or why nothing did, including the macOS Local Network permission hint).
  `auto` prefers WiFi and falls back to serial, then re-checks WiFi every 15s and releases
  the serial port when the ring reappears on the network — only one process can hold a
  serial port, and this app must not lock out LED Lab or `arduino-cli upload`.
- **FR-20c** Per-event configuration, like sound and banner: each event has LED on/off,
  a colour and a pattern from the firmware's effect list. Defaults, most urgent first
  (this order is fixed and decides what wins across sessions): question (magenta
  chase) > waiting (blue chase, off by default) > failure (red fire) > task-done (cyan
  juggle, off by default) > compact (purple breathe, off by default) > session-start
  (white chase, off by default) > working (blue chase — the "agent is busy" colour) >
  stop / done responding (green breathe). Default brightness is 5. One-off events
  (failure, task-done, compact, session-start, stop) show for 30s; an agent waiting /
  asking a question stays lit until the user responds; with nothing to show the ring is
  dark. Disabling an event makes the ring fall through to the next rung.
- **FR-20d** The app sends only a pattern name from the firmware's existing effect list
  plus colours, brightness and speed. Adding an agent source or event is a change to the
  mapping in `led.rs`; the firmware is never reflashed for it. The decision runs in the
  Rust shell, not the webview, so it keeps working while the dashboard window is hidden.
- **FR-20e** Brightness is capped at 160 (the firmware's 5V/700mA budget covers the ring
  and the WiFi radio from one USB supply) and dims to 10 during quiet hours when
  `dim_in_quiet_hours` is set — the ring dims rather than blanking, because a dark ring is
  indistinguishable from an unplugged one.
- **FR-20f** Drift self-heals: the worker asks the ring for `STATE?` every 5s and
  re-asserts on a mismatch, so a ring that rebooted onto firmware defaults or was changed
  by another client returns to the real status. A status light showing a stale colour is
  worse than one that is off.
- **FR-20h** Brownout guard: three reconnects within 90s is treated as the ring rebooting
  under load; the app halves brightness (to a floor) instead of re-applying the value
  that caused it, reports "brightness reduced (check its power)" in Settings, and
  restores full brightness after five stable minutes.
- **FR-20f2** Several machines may share one ring. An app with nothing to show never
  blanks a ring another machine lit; an app with something to show yields to an equal or
  more urgent status already on the ring and overrides a dark ring, a less urgent status,
  or anything it does not recognise (firmware defaults, LED Lab). Settings reports
  "showing another Mac's status" while yielding.
- **FR-20g** Strictly optional, and off by default. `notify.sh` has no knowledge of the
  ring. With `led.enabled` false nothing probes mDNS and no serial port is opened. No
  board, no network, or a port held by another app are all silent retries with backoff;
  none of it may produce an error or affect sounds, banners or the dashboard.

### 3.7 Onboarding & distribution

- **FR-21** In-app hook setup: when no notify.sh hooks are detected in
  `~/.claude/settings.json`, the dashboard shows a "Connect your agents" card;
  one click installs `~/.claude/notify.sh` (embedded in the binary), merges the
  Claude Code hook events into `settings.json` (timestamped backup first),
  writes the Copilot CLI hook file, and installs embedded banner icons —
  offline, no companion script.
- **FR-22** The same setup is reachable headless via the `--setup-hooks` CLI
  flag (used by packaging tests and power users).
- **FR-23** Distribution channels: a Homebrew cask (`imyuvii/tap/dev-pilot-board`,
  postflight clears the quarantine flag of the unsigned build) and a GitHub
  release zip whose `install.sh` installs the app, clears quarantine, and sets
  up hooks in one command.

## 4. Non-functional requirements

- **NFR-1** Footprint: resident memory well under Electron-class usage (Tauri; target
  < 100 MB), binary < 20 MB.
- **NFR-2** Resilience: hook execution must never block or fail an agent session
  (hooks are async, short timeouts, all failure paths exit 0).
- **NFR-3** Privacy: everything is local — no network calls at runtime; state and config
  stay in the user's home directory.
- **NFR-4** The app is optional: sounds and banners work with the shell script alone, and
  the LED ring is optional in the same way one layer further out — the app is fully
  functional with no ring attached, and off by default.
- **NFR-5** Cross-platform-ready: the app builds for macOS, Windows, and Linux; the
  delivery layer (`afplay`/`osascript`) is the only macOS-specific component and is
  isolated inside notify.sh.

## 5. Out of scope (current version)

- Windows/Linux notification delivery scripts
- Apple code signing / notarization (unsigned test builds; installers and the
  cask postflight clear the quarantine flag instead)
- Click-to-focus the originating terminal window
- Auto-start at login
- Per-project notification rules and snooze
- Duration-based suppression ("only notify if the task ran > N seconds")
- Waiting-too-long escalation / mobile push
- Code signing & notarization (friend-testing uses the quarantine workaround)

## 6. File inventory

| Path | Role |
|---|---|
| `notify.sh` | Hook target for both tools; delivery + state logging |
| `app/` | Tauri 2 + Vue 3 desktop app |
| `app/src/App.vue` | Dashboard (sessions, history, tray glyph updates) |
| `app/src/Settings.vue` | Settings panel (writes notify-config.json) |
| `app/src-tauri/src/lib.rs` | Tray, window lifecycle, `set_tray_title`, `play_sound` |
| `app/src/led.ts` | LED config types for the settings panel |
| `app/src-tauri/src/led.rs` | LED pipeline: log + config -> status -> colour/pattern -> USB serial / WebSocket; `led_status` |
| `app/assets/icon.svg`, `tray.svg` | Icon sources (rendered via sharp, `tauri icon`) |
| `~/.claude/notify-state.jsonl` | Event log (runtime, not in repo) |
| `~/.claude/notify-config.json` | User preferences (runtime, not in repo) |
| `~/.claude/notify-led-cache.json` | Last address the LED ring answered on (runtime, written by the app) |
| `~/.claude/notify-icons/*.png` | Banner logos (runtime, not in repo) |
| `~/.copilot/hooks/notify.json` | Copilot CLI hook registration (runtime, not in repo) |
