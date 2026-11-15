# Stand-in CLI: prints a recorded or prepared stream ($STREAM) and exits 0, the way `claude -p
# --output-format stream-json` ends a normal attempt. Launched as `/bin/sh replay.sh`.
cat "$STREAM"
