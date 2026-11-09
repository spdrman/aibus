#!/usr/bin/env python3
"""Reads the frame delays out of a GIF and checks the length and the size.

    demo-gif-check.py <file.gif> [seconds=10.0] [tolerance=0.2] [max_mb=5]

Walks the GIF blocks itself (no imaging library): every graphic control extension carries the
delay of the frame that follows, in hundredths of a second.
"""
import os
import sys


def delays(path: str) -> list[int]:
    b = open(path, "rb").read()
    if b[:6] not in (b"GIF87a", b"GIF89a"):
        raise SystemExit(f"{path}: not a GIF")
    i = 13
    if b[10] & 0x80:
        i += 3 * (2 << (b[10] & 7))
    out, pending = [], 0
    while i < len(b):
        tag = b[i]
        if tag == 0x3B:
            break
        if tag == 0x21:
            label = b[i + 1]
            if label == 0xF9:
                pending = b[i + 4] | (b[i + 5] << 8)
            i += 2
            while b[i]:
                i += b[i] + 1
            i += 1
        elif tag == 0x2C:
            flags = b[i + 9]
            i += 10
            if flags & 0x80:
                i += 3 * (2 << (flags & 7))
            i += 1
            while b[i]:
                i += b[i] + 1
            i += 1
            out.append(pending)
            pending = 0
        else:
            raise SystemExit(f"{path}: bad block {tag:#x} at {i}")
    return out


def main() -> int:
    path = sys.argv[1]
    want = float(sys.argv[2]) if len(sys.argv) > 2 else 10.0
    tol = float(sys.argv[3]) if len(sys.argv) > 3 else 0.2
    max_mb = float(sys.argv[4]) if len(sys.argv) > 4 else 5.0
    d = delays(path)
    total = sum(d) / 100.0
    size = os.path.getsize(path) / 1e6
    print(f"{path}: {len(d)} frames, {total:.2f} s, {size:.2f} MB, delays(cs) min {min(d)} max {max(d)}")
    ok = abs(total - want) <= tol and size <= max_mb
    if not ok:
        print(f"want {want} s +-{tol} and at most {max_mb} MB", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
