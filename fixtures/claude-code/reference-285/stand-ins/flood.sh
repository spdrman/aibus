# Stand-in CLI that writes a burst of tiny, distinct stderr lines (all of it fits in the pipe, so the CLI is
# done and exits long before the host has read it) and then prints a recorded stream and exits 0.
# Used only by the ignored defect test. Launched as `/bin/sh flood.sh`.
awk 'BEGIN { for (i = 0; i < 8000; i++) print "w" i }' >&2
cat "$STREAM"
