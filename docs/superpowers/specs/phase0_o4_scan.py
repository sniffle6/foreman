#!/usr/bin/env python3
"""Phase 0 outcome O4: evaluate the acceptance predicate after EVERY byte of
the paste epoch (every possible chunk boundary), using a minimal VT
interpreter with alacritty 0.26 flow semantics (lazy pending-wrap, wide
deferral pads as layout cells, overwrite).

Reports every byte offset where the predicate would fire, with the alias it
would produce. Green expectation: fires only at/after the final endpoint CUP,
always with the correct alias value.

Usage: phase0_o4_scan.py <capture.bin> <spec: s1|s2>
"""
import re
import sys
from phase0_diff import read_epoch, PAYLOADS
from phase0_predict import simulate_E, simulate_P, wchar

class Grid:
    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.cells = [[" "] * cols for _ in range(rows)]
        self.wide = [[False] * cols for _ in range(rows)]  # wide base marker
        self.spacer = [[False] * cols for _ in range(rows)]
        self.r, self.c, self.pending = 0, 0, False

    def put(self, ch, wide=False, spacer=False):
        self.cells[self.r][self.c] = ch
        self.wide[self.r][self.c] = wide
        self.spacer[self.r][self.c] = spacer

    def input(self, ch):
        w = wchar(ch)
        if w == 0:
            return
        if self.pending:
            self.r, self.c, self.pending = min(self.r + 1, self.rows - 1), 0, False
        if w == 2 and self.c + 1 >= self.cols:
            self.put(" ", spacer=True)  # deferral pad (flag scrub irrelevant to layout)
            self.r, self.c = min(self.r + 1, self.rows - 1), 0
        if w == 1:
            self.put(ch)
        else:
            self.put(ch, wide=True)
            self.c += 1
            self.put(" ", spacer=True)
        if self.c + 1 < self.cols:
            self.c += 1
        else:
            self.pending = True

    def cup(self, row, col):
        self.r = min(max(row - 1, 0), self.rows - 1)
        self.c = min(max(col - 1, 0), self.cols - 1)
        self.pending = False

    def empty(self, r, c):
        # spacer cells read as content only via glyph adjacency; plain model:
        return self.cells[r][c] == " " and not self.wide[r][c] and not self.spacer[r][c]

    def content_end(self, r, c):
        # emptiness of the suffix from (r,c) to the end of the viewport
        # (packed rows: no wrap-chain shortcut — conservative full scan)
        for rr in range(r, self.rows):
            for cc in range(c if rr == r else 0, self.cols):
                if not self.empty(rr, cc):
                    return False
        return True

def flat(cols, r, c):
    return r * cols + c

def advance(cols, r, c, n):
    f = flat(cols, r, c) + n
    return f // cols, f % cols

def predicate(g, P_col, delta, tail_glyph, trailing_spaces, require_behind):
    if g.c != P_col:
        return None
    er, ec = advance(g.cols, g.r, g.c, delta)
    if er >= g.rows or not g.content_end(er, ec):
        return None
    if require_behind and g.content_end(g.r, g.c):
        return None
    # fingerprint: walk back from E' over exactly `trailing_spaces` empty
    # space cells, then the tail glyph (wide base + spacer), char-exact
    f = flat(g.cols, er, ec)
    for i in range(1, trailing_spaces + 1):
        rr, cc = divmod(f - i, g.cols)
        if not (g.cells[rr][cc] == " " and not g.wide[rr][cc] and not g.spacer[rr][cc]):
            return None
    f -= trailing_spaces
    rr, cc = divmod(f - 1, g.cols)  # spacer of the last wide glyph
    br, bc = divmod(f - 2, g.cols)  # base of the last wide glyph
    if not g.spacer[rr][cc] or not g.wide[br][bc] or g.cells[br][bc] != tail_glyph:
        return None
    return (g.r, g.c, er, ec)

CSI = re.compile(rb"^\x1b\[([0-9;?]*)([A-Za-z])")

if __name__ == "__main__":
    path, spec = sys.argv[1], sys.argv[2]
    payload, start_col, cols = PAYLOADS[spec]
    rows = 14
    P = simulate_P(start_col, cols, payload)
    E = simulate_E(start_col, cols, payload)
    delta = E["flat"] - P["flat"]
    stripped = payload.rstrip(" ")
    trailing = len(payload) - len(stripped)
    tail_glyph = stripped[-1]
    print(f"{spec}: P_col={P['col']} delta={delta} tail={tail_glyph!r} trailing_spaces={trailing}")

    epoch = read_epoch(path)
    g = Grid(cols, rows)
    # seed the prompt row so start_col is plausible (content irrelevant)
    g.cup(1, 1)
    for ch in "@" * start_col:
        g.input(ch)

    fires = {"behind": [], "fingerprint-only": []}
    i, n = 0, len(epoch)
    text = epoch.decode("utf-8", errors="replace")
    # decode positions: iterate chars but track byte offsets
    off = 0
    buf = epoch
    while off < n:
        m = CSI.match(buf[off:])
        if m:
            params, letter = m.group(1).decode(), m.group(2).decode()
            if letter in "Hf":
                parts = (params or "1;1").split(";")
                row = int(parts[0] or 1)
                col = int(parts[1] or 1) if len(parts) > 1 else 1
                g.cup(row, col)
            # SGR/mode/other CSI: ignore for layout
            off += m.end()
        elif buf[off] == 0x1B:
            off += 2  # bare escape: skip introducer + one byte (none observed)
        elif buf[off] in (0x0D, 0x0A, 0x07):
            if buf[off] == 0x0D:
                g.c = 0
            elif buf[off] == 0x0A:
                g.r = min(g.r + 1, g.rows - 1)
            off += 1
        else:
            # printable UTF-8 char
            for width in (1, 2, 3, 4):
                try:
                    ch = buf[off:off + width].decode("utf-8")
                    break
                except UnicodeDecodeError:
                    continue
            else:
                ch, width = "�", 1
            if ch:
                g.input(ch)
            off += width
        for mode, req in (("behind", True), ("fingerprint-only", False)):
            hit = predicate(g, P["col"], delta, tail_glyph, trailing, req)
            if hit:
                fires[mode].append((off, hit))

    for mode, hits in fires.items():
        if not hits:
            print(f"  [{mode}] never fires")
            continue
        first, last = hits[0], hits[-1]
        # contiguous suffix check
        offs = [h[0] for h in hits]
        aliases = {h[1] for h in hits}
        print(f"  [{mode}] fires at {len(hits)} byte positions, first@{first[0]} last@{last[0]}")
        print(f"    distinct aliases: {aliases}")
