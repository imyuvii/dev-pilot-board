# Dev Pilot Board — tester install

A menu-bar app that watches your **Claude Code** and **GitHub Copilot CLI**
sessions and pings you (sound + banner) when an agent finishes or is waiting
on you. Thanks for testing!

**Needs:** macOS, `jq` (`brew install jq`). Optional: `terminal-notifier`
(`brew install terminal-notifier`) for nicer banners with tool logos.

## Install (one command)

Unzip, then in Terminal, from the unzipped folder:

```bash
./install.sh
```

That's it. The installer:

- copies **Dev Pilot Board.app** to /Applications and clears macOS's
  quarantine flag (this is an unsigned test build — without this step
  Gatekeeper shows a *"could not verify"* warning), then launches it
- installs the notification script to `~/.claude/notify.sh`
- registers Claude Code hooks in `~/.claude/settings.json`
  (a timestamped backup is made first)
- writes Copilot CLI hooks to `~/.copilot/hooks/notify.json`
- fetches the banner icons

Then **restart any running Claude Code / Copilot sessions** — hooks load at
session start.

## Try it

Start a Claude Code session anywhere and ask it something. You should hear a
sound when it finishes, see a banner, and watch the session go 🟢 → ⚪ in the
dashboard (click the board glyph in the menu bar). The Settings tab lets you
change tones, mute either tool, and set quiet hours.

## Uninstall

```bash
./uninstall.sh
```

then trash /Applications/Dev Pilot Board.app. Your `settings.json` is
restored minus our hooks (backups of every change are kept next to it).
