#!/usr/bin/env python3
"""Records `aibus demo` in a pseudo-terminal and writes an asciicast v2 file.

    demo-cast.py <cols> <rows> <out.cast> <command> [args...]

The demo marks the end of every frame with ESC ] 7777 ; <ms> BEL, the virtual time of that
frame. The bytes before a marker become one output event at that time, so the cast follows the
virtual clock and two recordings are byte-identical however fast the machine was. No wall clock
is read and the header has no timestamp.
"""
import fcntl
import json
import os
import pty
import re
import struct
import sys
import termios

MARK = re.compile(rb"\x1b\]7777;(\d+)\x07")


def main() -> int:
    cols, rows, out, *cmd = sys.argv[1:]
    cols, rows = int(cols), int(rows)
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(cmd[0], cmd)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    data = b""
    while True:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        data += chunk
    _, status = os.waitpid(pid, 0)
    if os.waitstatus_to_exitcode(status) != 0:
        sys.stderr.write("demo-cast: the demo failed\n")
        return 1
    header = {
        "version": 2,
        "width": cols,
        "height": rows,
        "env": {"TERM": "xterm-256color", "SHELL": "/bin/sh"},
    }
    events, pos = [], 0
    for m in MARK.finditer(data):
        text = data[pos : m.start()].decode("utf-8")
        pos = m.end()
        events.append([int(m.group(1)) / 1000.0, "o", text])
    tail = data[pos:].decode("utf-8")
    if tail:
        events.append([events[-1][0] if events else 0.0, "o", tail])
    with open(out, "w", encoding="utf-8") as f:
        f.write(json.dumps(header) + "\n")
        for e in events:
            f.write(json.dumps(e, ensure_ascii=False) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
