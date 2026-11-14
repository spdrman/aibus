# Sourced by the release suites. Runs a command under a pseudo-terminal nobody types into, and returns the
# command's exit status (124 if the timeout fired). script(1) differs by OS: BSD/macOS takes the command as
# arguments (`script -q /dev/null cmd args`), util-linux wants one string (`script -qec "cmd args" /dev/null`
# and -e to pass the exit status through). On Linux script runs that string through $SHELL, and %q writes an
# argument with a newline or tab as $'...', which dash does not know, so the command runs under bash.
pty_available() { command -v script >/dev/null && command -v timeout >/dev/null; }
pty_run() { # secs cmd args... : stdin stays open for secs+2, so a prompt would hang until the timeout.
  # If PTY_INPUT is set (even empty), it is typed into the terminal after 2s, before stdin idles: $'y\n' answers
  # a prompt, $'\004' is the end-of-file character.
  local secs=$1; shift
  pty_available || { echo "pty_run: script(1) or timeout is missing" >&2; return 127; }
  case $(uname -s) in
    Linux) SHELL=$(command -v bash) timeout "$secs" script -qec "$(printf '%q ' "$@")" /dev/null < <(if [ -n "${PTY_INPUT+x}" ]; then sleep 2; printf '%s' "$PTY_INPUT"; fi; sleep $((secs + 2))) ;;
    Darwin|*BSD) timeout "$secs" script -q /dev/null "$@" < <(if [ -n "${PTY_INPUT+x}" ]; then sleep 2; printf '%s' "$PTY_INPUT"; fi; sleep $((secs + 2))) ;;
    *) echo "pty_run: no known script(1) syntax for $(uname -s)" >&2; return 127 ;;
  esac
}
