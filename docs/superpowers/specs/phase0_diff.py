#!/usr/bin/env python3
"""Phase 0: diff the final PSReadLine redraw against the pasted payload.

Extracts the last full-line redraw (final `CUP 1;4` .. final endpoint CUP),
strips escape sequences, and walks the flowing text with whole-glyph widths
to locate every cell conhost inserted that is NOT in the payload (deferral
spaces). Also prints the frozen sim's pad positions for comparison.

Usage: phase0_diff.py <capture.bin> <start_col> <cols> <payload-spec>
  payload-spec: "s1" or "s2"
"""
import re
import struct
import sys
from phase0_predict import simulate_E, simulate_P, wchar

def read_epoch(path):
    with open(path, "rb") as f:
        data = f.read()
    (paste_start,) = struct.unpack_from("<I", data, 0)
    off, frames = 4, []
    while off < len(data):
        (n,) = struct.unpack_from("<I", data, off)
        off += 4
        frames.append(data[off:off + n])
        off += n
    return b"".join(frames[paste_start:])

ESCSEQ = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b[\]P][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b.")

def final_redraw_text(epoch: bytes) -> str:
    text = epoch.decode("utf-8", errors="replace")
    # last home CUP starts the final redraw
    homes = [m.start() for m in re.finditer(r"\x1b\[1;4H", text)]
    seg = text[homes[-1]:]
    # cut at the endpoint CUP that follows the content
    cups = list(re.finditer(r"\x1b\[(\d+);(\d+)H", seg))
    end = cups[-1]  # the endpoint CUP is the last one in the segment
    seg = seg[: end.start()]
    return ESCSEQ.sub("", seg)

PAYLOADS = {
    "s1": ("\U0001F952\U0001F923\U0001F923\U0001F923 " * 48, 3, 40),
    "s2": ("ab\U0001F923\U0001F952cd\U0001F923 " * 30, 6, 33),
}

if __name__ == "__main__":
    path, spec = sys.argv[1], sys.argv[2]
    payload, start_col, cols = PAYLOADS[spec]
    drawn = final_redraw_text(read_epoch(path))
    print(f"payload_chars={len(payload)} drawn_chars={len(drawn)}")

    # Walk the drawn flow with whole-glyph widths; diff against payload.
    col, row = start_col, 0
    pi, extras = 0, []
    pending = False
    for ch in drawn:
        w = wchar(ch)
        if pending:
            row, col, pending = row + 1, 0, False
        if w == 2 and col == cols - 1:
            # alacritty would pad here; conhost spaces should prevent this
            extras.append(("ALACRITTY-PAD", row, col))
            row, col = row + 1, 0
        if pi < len(payload) and ch == payload[pi]:
            pi += 1
        else:
            extras.append((repr(ch), row, col))
        nc = col + w
        if nc >= cols:
            col, pending = cols - 1, True
        else:
            col = nc
    endpoint_flat = row * cols + (cols if pending else col)
    print(f"payload_consumed={pi}/{len(payload)}")
    print(f"drawn endpoint: row+{row} col={col} pending={pending} flat={endpoint_flat}")
    for kind, r, c in extras:
        print(f"  extra {kind} at row+{r} col={c} (flat {r * cols + c})")

    E = simulate_E(start_col, cols, payload)
    P = simulate_P(start_col, cols, payload)
    print(f"frozen sim: E={E}  P={P}  delta={E['flat'] - P['flat']}")
