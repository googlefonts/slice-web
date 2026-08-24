#!/usr/bin/env python3
"""Does a font sliced here *render* the same as one sliced by fontTools?

Question this answers
---------------------
    The corpus compares outlines, advances and table fields. diffenator3 compares the
    rendered result: glyphs rasterised and diffed pixel by pixel, and words shaped through
    HarfBuzz and diffed the same way. That reaches things a structural comparison cannot
    -- kerning that no longer applies, a mark that lands in the wrong place, a shaping rule
    that stopped firing -- because it asks what someone setting type would actually see.

The comparison is **against fontTools' instance of the same job**, not against the input.
Instancing changes rendering by design: a static instance of Archivo at wght=600 differs
from the variable font drawn at wght=600 on seven currency glyphs, because instancing
rounds composite component offsets. fontTools' output has exactly those differences, so
they are a property of instancing rather than of this program. Diffing the two *instances*
against each other asks the question that is actually about Slice, and the answer should
be nothing at all.

`--vs-source` diffs against the original variable font instead, which shows what instancing
costs in general. That number is interesting but it is not a bug count.

Usage
-----
    tools/diffenator3-compare.py FONT [FONT ...]
    tools/diffenator3-compare.py --corpus /path/to/google/fonts --limit 20
    tools/diffenator3-compare.py FONT --job partial
    tools/diffenator3-compare.py FONT --json out.json

Needs `diffenator3` on the PATH, or `DIFFENATOR3` pointing at the binary.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SLICE = os.environ.get("SLICE_BINARY", str(REPO / "target" / "release" / "slice"))
DIFFENATOR = os.environ.get("DIFFENATOR3") or shutil.which("diffenator3")
VENV_PYTHON = REPO / ".suite-venv" / "bin" / "python"


def is_variable(path: Path) -> bool:
    try:
        with open(path, "rb") as f:
            header = f.read(12)
            if len(header) < 12:
                return False
            tag, count = header[:4], struct.unpack(">H", header[4:6])[0]
            if tag not in (b"\x00\x01\x00\x00", b"OTTO", b"true"):
                return False
            records = f.read(16 * count)
        return b"fvar" in {records[i:i + 4] for i in range(0, len(records) - 15, 16)}
    except OSError:
        return False


def axes_of(path: Path) -> list[dict]:
    out = subprocess.run([SLICE, "info", str(path), "--json"],
                         capture_output=True, text=True, timeout=300)
    if out.returncode != 0:
        return []
    try:
        return json.loads(out.stdout)["axes"]
    except Exception:  # noqa: BLE001
        return []


def jobs_for(axes: list[dict], which: str):
    """`(name, slice arguments, fontTools location, diff location)`."""
    jobs = []
    if which in ("both", "static"):
        jobs.append((
            "static",
            [f"{a['tag']}={a['default']}" for a in axes],
            {a["tag"]: float(a["default"]) for a in axes},
            None,  # no axes left to place the diff at
        ))
    if which in ("both", "partial") and axes:
        first = axes[0]
        low, high = float(first["default"]), float(first["max"])
        if high > low:
            middle = low + (high - low) / 2
            jobs.append((
                "partial",
                [f"{first['tag']}={low}:{high}"]
                + [f"{a['tag']}={a['default']}" for a in axes[1:]],
                {first["tag"]: (low, high),
                 **{a["tag"]: float(a["default"]) for a in axes[1:]}},
                # An interior location: the ends of a range are where two instancers are
                # most likely to agree by accident.
                f"{first['tag']}={middle:g}",
            ))
    return jobs


def fonttools_instance(font: Path, location: dict, target: Path) -> bool:
    script = (
        "import sys, json\n"
        "from fontTools.ttLib import TTFont\n"
        "from fontTools.varLib.instancer import instantiateVariableFont\n"
        "f = TTFont(sys.argv[1])\n"
        "loc = {k: tuple(v) if isinstance(v, list) else v\n"
        "       for k, v in json.loads(sys.argv[3]).items()}\n"
        "instantiateVariableFont(f, loc, inplace=True)\n"
        "f.save(sys.argv[2])\n"
    )
    python = str(VENV_PYTHON) if VENV_PYTHON.exists() else sys.executable
    run = subprocess.run([python, "-c", script, str(font), str(target),
                          json.dumps(location)],
                         capture_output=True, text=True, timeout=900)
    return run.returncode == 0 and target.exists()


def render_diff(a: Path, b: Path, location: str | None) -> tuple[int, int, list]:
    """(glyphs differing, words differing, the worst few)."""
    command = [DIFFENATOR, "--json", "--no-tables", "--no-kerns", "--no-languages"]
    if location:
        command += ["--location", location]
    command += [str(a), str(b)]
    run = subprocess.run(command, capture_output=True, text=True, timeout=1800)
    try:
        data = json.loads(run.stdout)
    except Exception:  # noqa: BLE001
        return -1, -1, []

    glyphs = words = 0
    worst = []
    for entry in data.get("locations", []):
        for item in entry.get("glyphs", []):
            glyphs += 1
            worst.append(("glyph", item.get("name", "?"), item.get("differing_pixels", 0)))
        for item in entry.get("words", []):
            words += 1
            # A word entry is sometimes a bare string and sometimes a record; take
            # whichever it is rather than assuming.
            if isinstance(item, dict):
                worst.append(("word", item.get("word", "?"),
                              item.get("differing_pixels", 0)))
            else:
                worst.append(("word", str(item), 0))
    worst.sort(key=lambda row: -row[2])
    return glyphs, words, worst[:6]


def outline_deviation(a: Path, b: Path, location: dict | None) -> float | None:
    """The largest coordinate difference between two fonts, in font units.

    diffenator3 counts differing *pixels*, which is the right measure for "would anyone
    see this" and the wrong one for "is this a defect": it rasterises large, so a quarter
    of a font unit lights up pixels. This answers the second question, so a report can say
    which kind of difference it found instead of leaving the reader to guess.
    """
    script = (
        "import json, sys\n"
        "from fontTools.ttLib import TTFont\n"
        "from fontTools.pens.recordingPen import DecomposingRecordingPen\n"
        "loc = json.loads(sys.argv[3])\n"
        "def draw(path):\n"
        "    f = TTFont(path)\n"
        "    gs = f.getGlyphSet(location=loc) if loc else f.getGlyphSet()\n"
        "    out = {}\n"
        "    for n in f.getGlyphOrder():\n"
        "        p = DecomposingRecordingPen(gs)\n"
        "        gs[n].draw(p)\n"
        "        out[n] = p.value\n"
        "    return out\n"
        "x, y = draw(sys.argv[1]), draw(sys.argv[2])\n"
        "worst = 0.0\n"
        "for g in x:\n"
        "    if g not in y or len(x[g]) != len(y[g]):\n"
        "        continue\n"
        "    for (o1, p1), (o2, p2) in zip(x[g], y[g]):\n"
        "        for q1, q2 in zip(p1, p2):\n"
        "            if q1 is None or q2 is None:\n"
        "                continue\n"
        "            for c1, c2 in zip(q1, q2):\n"
        "                worst = max(worst, abs(c1 - c2))\n"
        "print(worst)\n"
    )
    python = str(VENV_PYTHON) if VENV_PYTHON.exists() else sys.executable
    run = subprocess.run([python, "-c", script, str(a), str(b),
                          json.dumps(location or {})],
                         capture_output=True, text=True, timeout=900)
    try:
        return float(run.stdout.strip())
    except ValueError:
        return None


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("fonts", nargs="*", type=Path)
    parser.add_argument("--corpus", type=Path)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--job", choices=["both", "static", "partial"], default="both")
    parser.add_argument("--vs-source", action="store_true",
                        help="diff against the input font instead of fontTools' instance")
    parser.add_argument("--json", help="write the full comparison here")
    args = parser.parse_args()

    # Line-buffered: these runs take many minutes, and a log that only
    # appears at the end cannot be watched.
    sys.stdout.reconfigure(line_buffering=True)

    if not DIFFENATOR:
        raise SystemExit("diffenator3 not found; put it on PATH or set DIFFENATOR3")
    if not Path(SLICE).exists():
        raise SystemExit(f"{SLICE} is not built; run cargo build --release -p slice-cli")

    fonts = list(args.fonts)
    if args.corpus:
        for dirpath, dirnames, filenames in os.walk(args.corpus):
            dirnames[:] = [d for d in dirnames if d not in (".git", "venv", ".venv")]
            for name in sorted(filenames):
                if name.lower().endswith((".ttf", ".otf")):
                    path = Path(dirpath) / name
                    if is_variable(path):
                        fonts.append(path)
    if args.limit:
        fonts = fonts[: args.limit]
    if not fonts:
        raise SystemExit("no fonts given")

    version = subprocess.run([DIFFENATOR, "--version"], capture_output=True, text=True)
    against = "the source font" if args.vs_source else "fontTools' instance"
    print(f"{version.stdout.strip()}, comparing against {against}")
    print(f"{len(fonts)} font(s)\n")

    workdir = Path(tempfile.mkdtemp(prefix="diffenator3-compare-"))
    results = []
    differing = 0

    for n, font in enumerate(fonts, 1):
        axes = axes_of(font)
        if not axes:
            print(f"  {font.name}: could not read the axes, skipped")
            continue

        for job, arguments, location, diff_at in jobs_for(axes, args.job):
            ours = workdir / f"{n}-{job}-ours.ttf"
            run = subprocess.run(
                [SLICE, "cut", str(font), str(ours),
                 *sum(([f"--axis", a] for a in arguments), [])],
                capture_output=True, text=True, timeout=900,
            )
            if run.returncode != 0:
                message = (run.stderr or run.stdout).strip().splitlines()
                print(f"  -- {font.name} [{job}]: slice refused — "
                      f"{message[-1][:80] if message else '?'}")
                continue

            if args.vs_source:
                reference = font
            else:
                reference = workdir / f"{n}-{job}-fonttools.ttf"
                if not fonttools_instance(font, location, reference):
                    print(f"  -- {font.name} [{job}]: fontTools could not do this job")
                    continue

            glyphs, words, worst = render_diff(reference, ours, diff_at)
            if glyphs < 0:
                print(f"  -- {font.name} [{job}]: diffenator3 produced no report")
                continue

            results.append({"font": str(font), "job": job,
                            "glyphs": glyphs, "words": words, "worst": worst})
            if glyphs or words:
                coords = None
                if diff_at:
                    tag, _, value = diff_at.partition("=")
                    coords = {tag: float(value)}
                deviation = outline_deviation(reference, ours, coords)
                results[-1]["deviation"] = deviation

                # Under one font unit is the quantisation of a `gvar` delta: two
                # instancers rounding the same arithmetic differently, not a wrong
                # answer. Above it, something actually moved.
                sub_unit = deviation is not None and deviation < 1.0
                if sub_unit:
                    print(f"  ~~ {font.name} [{job}]: {glyphs} glyph(s), {words} word(s) "
                          f"differ, but by only {deviation:.3f} font units — "
                          f"delta rounding, not a wrong outline")
                else:
                    differing += 1
                    print(f"  !! {font.name} [{job}]: {glyphs} glyph(s), "
                          f"{words} word(s) differ"
                          + (f", by up to {deviation:.3f} font units"
                             if deviation is not None else ""))
                    for kind, name, pixels in worst:
                        print(f"        {kind:5} {name!r} — {pixels} pixels")
            else:
                print(f"  ok {font.name} [{job}]: renders identically")

    rounding = sum(1 for r in results
                   if (r.get("glyphs") or r.get("words"))
                   and r.get("deviation") is not None and r["deviation"] < 1.0)
    print(f"\n{differing} of {len(results)} job(s) render differently"
          + (f"; {rounding} more differ by under one font unit, which is delta rounding"
             if rounding else ""))
    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2))
        print(f"wrote {args.json}")
    shutil.rmtree(workdir, ignore_errors=True)
    return 1 if differing else 0


if __name__ == "__main__":
    raise SystemExit(main())
