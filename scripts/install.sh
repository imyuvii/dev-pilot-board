#!/bin/bash
# Dev Pilot Board — hook-side installer for testers.
# Sets up notify.sh, Claude Code + Copilot CLI hooks, and banner icons.
# Safe to re-run; backs up ~/.claude/settings.json before touching it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLAUDE_DIR="$HOME/.claude"
NOTIFY_SH="$CLAUDE_DIR/notify.sh"
SETTINGS="$CLAUDE_DIR/settings.json"

echo "── Dev Pilot Board installer ──"

# ── prerequisites ────────────────────────────────────────────────────────────
if ! command -v jq >/dev/null 2>&1; then
  echo "✗ jq is required. Install it first:  brew install jq"
  exit 1
fi
if [ ! -f "$SCRIPT_DIR/notify.sh" ]; then
  echo "✗ notify.sh not found next to this installer."
  exit 1
fi

# ── the app itself ───────────────────────────────────────────────────────────
# Copy to /Applications and clear the quarantine flag so Gatekeeper never
# shows the "could not verify" dialog for this unsigned test build.
if [ -d "$SCRIPT_DIR/Dev Pilot Board.app" ]; then
  rm -rf "/Applications/Dev Pilot Board.app"
  cp -R "$SCRIPT_DIR/Dev Pilot Board.app" /Applications/
  xattr -dr com.apple.quarantine "/Applications/Dev Pilot Board.app" 2>/dev/null || true
  echo "✓ Installed Dev Pilot Board.app to /Applications (quarantine cleared)"
else
  echo "ℹ App bundle not found next to installer — skipping app install."
fi

# ── notify.sh ────────────────────────────────────────────────────────────────
mkdir -p "$CLAUDE_DIR"
cp "$SCRIPT_DIR/notify.sh" "$NOTIFY_SH"
chmod +x "$NOTIFY_SH"
echo "✓ Installed $NOTIFY_SH"

# ── banner icons (best effort) ───────────────────────────────────────────────
mkdir -p "$CLAUDE_DIR/notify-icons"
curl -sL --max-time 10 "https://www.google.com/s2/favicons?domain=claude.ai&sz=128" \
  -o "$CLAUDE_DIR/notify-icons/claude.png" 2>/dev/null || true
curl -sL --max-time 10 "https://www.google.com/s2/favicons?domain=github.com&sz=128" \
  -o "$CLAUDE_DIR/notify-icons/copilot.png" 2>/dev/null || true
echo "✓ Banner icons in $CLAUDE_DIR/notify-icons"

# ── Claude Code hooks ────────────────────────────────────────────────────────
if [ -f "$SETTINGS" ]; then
  cp "$SETTINGS" "$SETTINGS.backup.$(date +%Y%m%d%H%M%S)"
  echo "✓ Backed up existing settings.json"
else
  echo '{}' > "$SETTINGS"
fi

jq --arg sh "$NOTIFY_SH" '
  def hook($cmd): [{"hooks": [{"type": "command", "command": $cmd, "async": true, "timeout": 10}]}];
  .hooks = (.hooks // {})
  | .hooks.Stop               = hook($sh + " stop")
  | .hooks.Notification       = hook($sh + " waiting")
  | .hooks.PreToolUse         = [{"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": ($sh + " question"), "async": true, "timeout": 10}]}]
  | .hooks.PostToolUseFailure = hook($sh + " failure")
  | .hooks.TaskCompleted      = hook($sh + " task-done")
  | .hooks.PreCompact         = hook($sh + " compact")
  | .hooks.SessionStart       = hook($sh + " session-start")
  | .hooks.UserPromptSubmit   = hook($sh + " working")
  | .hooks.SessionEnd         = hook($sh + " session-end")
' "$SETTINGS" > "$SETTINGS.tmp" && mv "$SETTINGS.tmp" "$SETTINGS"
echo "✓ Claude Code hooks registered in $SETTINGS"

# ── Copilot CLI hooks ────────────────────────────────────────────────────────
mkdir -p "$HOME/.copilot/hooks"
jq -n --arg sh "$NOTIFY_SH" '
  def hook($cmd): [{"type": "command", "bash": $cmd, "timeoutSec": 10}];
  {
    "version": 1,
    "hooks": {
      "sessionStart":        hook($sh + " session-start copilot"),
      "sessionEnd":          hook($sh + " session-end copilot"),
      "userPromptSubmitted": hook($sh + " working copilot"),
      "agentStop":           hook($sh + " stop copilot"),
      "postToolUseFailure":  hook($sh + " failure copilot"),
      "preCompact":          hook($sh + " compact copilot"),
      "notification":        hook($sh + " waiting copilot")
    }
  }
' > "$HOME/.copilot/hooks/notify.json"
echo "✓ Copilot CLI hooks written to ~/.copilot/hooks/notify.json"

# ── optional niceties ────────────────────────────────────────────────────────
if ! command -v terminal-notifier >/dev/null 2>&1; then
  echo "ℹ Optional: 'brew install terminal-notifier' gives banners per-tool logos."
fi

echo ""
echo "── Done! ──"
if [ -d "/Applications/Dev Pilot Board.app" ]; then
  open "/Applications/Dev Pilot Board.app" 2>/dev/null || true
  echo "Dev Pilot Board is starting — look for the board glyph in your menu bar."
else
  echo "Open Dev Pilot Board.app — look for the board glyph in your menu bar."
fi
echo "Restart any running Claude Code / Copilot sessions so they load the hooks."
