# Stand-in CLI that is mid-delegation when the user cancels. It leaves a marker on SIGTERM so the
# test can tell the graceful stop came before any kill. Launched as `/bin/sh cancel.sh`.
trap 'touch "$MARKER"; exit 143' TERM
head -n 40 "$STREAM"
while :; do sleep 0.05; done
