#!/usr/bin/env python3
"""Check a font that came out of overlap removal with an engine that is not ours.

Question this answers
---------------------
    After "Remove overlapping contours", does any glyph still overlap -- and did the
    merge keep every glyph's shape?

The engine's own tests check the filled region: that merging did not change which points
are inside a glyph. That is necessary and it is not the point. A merge that does nothing
at all passes it perfectly. What a user sees is the other half: outline the type in a
design application and look for paths that still cross. This checks that half, with
skia-pathops -- the Skia path ops fontTools' `removeOverlaps` is built on, and nothing
`linesweeper` shares code with.

Per glyph, composites resolved, it reports:

    cross   two contours whose boundaries cross
    self    a contour that crosses itself
    double  area filled more than once: the glyph's non-zero fill and its even-odd fill
            differ, which they do exactly when some region has a winding of 2 or more
    empty   a contour that encloses less than one square unit -- a stray path

Areas under half a square unit are ignored as noise.

With --against ORIGINAL it also says how far each glyph's filled outline moved: the area
of the symmetric difference between ORIGINAL's fill and the font's, divided by the length
of the font's outline. That is the mean distance the edge moved, in font units. Integer
rounding and the 0.05-unit quadratic refit keep it under half a unit; a counter filled
in or a contour lost would put it in the tens.

Usage
-----
    tools/overlap-check.py FONT [FONT ...]          # any overlaps left?
    tools/overlap-check.py --against PLAIN MERGED   # ...and did the shapes survive?
    tools/overlap-check.py --list FONT              # name every glyph, not just the first few

Needs network access the first time, to install fontTools and skia-pathops into
`.pathops-venv/`. Exits 0 when every font is clean (and, with --against, no glyph moved
more than half a unit), 1 otherwise. The fonts must be static.

The recipe that found the bug this was written for is in tools/README.md.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENV = REPO_ROOT / ".pathops-venv"
FONTTOOLS_VERSION = "4.62.1"
PATHOPS_VERSION = "0.9.2"

NOISE = 0.5  # square font units
EMPTY = 1.0  # square font units; matches NEGLIGIBLE_AREA in crates/slice-core/src/overlaps.rs
MOVED = 0.5  # font units of mean edge displacement


def ensure_venv() -> Path | None:
    python = VENV / "bin" / "python"
    if python.exists():
        return python
    print(f"setting up {VENV} with fontTools {FONTTOOLS_VERSION}, skia-pathops {PATHOPS_VERSION}")
    try:
        subprocess.run([sys.executable, "-m", "venv", str(VENV)], check=True)
        subprocess.run(
            [str(python), "-m", "pip", "install", "-q",
             f"fonttools=={FONTTOOLS_VERSION}", f"skia-pathops=={PATHOPS_VERSION}"],
            check=True,
        )
    except subprocess.CalledProcessError as e:
        print(f"could not prepare the environment: {e}", file=sys.stderr)
        return None
    return python


def reexec_in_venv() -> None:
    """Run the rest of this script inside the venv, where pathops is importable."""
    try:
        import pathops  # noqa: F401
        import fontTools  # noqa: F401
        return
    except ImportError as missing:
        # Once is enough: a venv left half-installed would otherwise re-execute forever.
        if os.environ.get("SLICE_TOOL_REEXECUTED"):
            sys.exit(f"{VENV} cannot import {missing.name}; delete it and run this again")
    python = ensure_venv()
    if python is None:
        sys.exit("skipping the overlap check")
    os.environ["SLICE_TOOL_REEXECUTED"] = "1"
    os.execv(str(python), [str(python), str(Path(__file__).resolve()), *sys.argv[1:]])


def fill(glyphset, name):
    """The glyph as one pathops Path, composites decomposed."""
    import pathops
    from fontTools.pens.recordingPen import DecomposingRecordingPen

    recording = DecomposingRecordingPen(glyphset)
    glyphset[name].draw(recording)
    path = pathops.Path()
    recording.replay(path.getPen())
    return path


def problems(glyphset, name) -> list[str]:
    import pathops

    whole = fill(glyphset, name)
    contours = list(whole.contours)
    found = []

    for i, contour in enumerate(contours):
        if abs(contour.area) < EMPTY:
            found.append(f"empty[{i}]")
            continue
        merged = pathops.simplify(contour, fix_winding=False)
        if abs(merged.area - contour.area) > NOISE or len(list(merged.contours)) > 1:
            found.append(f"self[{i}]")

    for i in range(len(contours)):
        for j in range(i + 1, len(contours)):
            a, b = contours[i], contours[j]
            if not a.bounds or not b.bounds:
                continue
            (ax0, ay0, ax1, ay1), (bx0, by0, bx1, by1) = a.bounds, b.bounds
            if ax1 < bx0 or bx1 < ax0 or ay1 < by0 or by1 < ay0:
                continue
            shared = pathops.op(a, b, pathops.PathOp.INTERSECTION, fix_winding=False).area
            # Wholly inside the other is nesting, not crossing; `double` judges that.
            if NOISE < shared < min(a.area, b.area) - NOISE:
                found.append(f"cross[{i},{j}]")

    nonzero = pathops.simplify(whole, fix_winding=False)
    whole.fillType = pathops.FillType.EVEN_ODD
    even_odd = pathops.simplify(whole, fix_winding=False)
    if abs(nonzero.area - even_odd.area) > NOISE:
        found.append(f"double({abs(nonzero.area - even_odd.area):.0f})")
    return found


def edge_movement(before, after, name) -> float:
    """Mean distance, in font units, that the filled outline moved."""
    import pathops
    from fontTools.pens.perimeterPen import PerimeterPen

    a, b = fill(before, name), fill(after, name)
    moved = abs(pathops.op(a, b, pathops.PathOp.XOR).area)
    if moved == 0:
        return 0.0
    perimeter = PerimeterPen()
    pathops.simplify(b).draw(perimeter)
    return moved / max(perimeter.value, 1.0)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("fonts", nargs="+", type=Path)
    parser.add_argument("--against", type=Path, metavar="ORIGINAL",
                        help="also measure how far each glyph's outline moved from ORIGINAL")
    parser.add_argument("--list", action="store_true", help="name every glyph found")
    args = parser.parse_args()
    reexec_in_venv()

    from fontTools.ttLib import TTFont

    failed = False
    for path in args.fonts:
        font = TTFont(path)
        glyphset = font.getGlyphSet()
        found = {name: p for name in font.getGlyphOrder() if (p := problems(glyphset, name))}
        kinds = {}
        for p in found.values():
            for kind in {k.split("[")[0].split("(")[0] for k in p}:
                kinds[kind] = kinds.get(kind, 0) + 1
        summary = ", ".join(f"{n} {k}" for k, n in sorted(kinds.items())) or "clean"
        print(f"{path}: {len(found)} of {len(font.getGlyphOrder())} glyphs with problems ({summary})")
        shown = list(found.items()) if args.list else list(found.items())[:12]
        for name, p in shown:
            print(f"    {name:24} {' '.join(p)}")
        if len(shown) < len(found):
            print(f"    ... and {len(found) - len(shown)} more; use --list")
        failed |= bool(found)

        if args.against:
            original = TTFont(args.against)
            before = original.getGlyphSet()
            moved = sorted(
                ((edge_movement(before, glyphset, n), n) for n in font.getGlyphOrder()),
                reverse=True,
            )
            changed = sum(1 for m, _ in moved if m > 0)
            print(f"  against {args.against}: {changed} glyphs' fill changed at all; "
                  f"mean edge movement, worst first:")
            for m, n in moved[:5]:
                print(f"    {n:24} {m:.3f} units")
            failed |= moved[0][0] > MOVED

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
