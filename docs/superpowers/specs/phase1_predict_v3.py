#!/usr/bin/env python3
"""S4/S5 predictions for the v3 validation gate. Frozen before capture.
S4 = v3 blind holdout: non-periodic (13-cell rep vs 47 cols), different
prompt/cols/phase from every prior fixture. S5 = non-BMP P==E control:
emoji present, no margin straddle, must not arm.

Do not edit phase0_predict.py. This file extends it.
Frozen 2026-07-12 before any S4/S5 capture run.
"""
from phase0_predict import simulate_E, simulate_P

# ---------------------------------------------------------------------------
# S4 — v3 blind holdout (must scroll on 14-row capture window)
# 60 reps * 13 cells = 780 cells ≈ 16.6 rows at 47 cols > 14 rows.
# ---------------------------------------------------------------------------
S4_REPS = 60
S4_COLS = 47
S4_START_COL = 5  # prompt "V3~> "
S4_TEXT = "q\U0001F923\U0001F952rs\U0001F923\U0001F923t " * S4_REPS

# ---------------------------------------------------------------------------
# S5 — non-BMP P==E control
# Selection rule (frozen): smallest k in 4..12 such that
#   ("\U0001F923" * 2 + "x" * (k - 4) + " ") * 40
# yields delta == 0 at cols=40, start_col=3.
# Chosen k = 4 → rep = "🤣🤣 " (two emoji + trailing space).
# ---------------------------------------------------------------------------
S5_K = 4
S5_COLS = 40
S5_START_COL = 3  # prompt "P> "
S5_TEXT = ("\U0001F923" * 2 + "x" * (S5_K - 4) + " ") * 40

# ---------------------------------------------------------------------------
# Frozen predictions (recorded before capture — do not revise from measured)
# ---------------------------------------------------------------------------
# S4:
#   E: row+16 col=36 pending=False pads=3 flat=788
#   P: row+16 col=33 splits=4 flat=785
#   delta E-P = 3  (arm)
# Viewport math (14-row window, pre.row=0 expected):
#   post viewport raw row ≈ pre.row + P.row - (history growth)
#   ≈ 0 + 16 - 3 = 13 when history_size ends at 3
#   → FROZEN_S4_ALIAS = (13, 33, 13, 36)
#
# S5:
#   E: flat=203 col=3 pending=False
#   P: flat=203 col=3
#   delta = 0  (must NOT arm)


def report(name, cols, start_col, text):
    E = simulate_E(start_col, cols, text)
    P = simulate_P(start_col, cols, text)
    delta = E["flat"] - P["flat"]
    print(
        f"{name}: cols={cols} start_col={start_col} chars={len(text)}"
    )
    print(
        f"  E: row+{E['row']} col={E['col']} pending={E['pending']} "
        f"pads={E['pads']} flat={E['flat']}"
    )
    print(
        f"  P: row+{P['row']} col={P['col']} splits={P['splits']} flat={P['flat']}"
    )
    print(f"  delta E-P = {delta}   (arm iff delta > 0 and E not pending)")
    return E, P, delta


if __name__ == "__main__":
    report("S4-v3holdout", S4_COLS, S4_START_COL, S4_TEXT)
    report("S5-nonbmp-control", S5_COLS, S5_START_COL, S5_TEXT)
    print()
    print("FROZEN_S4_PAYLOAD_REPS =", S4_REPS)
    print("FROZEN_S4_ALIAS (viewport prediction) = (13, 33, 13, 36)")
    print("FROZEN_S5_PAYLOAD =", repr(S5_TEXT[:20] + "..."))
    print("S5_K =", S5_K)
