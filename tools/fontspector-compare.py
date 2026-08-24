#!/usr/bin/env python3
"""Does slicing a font introduce problems fontspector can see?

Question this answers
---------------------
    Every check this project runs compares Slice against fontTools or against skrifa --
    against another implementation of the same idea. fontspector asks a different
    question: is the *result* a good font? It knows about hundreds of conventions and
    requirements neither of those oracles has any opinion on.

The measurement is a **delta**, not an absolute. Real fonts already fail checks -- a
family may ship with a known metrics warning for years -- so "the sliced font fails 14
checks" says nothing on its own. What matters is a check that passed on the input and
does not pass on the output: that is a problem this tool introduced.

The reverse is recorded too. A check that fails on the input and passes on the output is
usually instancing legitimately fixing something (`fvar` conventions stop applying to a
static font), and is worth seeing rather than hiding.

There is a third leg, and it is what makes the number trustworthy: fontTools is asked for
the *same* slice, and fontspector is run on that too. A problem both produce is inherent
to the request, not to this implementation. Restricting Archivo's weight to 600:900 drops
its "Regular" instance, because Archivo's weight default is 600 and 400 is no longer in
the design space -- fontspector rightly says so, and fontTools' output says it just the
same. Counting that against Slice would be measuring the request. `--no-reference` turns
the leg off and reports the raw delta.

Usage
-----
    tools/fontspector-compare.py FONT [FONT ...]
    tools/fontspector-compare.py --corpus /path/to/google/fonts --limit 25
    tools/fontspector-compare.py FONT --job static      # only the static instance
    tools/fontspector-compare.py FONT --json out.json

Needs `fontspector` on the PATH, or `FONTSPECTOR` pointing at the binary. Network checks
are skipped: they ask about a font's presence on Google Fonts, which is a fact about the
input and is neither true nor false of a slice of it.
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
#: Seconds to give fontspector per font, after which it is skipped.
TIMEOUT = int(os.environ.get("FONTSPECTOR_TIMEOUT", "600"))
FONTSPECTOR = os.environ.get("FONTSPECTOR") or shutil.which("fontspector")

#: Checks whose result is a fact about the *file on disk*, not about the font's quality,
#: and which therefore say nothing useful about a slice. Each is excluded for a reason.
IRRELEVANT = {
    # Asks whether this font is on Google Fonts. A slice of it is not, and should not be.
    "com.google.fonts/check/fontdata_namecheck",
    "googlefonts/metadata/parses",
    "googlefonts/metadata/designer_profiles",
    # Wants a METADATA.pb, DESCRIPTION.en_us.html and so on beside the font. A file in a
    # temporary directory has none, and neither does anything a user saves from a browser.
    "googlefonts/metadata/included_fonts",
    "googlefonts/description/family_update",
}


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


def run_fontspector(paths: list[Path], workdir: Path, label: str) -> dict | None:
    """`{check_id: worst_status}` for the given fonts."""
    report = workdir / f"{label}.json"
    try:
        subprocess.run(
            [FONTSPECTOR, "--skip-network", "-q", "--json", str(report),
             "-l", "info", *[str(p) for p in paths]],
            capture_output=True, text=True, timeout=TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        # Some fonts are simply slow to check -- an 11-axis Bitcount takes fontspector
        # past fifteen minutes. That is a fact about fontspector, not about the font or
        # about this program, so the font is skipped rather than allowed to end the run.
        return None
    if not report.exists():
        return None

    data = json.loads(report.read_text())
    # `{"summary": {...}, "results": {filename: {section: [check, ...]}}}`. The worst
    # status per check id is kept, since a check can report per-file and this compares
    # one font against one font.
    worst: dict[str, str] = {}
    rank = {"SKIP": 0, "PASS": 1, "INFO": 2, "WARN": 3, "FAIL": 4, "FATAL": 5, "ERROR": 6}
    for sections in data.get("results", {}).values():
        for checks in sections.values():
            for check in checks:
                key = check.get("check_id")
                if not key or key in IRRELEVANT:
                    continue
                status = (check.get("worst_status") or "SKIP").upper()
                if rank.get(status, 0) > rank.get(worst.get(key, "SKIP"), 0):
                    worst[key] = status
    return worst


def fonttools_instance(font: Path, arguments: list[str], workdir: Path,
                       label: str) -> Path | None:
    """The same slice, done by fontTools, or None if it cannot do it."""
    location: dict = {}
    for argument in arguments:
        tag, _, value = argument.partition("=")
        if ":" in value:
            low, _, high = value.partition(":")
            location[tag] = (float(low), float(high))
        else:
            location[tag] = float(value)

    output = workdir / f"{label}-fonttools.ttf"
    script = (
        "import sys\n"
        "from fontTools.ttLib import TTFont\n"
        "from fontTools.varLib.instancer import instantiateVariableFont\n"
        "import json\n"
        "f = TTFont(sys.argv[1])\n"
        "instantiateVariableFont(f, json.loads(sys.argv[3]), inplace=True)\n"
        "f.save(sys.argv[2])\n"
    )
    python = REPO / ".suite-venv" / "bin" / "python"
    run = subprocess.run(
        [str(python) if python.exists() else sys.executable, "-c", script,
         str(font), str(output), json.dumps(location)],
        capture_output=True, text=True, timeout=900,
    )
    return output if run.returncode == 0 and output.exists() else None


def jobs_for(axes: list[dict], which: str) -> list[tuple[str, list[str]]]:
    pinned = [f"{a['tag']}={a['default']}" for a in axes]
    jobs = []
    if which in ("both", "static"):
        jobs.append(("static", pinned))
    if which in ("both", "partial") and axes:
        first = axes[0]
        low, high = float(first["default"]), float(first["max"])
        if high > low:
            rest = [f"{a['tag']}={a['default']}" for a in axes[1:]]
            jobs.append(("partial", [f"{first['tag']}={low}:{high}"] + rest))
    return jobs


BAD = {"FAIL", "FATAL", "ERROR"}


def compare(before: dict, after: dict) -> tuple[list, list]:
    """(introduced, resolved), each `(check, before, after)`."""
    introduced, resolved = [], []
    for key in sorted(set(before) | set(after)):
        was, now = before.get(key, "SKIP"), after.get(key, "SKIP")
        if was == now:
            continue
        # Only a move into or out of a *problem* counts. PASS -> INFO is noise.
        if now in BAD and was not in BAD:
            introduced.append((key, was, now))
        elif was in BAD and now not in BAD:
            resolved.append((key, was, now))
        elif now == "WARN" and was in ("PASS", "SKIP"):
            introduced.append((key, was, now))
        elif was == "WARN" and now in ("PASS", "SKIP"):
            resolved.append((key, was, now))
    return introduced, resolved


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("fonts", nargs="*", type=Path)
    parser.add_argument("--corpus", type=Path, help="walk this directory for variable fonts")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--job", choices=["both", "static", "partial"], default="both")
    parser.add_argument("--json", help="write the full comparison here")
    parser.add_argument("--no-reference", action="store_true",
                        help="do not discount problems fontTools' own output also has")
    args = parser.parse_args()

    # Line-buffered: these runs take many minutes, and a log that only
    # appears at the end cannot be watched.
    sys.stdout.reconfigure(line_buffering=True)

    if not FONTSPECTOR:
        raise SystemExit("fontspector not found; put it on PATH or set FONTSPECTOR")
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

    print(f"fontspector: {subprocess.run([FONTSPECTOR, '--version'], capture_output=True, text=True).stdout.strip()}")
    print(f"{len(fonts)} font(s)\n")

    workdir = Path(tempfile.mkdtemp(prefix="fontspector-compare-"))
    results = []
    total_introduced = 0
    skipped = 0

    for n, font in enumerate(fonts, 1):
        axes = axes_of(font)
        if not axes:
            print(f"  {font.name}: could not read the axes, skipped")
            continue
        before = run_fontspector([font], workdir, f"{n}-before")
        if before is None:
            print(f"  -- {font.name}: fontspector timed out on the input, skipped")
            skipped += 1
            continue

        for job, arguments in jobs_for(axes, args.job):
            output = workdir / f"{n}-{job}-{font.stem}.ttf"
            run = subprocess.run(
                [SLICE, "cut", str(font), str(output),
                 *sum(([f"--axis", a] for a in arguments), [])],
                capture_output=True, text=True, timeout=600,
            )
            if run.returncode != 0:
                message = (run.stderr or run.stdout).strip().splitlines()
                print(f"  {font.name} [{job}]: slice refused — "
                      f"{message[-1][:90] if message else '?'}")
                continue

            after = run_fontspector([output], workdir, f"{n}-{job}-after")
            if after is None:
                print(f"  -- {font.name} [{job}]: fontspector timed out, skipped")
                skipped += 1
                continue
            introduced, resolved = compare(before, after)

            # Discount anything fontTools' own instance of the same job also produces.
            shared = []
            if introduced and not args.no_reference:
                reference = fonttools_instance(font, arguments, workdir, f"{n}-{job}")
                if reference is not None:
                    theirs = run_fontspector([reference], workdir, f"{n}-{job}-ref")
                    their_problems, _ = compare(before, theirs or {})
                    their_keys = {key for key, _, _ in their_problems}
                    shared = [row for row in introduced if row[0] in their_keys]
                    introduced = [row for row in introduced if row[0] not in their_keys]

            total_introduced += len(introduced)
            results.append({
                "font": str(font), "job": job,
                "introduced": introduced, "resolved": resolved,
                "shared_with_fonttools": shared,
            })

            mark = "!!" if introduced else "ok"
            print(f"  {mark} {font.name} [{job}]: "
                  f"{len(introduced)} introduced, {len(resolved)} resolved")
            for key, was, now in introduced:
                print(f"        INTRODUCED  {was} -> {now}  {key}")
            for key, was, now in resolved:
                print(f"        resolved    {was} -> {now}  {key}")
            for key, was, now in shared:
                print(f"        (fontTools' output has this too: {was} -> {now}  {key})")

    print(f"\n{total_introduced} problem(s) introduced across {len(results)} job(s)"
          + (f", {skipped} skipped for timeouts" if skipped else ""))
    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2))
        print(f"wrote {args.json}")
    shutil.rmtree(workdir, ignore_errors=True)
    return 1 if total_introduced else 0


if __name__ == "__main__":
    raise SystemExit(main())
