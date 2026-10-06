#!/usr/bin/env python3
"""Shape text with an instance sliced here, and compare where the glyphs land.

Question this answers
---------------------
    Does a font sliced here position its glyphs -- kerning, mark attachment -- the way
    the variable font does at the same location, and the way fontTools' instance does?

Outline checks cannot see positioning at all. Variable kerning and anchors live in a
`GDEF` item variation store that `GPOS` reaches into, and an instance that mishandles it
draws every glyph perfectly and sets every line at the wrong width. The way to see that is
to set text, so this shapes with HarfBuzz:

    every ordered pair of characters, in three fonts:
        the variable font, at the location (HarfBuzz applies the store itself)
        fontTools' instance, from `instantiateVariableFont` with the same request
        ours, from `slice cut`

and compares each glyph's advance and offsets. A pair is a base and a combining mark as
often as two letters, so mark anchors are covered along with kerning.

A static request is compared at its one location. A request that leaves axes variable is
compared at each surviving axis's minimum, middle and maximum, with every combination of
those across axes -- the interior sample is the one that matters, because a store re-tented
without its residual is exactly right at the default and wrong everywhere else.

Usage
-----
    tools/kerning-compare.py FONT --axis wght=900 --axis wdth=80     # static
    tools/kerning-compare.py FONT --axis CASL=1 --axis wght=300:700   # partial
    tools/kerning-compare.py FONT ... --all-chars   # every character in cmap
    tools/kerning-compare.py FONT ... --no-build    # use the built target/release/slice
    tools/kerning-compare.py FONT ... --slice PATH  # test another build's binary

`--axis` takes the Axis Editor syntax, exactly as `slice cut` does, and every axis not named
is left whole. Without --all-chars the pairs are drawn from the font's characters in
U+0020-024F and U+0300-036F, which is the Latin a reader would notice first and every
combining mark that attaches to it.

Needs network access the first time, to install fontTools and uharfbuzz into
`.shaping-venv/`. Exits 0 when ours positions every pair exactly as fontTools' instance
does, 1 otherwise.
"""

from __future__ import annotations

import argparse
import itertools
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENV = REPO_ROOT / ".shaping-venv"
FONTTOOLS_VERSION = "4.62.1"
UHARFBUZZ_VERSION = "0.56.3"
SLICE = REPO_ROOT / "target" / "release" / "slice"


def ensure_venv() -> Path | None:
    python = VENV / "bin" / "python"
    if python.exists():
        return python
    print(f"setting up {VENV} with fontTools {FONTTOOLS_VERSION}, uharfbuzz {UHARFBUZZ_VERSION}")
    try:
        subprocess.run([sys.executable, "-m", "venv", str(VENV)], check=True)
        subprocess.run(
            [str(python), "-m", "pip", "install", "-q",
             f"fonttools=={FONTTOOLS_VERSION}", f"uharfbuzz=={UHARFBUZZ_VERSION}"],
            check=True,
        )
    except subprocess.CalledProcessError as e:
        print(f"could not prepare the environment: {e}", file=sys.stderr)
        return None
    return python


def reexec_in_venv() -> None:
    """Run the rest of this script inside the venv, where uharfbuzz is importable."""
    try:
        import uharfbuzz  # noqa: F401
        import fontTools  # noqa: F401
        return
    except ImportError as missing:
        # Once is enough: a venv left half-installed would otherwise re-execute forever.
        if os.environ.get("SLICE_TOOL_REEXECUTED"):
            sys.exit(f"{VENV} cannot import {missing.name}; delete it and run this again")
    python = ensure_venv()
    if python is None:
        sys.exit("skipping the kerning comparison")
    os.environ["SLICE_TOOL_REEXECUTED"] = "1"
    os.execv(str(python), [str(python), str(Path(__file__).resolve()), *sys.argv[1:]])


def parse_axes(specs: list[str]) -> dict:
    """`wght=900` to a pin, `wght=300:700` to a range, as `slice cut` reads them."""
    out = {}
    for spec in specs:
        tag, value = spec.split("=", 1)
        if ":" in value:
            low, high = value.split(":", 1)
            out[tag] = (float(low), float(high))
        else:
            out[tag] = float(value)
    return out


