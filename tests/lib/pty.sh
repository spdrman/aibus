# Sourced by the release suites. Runs a command under a pseudo-terminal nobody types into, and returns the
# command's exit status (124 if the timeout fired). script(1) differs by OS: BSD/macOS takes the command as
# arguments (`script -q /dev/null cmd args`), util-linux wants one string (`script -qec "cmd args" /dev/null`
# and -e to pass the exit status through). On Linux script runs that string through $SHELL, and %q writes an
# argument with a newline or tab as $'...', which dash does not know, so the command runs under bash.
pty_available() { command -v script >/dev/null && command -v timeout >/dev/null; }
pty_run() { # secs cmd args... : stdin stays open for secs+2, so a prompt would hang until the timeout.
  # If PTY_INPUT is set (even empty), it is typed into the terminal before stdin idles: $'y\n' answers a prompt,
  # $'\004' is the end-of-file character. By default it is typed after a fixed 2s, which loses the race on a
  # loaded host (#278). If PTY_PROMPT is also set, it is an extended regular expression (grep -E): the input is
  # typed once the terminal output matches it (polled every 0.1s), however long that takes. secs is then only
  # the overall budget that guards a hang: when the prompt never shows, nothing is typed and timeout exits 124.
  local secs=$1; shift
  pty_available || { echo "pty_run: script(1) or timeout is missing" >&2; return 127; }
  local out rc
  out=$(/usr/bin/mktemp "${TMPDIR:-/tmp}/pty-out.XXXXXX") || return 127
  # The child's output goes to a file the feeder can poll; it is copied to our stdout once the child has ended.
  case $(uname -s) in
    Linux) SHELL=$(command -v bash) timeout "$secs" script -qec "$(printf '%q ' "$@")" /dev/null >"$out" < <(_pty_feed "$secs" "$out") ;;
    Darwin|*BSD) timeout "$secs" script -q /dev/null "$@" >"$out" < <(_pty_feed "$secs" "$out") ;;
    *) rm -f "$out"; echo "pty_run: no known script(1) syntax for $(uname -s)" >&2; return 127 ;;
  esac
  rc=$?
  cat "$out"; rm -f "$out"   # the feeder stops when the file is gone
  return $rc
}
_pty_feed() { # secs outfile : what pty_run types, then holds stdin open until the child has ended (outfile is gone) or secs+2
  local secs=$1 out=$2 waited=0 type=1
  if [ -n "${PTY_INPUT+x}" ]; then
    if [ -n "${PTY_PROMPT+x}" ]; then
      while ! grep -Eq -- "$PTY_PROMPT" "$out" 2>/dev/null; do
        if [ $waited -ge $((secs * 10)) ] || [ ! -e "$out" ]; then type=0; break; fi   # never printed: type nothing, timeout ends the run
        sleep 0.1; waited=$((waited + 1))
      done
    else sleep 2; fi
    [ $type = 1 ] && printf '%s' "$PTY_INPUT"
  fi
  waited=0
  while [ -e "$out" ] && [ $waited -lt $(((secs + 2) * 10)) ]; do sleep 0.1; waited=$((waited + 1)); done
}
