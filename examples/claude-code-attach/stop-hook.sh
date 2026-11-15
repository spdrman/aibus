#!/bin/sh
# Claude Code Stop hook for an attached aibus session (#322, Phase 1 spike). Example only: aibus
# never installs it, you do, in your own settings (see README.md next to this file).
#
# When a turn ends and you have switched attaching on (the marker file exists), it asks Claude
# Code to keep going with one instruction: pull the next step from aibus. It decides nothing else:
# it answers no permission prompt and no elicitation, and it never reads a step itself.
#
# Bounds: it does nothing without the marker, nothing when the turn is already a continuation
# (`stop_hook_active`, which I could not confirm on the vendor page, so there is a second guard),
# and at most one continuation every 20 seconds.
marker="${AIBUS_ATTACH_MARKER:-${XDG_STATE_HOME:-$HOME/.local/state}/aibus/attached}"
[ -f "$marker" ] || exit 0
input=$(cat)
case "$input" in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac
stamp="$marker.last"
now=$(date +%s)
last=$(cat "$stamp" 2>/dev/null || echo 0)
[ $((now - last)) -ge 20 ] || exit 0
echo "$now" > "$stamp"
printf '%s\n' '{"decision":"block","reason":"aibus is attached to this session. Call the aibus next_step tool with wait_ms 20000. If it returns a step, carry it out here and answer with step_result (you may decline). If it returns no step, stop."}'