def sample_locations(request: dict, fvar_axes) -> list[dict]:
    """Every location to compare at: the request's pins, crossed with each surviving
    axis's minimum, middle and maximum."""
    fixed, ranges = {}, {}
    for axis in fvar_axes:
        limit = request.get(axis.axisTag, (axis.minValue, axis.maxValue))
        if isinstance(limit, tuple):
            low, high = limit
            ranges[axis.axisTag] = sorted({low, (low + high) / 2, high})
        else:
            fixed[axis.axisTag] = limit
    if not ranges:
        return [fixed]
    tags = list(ranges)
    return [{**fixed, **dict(zip(tags, values))}
            for values in itertools.product(*(ranges[t] for t in tags))]


class Shaper:
    def __init__(self, path: Path, location: dict | None):
        import uharfbuzz as hb

        self.hb = hb
        self.font = hb.Font(hb.Face(path.read_bytes()))
        if location:
            self.font.set_variations(location)

    def positions(self, text: str) -> tuple:
        buf = self.hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        self.hb.shape(self.font, buf)
        return tuple((p.x_advance, p.x_offset, p.y_offset) for p in buf.glyph_positions)


def characters(font, all_chars: bool) -> list[str]:
    cmap = font.getBestCmap() or {}
    keep = sorted(cmap) if all_chars else sorted(
        c for c in cmap if 0x20 <= c <= 0x24F or 0x300 <= c <= 0x36F)
    return [chr(c) for c in keep if not chr(c).isspace()]


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("font", type=Path)
    parser.add_argument("--axis", action="append", default=[], metavar="TAG=VALUE")
    parser.add_argument("--all-chars", action="store_true")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--slice", type=Path, default=SLICE,
                        help="the slice binary to test; another build's implies --no-build")
    args = parser.parse_args()
    reexec_in_venv()

    from fontTools.ttLib import TTFont
    from fontTools.varLib.instancer import instantiateVariableFont

    if not args.no_build and args.slice == SLICE:
        subprocess.run(["cargo", "build", "--release", "-p", "slice-cli"], cwd=REPO_ROOT,
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    request = parse_axes(args.axis)
    source = TTFont(args.font)
    locations = sample_locations(request, source["fvar"].axes)
    chars = characters(source, args.all_chars)
    pairs = ["".join(p) for p in itertools.product(chars, repeat=2)]
    print(f"{args.font.name}: {len(chars)} characters, {len(pairs)} pairs, "
          f"{len(locations)} location(s)")

    with tempfile.TemporaryDirectory(prefix="kerning-compare-") as scratch:
        scratch = Path(scratch)
        ours = scratch / "ours.ttf"
        reference = scratch / "fonttools.ttf"
        subprocess.run([str(args.slice), "cut", str(args.font), str(ours),
                        *(f"--axis={a}" for a in args.axis)],
                       check=True, stdout=subprocess.DEVNULL)
        instantiateVariableFont(TTFont(args.font), dict(request)).save(reference)
        still_variable = "fvar" in TTFont(ours)

        failed = False
        for location in locations:
            at = location if still_variable else None
            truth = Shaper(args.font, location)
            theirs = Shaper(reference, at)
            mine = Shaper(ours, at)
            ours_off, theirs_off, ours_vs_theirs = [], 0, 0
            for text in pairs:
                want = truth.positions(text)
                got = mine.positions(text)
                ref = theirs.positions(text)
                if got != want:
                    worst = max((abs(a - b) for g, w in zip(got, want) for a, b in zip(g, w)),
                                default=0)
                    ours_off.append((worst, text, got, want))
                theirs_off += ref != want
                ours_vs_theirs += got != ref
            where = " ".join(f"{t}={v:g}" for t, v in location.items())
            print(f"  at {where}")
            print(f"    pairs placed differently from the variable font: "
                  f"fontTools {theirs_off}, ours {len(ours_off)}")
            print(f"    pairs where ours and fontTools disagree: {ours_vs_theirs}")
            for worst, text, got, want in sorted(ours_off, reverse=True)[:5]:
                print(f"      {text!r:8} off by up to {worst}: ours {got}, variable font {want}")
            failed |= ours_vs_theirs > 0
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
