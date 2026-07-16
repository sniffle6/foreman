#!/usr/bin/env python3
"""Predicate v3 (full-extent back-fingerprint) + the scan that produced the
2/8-firing result recorded in 2026-07-12-paste-sim-phase0-results.md.

PRESERVED VERBATIM from the inline Phase 0 run (fifth review: the inline-only
run was not reproducible). Status: CANDIDATE — approximate Python grid, no
pad-flag scrub modeling; superseded by the Rust real-Term replay (spec Task 0).
Known latent issue found at review: this model checks spacer/pad identity per
cell, but the real grid scrubs pad flags (results F2) — the Rust predicate
must treat pad cells as char-only, flags don't-care.

Usage: phase0_predicate_v3.py <capture.bin> <spec: s1|s2>
"""
import os
import sys
from phase0_diff import read_epoch, PAYLOADS
from phase0_predict import simulate_E, simulate_P, wchar
import phase0_o4_scan as scan

def e_cells(start_col, cols, text):
    cells, col, pending = [], start_col, False
    for ch in text:
        w = wchar(ch)
        if pending:
            col, pending = 0, False
        if w == 2 and col == cols - 1:
            cells.append((" ", False, True))   # deferral pad
            col = 0
        if w == 1:
            cells.append((ch, False, False))
        else:
            cells.append((ch, True, False))
            cells.append((" ", False, True))
        nc = col + w
        if nc >= cols:
            col, pending = cols - 1, True
        else:
            col = nc
    return cells

def predicate_v3(g, P_col, delta, expected):
    if g.c != P_col:
        return None
    er, ec = scan.advance(g.cols, g.r, g.c, delta)
    if er >= g.rows or not g.content_end(er, ec):
        return None
    f = scan.flat(g.cols, er, ec)
    if f < len(expected):
        return None
    for i, (ch, wide, sp) in enumerate(reversed(expected)):
        rr, cc = divmod(f - 1 - i, g.cols)
        if g.cells[rr][cc] != ch or g.wide[rr][cc] != wide or g.spacer[rr][cc] != sp:
            return None
    return (g.r, g.c, er, ec)

if __name__ == "__main__":
    path, spec = sys.argv[1], sys.argv[2]
    payload, start_col, cols = PAYLOADS[spec]
    P = simulate_P(start_col, cols, payload)
    E = simulate_E(start_col, cols, payload)
    delta = E["flat"] - P["flat"]
    expected = e_cells(start_col, cols, payload)
    assert len(expected) == E["flat"] - start_col
    epoch = read_epoch(path)
    g = scan.Grid(cols, 14)
    g.cup(1, 1)
    for ch in "@" * start_col:
        g.input(ch)
    fires, buf, off, n = [], epoch, 0, len(epoch)
    while off < n:
        m = scan.CSI.match(buf[off:])
        if m:
            params, letter = m.group(1).decode(), m.group(2).decode()
            if letter in "Hf":
                parts = (params or "1;1").split(";")
                g.cup(int(parts[0] or 1), int(parts[1] or 1) if len(parts) > 1 else 1)
            off += m.end()
        elif buf[off] == 0x1B:
            off += 2
        elif buf[off] in (0x0D, 0x0A, 0x07):
            if buf[off] == 0x0D:
                g.c = 0
            elif buf[off] == 0x0A:
                g.r = min(g.r + 1, g.rows - 1)
            off += 1
        else:
            for width in (1, 2, 3, 4):
                try:
                    ch = buf[off:off + width].decode("utf-8")
                    break
                except UnicodeDecodeError:
                    continue
            else:
                ch, width = "�", 1
            g.input(ch)
            off += width
        hit = predicate_v3(g, P["col"], delta, expected)
        if hit:
            fires.append((off, hit))
    print(f"{spec}: v3 fires={len(fires)} aliases={ {h[1] for h in fires} } "
          f"first@{fires[0][0] if fires else None} last@{fires[-1][0] if fires else None} epoch={n}")
