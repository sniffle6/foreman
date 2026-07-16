#!/usr/bin/env python3
"""Phase 0 replay analysis: decode a framed capture from the phase0 probe.

Usage: python phase0_analyze.py %TEMP%/phase0-s1-primary.bin [--frames]
Prints the paste-epoch byte stream with readable escapes, CUP offsets with
parameters, and a per-row reconstruction summary.
"""
import re
import struct
import sys

def read_frames(path):
    with open(path, "rb") as f:
        data = f.read()
    (paste_start,) = struct.unpack_from("<I", data, 0)
    off, frames = 4, []
    while off < len(data):
        (n,) = struct.unpack_from("<I", data, off)
        off += 4
        frames.append(data[off:off + n])
        off += n
    return paste_start, frames

def readable(b: bytes) -> str:
    out = []
    text = b.decode("utf-8", errors="replace")
    for ch in text:
        cp = ord(ch)
        if ch == "\x1b":
            out.append("<ESC>")
        elif cp == 13:
            out.append("<CR>")
        elif cp == 10:
            out.append("<LF>\n")
        elif cp < 32 or cp == 127:
            out.append(f"<{cp:02X}>")
        else:
            out.append(ch)
    return "".join(out)

CUP = re.compile(rb"\x1b\[(\d*);?(\d*)([Hf])")

if __name__ == "__main__":
    path = sys.argv[1]
    paste_start, frames = read_frames(path)
    epoch = b"".join(frames[paste_start:])
    print(f"frames={len(frames)} paste_start_frame={paste_start} epoch_bytes={len(epoch)}")
    print("=== CUPs in paste epoch (byte offset, 1-based row;col) ===")
    for m in CUP.finditer(epoch):
        row = m.group(1).decode() or "1"
        col = m.group(2).decode() or "1"
        print(f"  @{m.start():6d}  CUP {row};{col}")
    print("=== decoded epoch stream ===")
    print(readable(epoch))
