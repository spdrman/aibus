# Stand-in CLI that mixes a recorded basic run with the damage a real pipe can carry: broken JSON,
# invalid UTF-8, an event type aibus has never seen, and a final
# line cut off with no newline. Launched as `/bin/sh malformed.sh`.
head -n 4 "$STREAM"
echo '{not json at all'
printf '\377\376 not utf-8\n'
echo '{"type":"future_event_kind","payload":{"x":1}}'
tail -n +5 "$STREAM"
printf '{"type":"assistant","message":{"role":"assis'
