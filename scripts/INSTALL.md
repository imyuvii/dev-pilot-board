# Dev Pilot Board — tester install

A menu-bar app that watches your **Claude Code** and **GitHub Copilot CLI**
sessions and pings you (sound + banner) when an agent finishes or is waiting
on you. Thanks for testing!

**Needs:** macOS, `jq` (`brew install jq`). Optional: `terminal-notifier`
(`brew install terminal-notifier`) for nicer banners with tool logos.

## Install (2 minutes)

1. **The app** — open the DMG, drag *Dev Pilot Board* to Applications.
   The build is unsigned (test build), so macOS will refuse to open it until
   you clear the quarantine flag — run this once in Terminal:

   ```bash
   xattr -d com.apple.quarantine "/Applications/Dev Pilot Board.app"
   ```

2. **The hooks** — from this folder, run:

   ```bash
   ./install.sh
   ```

   This installs the notification script to `~/.claude/notify.sh`, registers
   Claude Code hooks in `~/.claude/settings.json` (a timestamped backup is
   made first), writes Copilot CLI hooks, and fetches the banner icons.

3. Open *Dev Pilot Board* — a small board glyph appears in the menu bar.
   Left-click it for the dashboard, right-click for the menu.

4. **Restart any running Claude Code / Copilot sessions** — hooks load at
   session start.

## Try it

Start a Claude Code session anywhere and ask it something. You should hear a
sound when it finishes, see a banner, and watch the session go 🟢 → ⚪ in the
dashboard. The Settings tab lets you change tones, mute either tool, and set
quiet hours.

## Uninstall

```bash
./uninstall.sh
```

then trash the app. Your `settings.json` is restored minus our hooks
(backups of every change are kept next to it).
