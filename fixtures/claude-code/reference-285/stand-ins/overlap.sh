# Stand-in CLI that writes stderr at the same time as it prints a recorded stream whose two
# subagents overlap. $NOISE is how many stderr lines (default 20). Launched as `/bin/sh overlap.sh`.
( i=0; while [ "$i" -lt "${NOISE:-20}" ]; do echo "warning: stand-in noise $i" >&2; i=$((i+1)); done ) &
cat "$STREAM"
wait
