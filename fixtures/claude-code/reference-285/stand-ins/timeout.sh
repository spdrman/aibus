# Stand-in CLI that starts a recorded delegation and then never finishes: the host's wall-time
# limit has to stop it. Launched as `/bin/sh timeout.sh`.
head -n 40 "$STREAM"
while :; do sleep 0.05; done
