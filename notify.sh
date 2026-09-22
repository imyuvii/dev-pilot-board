#!/bin/bash
# Claude Code notification hook — distinct sound + macOS banner per event type,
# appends every event to ~/.claude/notify-state.jsonl for the Claude Notify app,
# and honors user preferences from ~/.claude/notify-config.json (written by the
# app's Settings panel): master mute, quiet hours, per-event sound/banner/tone.
# Usage: notify.sh <event> [source]     Receives hook JSON on stdin.
#   source: claude (default) | copilot — which AI tool emitted the event
#
# Sound events:  stop | waiting | question | failure | task-done | compact | session-start
# Silent events: working | session-end   (state log only, for the desktop app)
#
# Sound precedence: CLAUDE_NOTIFY_SOUND_<EVENT> env var > config file > built-in default.

EVENT="${1:-stop}"
SOURCE="${2:-claude}"
INPUT=$(cat)

CWD=$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
# Claude Code sends session_id; Copilot CLI sends sessionId
SESSION=$(printf '%s' "$INPUT" | jq -r '.session_id // .sessionId // empty' 2>/dev/null)
DETAIL=$(printf '%s' "$INPUT" | jq -r '.message // empty' 2>/dev/null | tr -d '"\\' | head -c 200)
PROJECT=$(basename "${CWD:-$PWD}" | tr -cd 'A-Za-z0-9 ._-')
PROJECT="${PROJECT:-this project}"

# ---- State log for the desktop app -----------------------------------------
STATE_FILE="$HOME/.claude/notify-state.jsonl"
if command -v jq >/dev/null 2>&1; then
  jq -cn \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg event "$EVENT" \
    --arg project "$PROJECT" \
    --arg cwd "${CWD:-$PWD}" \
    --arg session "$SESSION" \
    --arg msg "$DETAIL" \
    --arg source "$SOURCE" \
    '{ts:$ts, event:$event, project:$project, cwd:$cwd, session:$session, msg:$msg, source:$source}' \
    >> "$STATE_FILE" 2>/dev/null

  # Occasionally trim the log so it never grows unbounded
  if [ $((RANDOM % 25)) -eq 0 ] && [ "$(wc -l < "$STATE_FILE" 2>/dev/null || echo 0)" -gt 2000 ]; then
    tail -n 1000 "$STATE_FILE" > "$STATE_FILE.tmp" && mv "$STATE_FILE.tmp" "$STATE_FILE"
  fi
fi

# ---- Built-in defaults per event -------------------------------------------
BANNER=1
case "$EVENT" in
  stop)          SOUND=Glass;     MSG="Done responding in $PROJECT" ;;
  waiting)       SOUND=Ping;      MSG="${DETAIL:-Waiting for your input} — $PROJECT" ;;
  question)      SOUND=Hero;      MSG="Claude has a question in $PROJECT" ;;
  failure)       SOUND=Basso;     MSG="A tool call failed in $PROJECT" ;;
  task-done)     SOUND=Submarine; MSG="Background task finished in $PROJECT" ;;
  compact)       SOUND=Purr;      MSG="Compacting context in $PROJECT"; BANNER=0 ;;
  session-start) SOUND=Pop;       MSG="Session started: $PROJECT";     BANNER=0 ;;
  working|session-end)
    # State-log-only events for the desktop app: no sound, no banner
    exit 0
    ;;
  *)             SOUND=Glass;     MSG="$EVENT in $PROJECT" ;;
esac
SOUND_ON=1

# ---- User preferences from the app's Settings panel ------------------------
CONFIG="$HOME/.claude/notify-config.json"
if [ -f "$CONFIG" ] && command -v jq >/dev/null 2>&1; then
  # Master mute: log-only, nothing audible or visible
  [ "$(jq -r '.master_mute // false' "$CONFIG" 2>/dev/null)" = "true" ] && exit 0

  # Note: "// d" would swallow a stored `false`, so null-check explicitly
  CFG=$(jq -r --arg e "$EVENT" \
    '[.events[$e].sound_enabled, .events[$e].banner_enabled, .events[$e].sound]
     | map(if . == null then "d" else tostring end) | join("|")' \
    "$CONFIG" 2>/dev/null)
  IFS='|' read -r CFG_SOUND_ON CFG_BANNER_ON CFG_SOUND <<< "$CFG"
  [ "$CFG_SOUND_ON" = "false" ] && SOUND_ON=0
  [ "$CFG_SOUND_ON" = "true" ] && SOUND_ON=1
  [ "$CFG_BANNER_ON" = "false" ] && BANNER=0
  [ "$CFG_BANNER_ON" = "true" ] && BANNER=1
  [ "$CFG_SOUND" != "d" ] && [ -n "$CFG_SOUND" ] && SOUND="$CFG_SOUND"

  # Quiet hours: suppress sounds only; banners stay as configured
  if [ "$(jq -r '.quiet_hours.enabled // false' "$CONFIG" 2>/dev/null)" = "true" ]; then
    QH_START=$(jq -r '.quiet_hours.start // "22:00"' "$CONFIG" 2>/dev/null)
    QH_END=$(jq -r '.quiet_hours.end // "08:00"' "$CONFIG" 2>/dev/null)
    NOW_M=$((10#$(date +%H) * 60 + 10#$(date +%M)))
    START_M=$((10#${QH_START%%:*} * 60 + 10#${QH_START##*:}))
    END_M=$((10#${QH_END%%:*} * 60 + 10#${QH_END##*:}))
    if [ "$START_M" -le "$END_M" ]; then
      [ "$NOW_M" -ge "$START_M" ] && [ "$NOW_M" -lt "$END_M" ] && SOUND_ON=0
    else
      # Overnight range, e.g. 22:00 -> 08:00
      { [ "$NOW_M" -ge "$START_M" ] || [ "$NOW_M" -lt "$END_M" ]; } && SOUND_ON=0
    fi
  fi
fi

# ---- Deliver ----------------------------------------------------------------
OVERRIDE_VAR="CLAUDE_NOTIFY_SOUND_$(printf '%s' "$EVENT" | tr 'a-z-' 'A-Z_')"
SOUND="${!OVERRIDE_VAR:-$SOUND}"
[[ "$SOUND" != /* ]] && SOUND="/System/Library/Sounds/${SOUND}.aiff"

if [ "$SOUND_ON" -eq 1 ]; then
  afplay "$SOUND" >/dev/null 2>&1 &
fi

TITLE="Claude Code"; EMOJI="✳️"; ICON="$HOME/.claude/notify-icons/claude.png"
if [ "$SOURCE" = "copilot" ]; then
  TITLE="GitHub Copilot"; EMOJI="🤖"; ICON="$HOME/.claude/notify-icons/copilot.png"
fi

if [ "$BANNER" -eq 1 ] && [ -n "$MSG" ]; then
  if command -v terminal-notifier >/dev/null 2>&1 && [ -f "$ICON" ]; then
    # Logo on the banner via contentImage; -group collapses stale banners per session
    terminal-notifier -title "$EMOJI $TITLE" -message "$MSG" \
      -contentImage "$ICON" -group "claude-notify-${SESSION:-$PROJECT}" >/dev/null 2>&1 &
  else
    osascript -e "display notification \"$MSG\" with title \"$EMOJI $TITLE\"" >/dev/null 2>&1
  fi
fi

exit 0
