# Stand-in CLI whose process is killed by someone else after a recorded, truncated stream. Launched
# as `/bin/sh killed.sh`.
cat "$STREAM"
kill -9 $$
