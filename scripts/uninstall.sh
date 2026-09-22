#!/bin/bash
# Dev Pilot Board — removes everything install.sh set up.
set -euo pipefail

CLAUDE_DIR="$HOME/.claude"
SETTINGS="$CLAUDE_DIR/settings.json"

echo "── Dev Pilot Board uninstaller ──"

# Strip only OUR hook entries (any whose command references notify.sh)
if [ -f "$SETTINGS" ] && command -v jq >/dev/null 2>&1; then
  cp "$SETTINGS" "$SETTINGS.backup.$(date +%Y%m%d%H%M%S)"
  jq '
    if .hooks then
      .hooks |= with_entries(
        .value |= map(.hooks |= map(select((.command // "") | contains("notify.sh") | not)))
        | .value |= map(select(.hooks | length > 0))
      )
      | .hooks |= with_entries(select(.value | length > 0))
    else . end
  ' "$SETTINGS" > "$SETTINGS.tmp" && mv "$SETTINGS.tmp" "$SETTINGS"
  echo "✓ Removed hooks from $SETTINGS (backup kept)"
fi

rm -f "$CLAUDE_DIR/notify.sh" \
      "$HOME/.copilot/hooks/notify.json" \
      "$CLAUDE_DIR/notify-state.jsonl" \
      "$CLAUDE_DIR/notify-config.json"
rm -rf "$CLAUDE_DIR/notify-icons"
echo "✓ Removed notify.sh, state, config, icons, and Copilot hooks"

echo "ℹ Drag 'Dev Pilot Board.app' from /Applications to the Trash to finish."
