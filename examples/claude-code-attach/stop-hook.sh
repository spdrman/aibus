#!/bin/sh
# Claude Code Stop hook for an attached aibus session (#322, Phase 1 spike). Example only: aibus
# never installs it, you do, in your own settings (see README.md next to this file).
#
# When a turn ends and you have switched attaching on (the marker file exists), it asks Claude
# Code to keep going with one instruction: pull the next step from aibus. It decides nothing else:
# it answers no permission prompt and no elicitation, and it never reads a step itself.
#
# Bounds: it does nothing without the marker, nothing when the turn is already a continuation
# (`stop_hook_active`: true when Claude Code is
# already continuing because of a stop hook; seen live, not read from a reference, on Claude Code 2.1.311, false on the first Stop
# and true on the continuation, see docs/spikes/353-attached-claude-code.md), and at most one continuation every 20 seconds.
# Claude Code also caps consecutive stop-hook continuations at eight. The input is parsed as JSON
# (jq, else python3); with neither it does nothing rather than guess.
marker="${AIBUS_ATTACH_MARKER:-${XDG_STATE_HOME:-$HOME/.local/state}/aibus/attached}"
[ -f "$marker" ] || exit 0
input=$(cat)
if command -v jq >/dev/null 2>&1; then
  active=$(printf '%s' "$input" | jq -r '.stop_hook_active // false' 2>/dev/null) || exit 0
elif command -v python3 >/dev/null 2>&1; then
  active=$(printf '%s' "$input" | python3 -c 'import json,sys; print(str(json.load(sys.stdin).get("stop_hook_active", False)).lower())' 2>/dev/null) || exit 0
else
  exit 0
fi
[ "$active" = "false" ] || exit 0
stamp="$marker.last"
now=$(date +%s)
last=$(cat "$stamp" 2>/dev/null || echo 0)
[ $((now - last)) -ge 20 ] || exit 0
echo "$now" > "$stamp"
printf '%s\n' '{"decision":"block","reason":"aibus is attached to this session. Call the aibus next_step tool with wait_ms 20000. If it returns a step, carry it out here and answer with step_result (you may decline). If it returns no step, stop."}'
