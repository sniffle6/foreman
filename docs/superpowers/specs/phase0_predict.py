#!/usr/bin/env python3
"""Frozen Phase 0 prediction model for the paste-simulation rework.

Computes P (PSReadLine per-UTF-16-unit endpoint) and E (whole-glyph /
alacritty endpoint) for the protocol scenarios. This file is part of the
frozen protocol: do not edit after the probe runs. See
2026-07-12-paste-sim-phase0-protocol.md.

Model assumptions under test (pre-registered):
  E (alacritty 0.26, vendored source verified):
    - width via unicode-width (emoji=2, ASCII/space=1)
    - pending-wrap (input_needs_wrap) applies lazily BEFORE the next char
    - a width-2 char at the LAST column writes a LEADING_WIDE_CHAR_SPACER
      pad, wraps, places the glyph at cols 0..1 of the next row
    - a char that fills the last column parks the cursor at cols-1 with
      pending=True (cursor never occupies col == cols)
  P (PSReadLine 2.4.5 ConvertOffsetToPoint, Render.cs L1364-1418):
    - iterates UTF-16 units; a non-BMP char is TWO units of width 1 each
      (LengthInBufferCells does not combine surrogate pairs, issue #1329)
    - x advances by unit width; x == cols rolls to (0, y+1)  [ASSUMPTION A1]
    - no pads are ever inserted for width-1 units
"""
import sys
from unicodedata import east_asian_width

def wchar(c: str) -> int:
    # unicode-width 0.2 approximation for the codepoints used in the
    # scenarios (emoji W=2, ASCII/space=1). Scenarios avoid ambiguous chars.
    return 2 if east_asian_width(c) in ("W", "F") else 1

def simulate_E(start_col: int, cols: int, text: str):
    row, col, pending, pads = 0, start_col, False, 0
    for c in text:
        w = wchar(c)
        if pending:
            row, col, pending = row + 1, 0, False
        if w == 2 and col == cols - 1:
            pads += 1                      # LEADING_WIDE_CHAR_SPACER
            row, col = row + 1, 0
        nc = col + w
        if nc >= cols:
            col, pending = cols - 1, True
        else:
            col = nc
    return {"row": row, "col": col, "pending": pending, "pads": pads,
            "flat": row * cols + (cols if pending else col)}

def simulate_P(start_col: int, cols: int, text: str):
    x, y, splits = start_col, 0, 0
    for c in text:
        units = [1, 1] if ord(c) > 0xFFFF else [wchar(c)]
        first = True
        for w in units:
            if not first and x == 0:
                splits += 1               # surrogate pair split at a margin
            x += w
            if x >= cols:                 # A1: immediate roll
                x, y = x - cols if x > cols else 0, y + 1
            first = False
    return {"row": y, "col": x, "splits": splits, "flat": y * cols + x}

SCENARIOS = {
    "S1-primary":  dict(cols=40, start_col=3, text="\U0001F952\U0001F923\U0001F923\U0001F923 " * 48),
    "S2-holdout":  dict(cols=33, start_col=6, text="ab\U0001F923\U0001F952cd\U0001F923 " * 30),
    "S3-control":  dict(cols=40, start_col=3, text="abcd " * 80),
}

if __name__ == "__main__":
    for name, s in SCENARIOS.items():
        E = simulate_E(s["start_col"], s["cols"], s["text"])
        P = simulate_P(s["start_col"], s["cols"], s["text"])
        delta = E["flat"] - P["flat"]
        print(f"{name}: cols={s['cols']} start_col={s['start_col']} "
              f"chars={len(s['text'])} cells_wholeglyph={sum(wchar(c) for c in s['text'])}")
        print(f"  E: row+{E['row']} col={E['col']} pending={E['pending']} pads={E['pads']} flat={E['flat']}")
        print(f"  P: row+{P['row']} col={P['col']} splits={P['splits']} flat={P['flat']}")
        print(f"  delta E-P = {delta}   (arm iff delta > 0 and E not pending)")
