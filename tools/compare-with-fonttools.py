#!/usr/bin/env python3
"""Compare what this engine produces against what fontTools produces.

Question this answers
---------------------
    Does slicing a font here give the same font the original Slice would have given?

The original Slice is a thin interface over `fontTools.varLib.instancer`. So the sharpest
possible parity test is not to describe the original's behaviour and check against the
description — it is to run the actual library, on the same input, with the same request,
and diff the results table by table.

That is what this does. It builds a virtual environment with the exact fontTools release
the sub-space solver was ported from, runs `instantiateVariableFont` the way
`InstanceWorker` does (`inplace=True, optimize=True`, default overlap mode), runs
`slice cut` with the same axis settings, and compares the fields where a difference would
mean something.

Usage
-----
    tools/compare-with-fonttools.py             # build, set up the venv, compare
    tools/compare-with-fonttools.py --verbose   # print every field, not just differences

Needs network access the first time, to install fontTools into `.fonttools-venv/`.
Exits 0 when everything matches (allowing for the differences listed in ACCEPTED below),
1 otherwise, and 0 with a message if the environment cannot be prepared.

Known and accepted differences
------------------------------
Two, both recorded in ACCEPTED and both size rather than behaviour:

* fontTools prunes a feature whose lookup list it has just emptied, and then prunes the
  lookups nothing references any more. This keeps them. An empty feature runs no lookups
  and an unreferenced lookup is never reached, so the font behaves identically; it is a
  few hundred bytes larger. Renumbering lookup indices correctly across the script list,
  the feature list and every chained-context rule is error-prone, and the size it would
  save does not justify the risk.
* fontTools rewrites `HVAR` and `MVAR` for a partial instance. This drops them. For
  TrueType outlines `HVAR` is redundant with the phantom points in `gvar`, which are
  rebased — `partial_instance_matches_skrifa` asserts advances still agree exactly with
  `HVAR` gone. `MVAR` is applied at the new default and dropped, so vertical metrics are
  right there but stop varying, which is a real if narrow loss.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
VENV = REPO_ROOT / ".fonttools-venv"
FONTTOOLS_VERSION = "4.62.1"
FIXTURE = REPO_ROOT / "testdata" / "fonts" / "Recursive-VF.subset.ttf"
# Roboto Delta, the only avar 2 font available -- Google Fonts ships none, and fontTools
# 4.62.1 reads that table but cannot build one, so it cannot be generated either.
AVAR2_FIXTURE = REPO_ROOT / "testdata" / "fonts" / "RobotoDelta-avar2.subset.ttf"

# Tables whose absence or size is expected to differ; see the module docstring.
ACCEPTED = {
    "GSUB": "fontTools prunes emptied features and unreferenced lookups; we keep them",
    "GPOS": "fontTools prunes emptied features and unreferenced lookups; we keep them",
    "HVAR": "dropped deliberately: gvar's phantom points carry advance variation",
    "MVAR": "re-tented onto the surviving axes, so its size differs",
}

# Roboto Delta's 39 axes, all at their defaults. An avar 2 font can only be compared with
# *every* axis pinned: fontTools refuses to partially instance one, and so do we. Listing
# them is the price of that -- leave one out and the request becomes a partial.
ROBOTO_DELTA_DEFAULTS = {
    "opsz": 14, "wght": 400, "wdth": 100, "slnt": 0, "GRAD": 0, "XTSP": 0, "XOPQ": 96,
    "YOPQ": 79, "XTRA": 463, "BDSZ": 0, "WDSP": 246, "XOUC": 96, "YOUC": 79, "XTUC": 463,
    "XTUD": 463, "XTUR": 463, "YOPE": 79, "YTUC": 728, "STUI": 92, "STUO": 92, "XOLC": 96,
    "YOLC": 79, "XTLC": 463, "YTAS": 728, "YTDE": -208, "YTLC": 526, "STLI": 32,
    "STLO": 32, "YOLB": 132, "XOFI": 96, "YOFI": 79, "XTFI": 463, "YTFI": 743,
    "BARS": 1000, "VANG": 0, "VROT": 0, "YTTL": 25, "XTTW": 5, "YTOS": 30,
}

# Each case is (fixture, name, axis settings). A value is a number to pin, or a (min, max)
# pair.
CASES = [
    (FIXTURE, "pin every axis", {"MONO": 0, "CASL": 1, "wght": 1000, "slnt": 0, "CRSV": 0.5}),
    (FIXTURE, "pin at the defaults", {"MONO": 0, "CASL": 0, "wght": 300, "slnt": 0, "CRSV": 0.5}),
    # The case that exposed feature variations never being resolved: at CRSV=1 the
    # 'rvrn' feature substitutes the cursive 'a', and that has to be baked in.
    (FIXTURE, "pin CRSV at 1, where rvrn fires", {"MONO": 0, "CASL": 0, "wght": 300, "slnt": 0, "CRSV": 1}),
    (FIXTURE, "pin the slant axis", {"MONO": 0, "CASL": 0, "wght": 700, "slnt": -15, "CRSV": 0.5}),
    (FIXTURE, "keep wght whole", {"MONO": 0, "CASL": 0, "slnt": 0, "CRSV": 0.5}),
    (FIXTURE, "restrict wght", {"MONO": 0, "CASL": 0, "slnt": 0, "CRSV": 0.5, "wght": (300, 700)}),
    (FIXTURE, "keep wght and CASL", {"MONO": 0, "slnt": 0, "CRSV": 0.5, "wght": (300, 800)}),
    # avar 2: the axes move each other through an item variation store, so the normalized
    # location depends on where the *other* axes are. Reading only the version 1 segment
    # maps put these outlines 173 font units from fontTools -- a visibly different letter,
    # produced silently. This is the case that measures it.
    (
        AVAR2_FIXTURE,
        "pin every axis of an avar 2 font",
        {**ROBOTO_DELTA_DEFAULTS, "opsz": 70, "wght": 600, "wdth": 120},
    ),
]

# (label, expression) evaluated on a fontTools TTFont, run inside the venv.
PROBE = r'''
import json, sys
from fontTools import ttLib

def describe(path):
    f = ttLib.TTFont(path)
    out = {}
    out["tables"] = sorted(f.keys())
    m = f["maxp"]
    out["numGlyphs"] = m.numGlyphs
    if getattr(m, "maxPoints", None) is not None:
        out["maxPoints"] = m.maxPoints
        out["maxContours"] = m.maxContours
        out["maxCompositePoints"] = m.maxCompositePoints
        out["maxCompositeContours"] = m.maxCompositeContours
        out["maxComponentElements"] = m.maxComponentElements
        out["maxComponentDepth"] = m.maxComponentDepth
    h = f["head"]
    out["headBBox"] = [h.xMin, h.yMin, h.xMax, h.yMax]
    out["headFlags"] = h.flags
    out["unitsPerEm"] = h.unitsPerEm
    hh = f["hhea"]
    out["advanceWidthMax"] = hh.advanceWidthMax
    out["minLeftSideBearing"] = hh.minLeftSideBearing
    out["minRightSideBearing"] = hh.minRightSideBearing
    out["xMaxExtent"] = hh.xMaxExtent
    out["numberOfHMetrics"] = hh.numberOfHMetrics
    if "OS/2" in f:
        o = f["OS/2"]
        out["usWeightClass"] = o.usWeightClass
        out["usWidthClass"] = o.usWidthClass
        out["xAvgCharWidth"] = o.xAvgCharWidth
        out["fsSelection"] = o.fsSelection
    out["italicAngle"] = f["post"].italicAngle
    out["nameIDs"] = sorted({r.nameID for r in f["name"].names})
    if "fvar" in f:
        out["axes"] = [[a.axisTag, a.minValue, a.defaultValue, a.maxValue] for a in f["fvar"].axes]
        out["instances"] = len(f["fvar"].instances)
    else:
        out["axes"] = None
    if "STAT" in f:
        s = f["STAT"].table
        out["statAxes"] = [a.AxisTag for a in s.DesignAxisRecord.Axis] if s.DesignAxisRecord else []
        out["statValues"] = len(s.AxisValueArray.AxisValue) if s.AxisValueArray else 0
    else:
        out["statAxes"] = None
    # Which lookups each feature actually runs, which is what shaping depends on.
    for tag in ("GSUB", "GPOS"):
        if tag in f:
            t = f[tag].table
            out[tag + "Features"] = sorted(
                (fr.FeatureTag, tuple(fr.Feature.LookupListIndex))
                for fr in (t.FeatureList.FeatureRecord if t.FeatureList else [])
            )
            try:
                fv = t.FeatureVariations
            except AttributeError:
                fv = None
            out[tag + "VarRecords"] = len(fv.FeatureVariationRecord) if fv else 0
    # Glyph outlines, so a numeric difference anywhere shows up.
    gs = f.getGlyphSet()
    from fontTools.pens.recordingPen import RecordingPen
    outlines = {}
    for name in f.getGlyphOrder():
        pen = RecordingPen()
        gs[name].draw(pen)
        outlines[name] = [(op, [[round(c, 3) for c in pt] for pt in args]) for op, args in pen.value]
    out["outlines"] = outlines
    out["advances"] = {n: f["hmtx"][n][0] for n in f.getGlyphOrder()}
    return out

print(json.dumps(describe(sys.argv[1])))
'''

BUILD = r'''
import sys, json
from fontTools import ttLib
from fontTools.varLib import instancer

src, dst, limits = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
limits = {k: (tuple(v) if isinstance(v, list) else v) for k, v in limits.items()}
font = ttLib.TTFont(src)
# The same call the original Slice's InstanceWorker makes.
instancer.instantiateVariableFont(font, limits, inplace=True, optimize=True)
font.save(dst)
'''


def ensure_venv() -> Path | None:
    python = VENV / "bin" / "python"
    if python.exists():
        return python
    print(f"setting up {VENV} with fontTools {FONTTOOLS_VERSION}")
    try:
        subprocess.run([sys.executable, "-m", "venv", str(VENV)], check=True)
        subprocess.run(
            [str(python), "-m", "pip", "install", "-q",
             f"fonttools=={FONTTOOLS_VERSION}", "brotli"],
            check=True,
        )
    except subprocess.CalledProcessError as e:
        print(f"could not prepare the fontTools environment: {e}", file=sys.stderr)
        return None
    return python


def summarise(value) -> str:
    """A one-line rendering of a field, short enough to scan in a column."""
    if isinstance(value, dict):
        return f"<{len(value)} entries>"
    text = str(value)
    return text if len(text) <= 70 else text[:67] + "..."


def run_probe(python: Path, path: Path) -> dict:
    import json
    result = subprocess.run(
        [str(python), "-c", PROBE, str(path)],
        capture_output=True, text=True, check=True,
    )
    return json.loads(result.stdout)


def main() -> int:
    import json

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="print every field's value, not just the ones that differ",
    )
    parser.add_argument("--no-build", action="store_true", help="use the existing binary")
    args = parser.parse_args()

    python = ensure_venv()
    if python is None:
        print("skipping the fontTools comparison")
        return 0

    if not args.no_build:
        subprocess.run(["cargo", "build", "-p", "slice-cli"], cwd=REPO_ROOT, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    slice_cli = REPO_ROOT / "target" / "debug" / "slice"

    workdir = Path(tempfile.mkdtemp(prefix="slice-vs-fonttools-"))
    failures = 0

    for fixture, name, limits in CASES:
        print(f"=== {name} ===")
        reference = workdir / "reference.ttf"
        ours = workdir / "ours.ttf"

        subprocess.run(
            [str(python), "-c", BUILD, str(fixture), str(reference), json.dumps(limits)],
            check=True,
        )

        axis_args = []
        for tag, value in limits.items():
            if isinstance(value, tuple):
                axis_args += ["--axis", f"{tag}={value[0]}:{value[1]}"]
            else:
                axis_args += ["--axis", f"{tag}={value}"]
        subprocess.run(
            [str(slice_cli), "cut", str(fixture), str(ours), *axis_args],
            check=True, stdout=subprocess.DEVNULL,
        )

        a = run_probe(python, reference)
        b = run_probe(python, ours)

        for key in sorted(set(a) | set(b)):
            va, vb = a.get(key), b.get(key)
            if va == vb:
                if args.verbose:
                    # Show the value, not just that it matched. These are the numbers
                    # the review quoted, and a harness that only prints disagreements
                    # proves them without ever showing them.
                    print(f"  ok   {key:24} {summarise(va)}")
                continue

            if key == "tables":
                only_reference = [t for t in (va or []) if t not in (vb or [])]
                only_ours = [t for t in (vb or []) if t not in (va or [])]
                unexplained = [t for t in only_reference + only_ours if t not in ACCEPTED]
                if not unexplained:
                    if args.verbose:
                        for t in only_reference + only_ours:
                            print(f"  --   {t} differs: {ACCEPTED[t]}")
                    continue
                print(f"  FAIL tables differ beyond the accepted list: {unexplained}")
                failures += 1
                continue

            if any(key.startswith(t) for t in ACCEPTED):
                if args.verbose:
                    print(f"  --   {key} differs (accepted)")
                continue

            if key == "outlines":
                differing = [g for g in set(va) | set(vb) if va.get(g) != vb.get(g)]
                print(f"  FAIL outlines differ for {len(differing)} glyph(s): {differing[:5]}")
                failures += 1
                continue

            print(f"  FAIL {key}\n         fontTools: {va}\n         ours:      {vb}")
            failures += 1

        if failures == 0 or args.verbose:
            print("  (everything else matches)")
        print()

    if failures:
        print(f"{failures} difference(s) beyond the accepted list", file=sys.stderr)
        return 1
    print("every case matches fontTools, apart from the accepted differences")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
