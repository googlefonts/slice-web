#!/usr/bin/env python3
"""Run the conformance corpus against one or both implementations.

    tests/suite/run.py                      both, full corpus
    tests/suite/run.py --runner ours
    tests/suite/run.py --case axis.         id prefix filter
    tests/suite/run.py --verbose            per-check detail for failures
    tests/suite/run.py --json report.json   machine-readable

Bootstraps its own virtual environment on first run, with PyQt5 (to drive the original)
and fontTools (to evaluate results). Re-executes itself inside it.

Scoring separates three outcomes for the original, because they mean different things:

    passed                  agrees with the corpus
    failed                  disagrees — either a defect in it, or a defect in the test,
                            and section 4 of the plan adjudicates which
    lacks feature           the case needs something that implementation does not have:
                            `original_lacks_feature` for overlap removal, which the
                            original never claimed to do, and `ours_lacks_feature` for a
                            job fontTools does and this build does not yet. Reported
                            separately so a known gap is visible rather than either
                            hidden or mistaken for a regression.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

SUITE = Path(__file__).resolve().parent
REPO = SUITE.parent.parent
VENV = REPO / ".suite-venv"
REQUIREMENTS = ["PyQt5==5.15.11", "fonttools[woff]==4.62.1", "brotli"]

FIXTURE_DIRS = [SUITE / "fixtures" / "out", REPO / "testdata" / "fonts"]
FIXTURE_ALIASES = {
    "recursive-vf": ["Recursive-VF.subset.ttf"],
    "avar2": ["RobotoDelta-avar2.subset.ttf"],
}
EXTENSIONS = [".ttf", ".otf", ".woff", ".woff2"]


# ------------------------------------------------------------------ bootstrapping

def ensure_venv() -> Path:
    python = VENV / "bin" / "python"
    if python.exists():
        return python
    print(f"creating {VENV.name} (PyQt5 + fontTools); this happens once")
    subprocess.run([sys.executable, "-m", "venv", str(VENV)], check=True)
    subprocess.run([str(python), "-m", "pip", "install", "-q", *REQUIREMENTS], check=True)
    return python


def reexec_if_needed() -> None:
    try:
        import fontTools  # noqa: F401
        import PyQt5  # noqa: F401
    except ImportError:
        python = ensure_venv()
        os.execv(str(python), [str(python), str(Path(__file__).resolve()), *sys.argv[1:]])


reexec_if_needed()

sys.path.insert(0, str(SUITE))
from checker import evaluate  # noqa: E402


# ----------------------------------------------------------------------- loading

def load_cases(filter_prefix: str | None) -> list[dict]:
    cases: list[dict] = []
    seen: set[str] = set()
    for path in sorted((SUITE / "cases").glob("*.json")):
        with open(path) as f:
            data = json.load(f)
        for case in data.get("cases", []):
            case.setdefault("_file", path.name)
            case.setdefault("_area", data.get("area", path.stem))
            if case["id"] in seen:
                raise SystemExit(f"duplicate case id {case['id']} in {path.name}")
            seen.add(case["id"])
            if filter_prefix and not case["id"].startswith(filter_prefix):
                continue
            cases.append(case)
    return cases


def resolve_fixture(name: str) -> Path | None:
    for directory in FIXTURE_DIRS:
        for candidate in FIXTURE_ALIASES.get(name, []):
            path = directory / candidate
            if path.exists():
                return path
        for extension in EXTENSIONS:
            path = directory / f"{name}{extension}"
            if path.exists():
                return path
    return None


# ----------------------------------------------------------------------- running

def run_case(case: dict, runner: str, workdir: Path) -> dict:
    fixture = resolve_fixture(case["fixture"])
    if fixture is None:
        return {"ok": False, "error": f"fixture {case['fixture']!r} not found"}

    suffix = {"woff": ".woff", "woff2": ".woff2", "otf": ".otf"}.get(
        case.get("input", {}).get("format", "ttf"), ".ttf"
    )
    output = workdir / f"{case['id'].replace('/', '_')}{suffix}"
    case_file = workdir / "case.json"
    case_file.write_text(json.dumps(case))

    script = SUITE / "runners" / f"{runner}.py"
    completed = subprocess.run(
        [sys.executable, str(script), str(case_file), str(fixture), str(output)],
        capture_output=True, text=True, timeout=600,
    )
    if completed.returncode != 0:
        return {"ok": False, "error": f"runner failed: {completed.stderr.strip()[:400]}"}
    try:
        return json.loads(completed.stdout.strip().splitlines()[-1])
    except Exception:  # noqa: BLE001
        return {"ok": False, "error": f"runner produced no result: {completed.stdout[:400]}"}


def score(cases: list[dict], runner: str, verbose: bool) -> dict:
    results = []
    workdir = Path(tempfile.mkdtemp(prefix=f"slice-suite-{runner}-"))
    try:
        # Run everything first, then evaluate. `matches_case_output` compares one case's
        # output against another's, and doing it in two passes means a case never has to
        # care whether the one it references has run yet.
        outcomes: dict[str, dict] = {}
        for case in cases:
            outcomes[case["id"]] = run_case(case, runner, workdir)
        peers = {
            case_id: outcome["path"]
            for case_id, outcome in outcomes.items()
            if outcome.get("ok") and outcome.get("path")
        }

        for case in cases:
            # A case can record that one implementation does not have the feature it
            # needs. `original_lacks_feature` is the common direction -- overlap removal,
            # which the original never had. `ours_lacks_feature` is the mirror, and it
            # exists so that a gap where fontTools is ahead is reported in the open rather
            # than smuggled in as a case that demands the refusal we happen to produce.
            lacks = (runner == "original" and case.get("original_lacks_feature")) or (
                runner == "ours" and case.get("ours_lacks_feature")
            )
            outcome = outcomes[case["id"]]
            fixture = resolve_fixture(case["fixture"])
            if fixture is None:
                results.append({
                    "id": case["id"], "area": case["_area"], "status": "no-fixture",
                    "reason": f"fixture {case['fixture']!r} not found",
                })
                continue
            verdict = evaluate(case, outcome, str(fixture), peers)
            status = "pass" if verdict.passed else ("lacks-feature" if lacks else "fail")
            entry = {
                "id": case["id"],
                "area": case["_area"],
                "status": status,
                "reason": verdict.reason,
                "covers": case.get("covers", []),
                "class": case.get("class"),
            }
            if verbose and not verdict.passed:
                entry["checks"] = [
                    {"kind": c.kind, "passed": c.passed, "detail": c.detail}
                    for c in verdict.checks
                ]
            results.append(entry)
    finally:
        shutil.rmtree(workdir, ignore_errors=True)
    return {"runner": runner, "results": results}


# ---------------------------------------------------------------------- reporting

def report(reports: list[dict], cases: list[dict], verbose: bool) -> bool:
    everything_expected_passed = True

    for r in reports:
        counts = Counter(x["status"] for x in r["results"])
        total = len(r["results"])
        passed = counts["pass"]
        print(f"\n=== {r['runner']} ===")
        print(f"  {passed}/{total} passed", end="")
        if counts["lacks-feature"]:
            print(f", {counts['lacks-feature']} need a feature it does not have", end="")
        if counts["fail"]:
            print(f", {counts['fail']} failed", end="")
        if counts["no-fixture"]:
            print(f", {counts['no-fixture']} skipped for a missing fixture", end="")
        print(f"   ({100 * passed / max(1, total - counts['no-fixture']):.1f}% of runnable)")

        by_area: dict[str, Counter] = {}
        for entry in r["results"]:
            by_area.setdefault(entry["area"], Counter())[entry["status"]] += 1
        for area in sorted(by_area):
            c = by_area[area]
            line = f"    {area:24} {c['pass']:3} pass"
            if c["fail"]:
                line += f"  {c['fail']:3} fail"
            if c["lacks-feature"]:
                line += f"  {c['lacks-feature']:3} n/a"
            if c["no-fixture"]:
                line += f"  {c['no-fixture']:3} skip"
            print(line)

        failures = [x for x in r["results"] if x["status"] == "fail"]
        if failures:
            print(f"\n  failures ({len(failures)}):")
            for entry in failures[: (None if verbose else 25)]:
                print(f"    {entry['id']}")
                print(f"      {entry['reason']}")
                for check in entry.get("checks", []):
                    if not check["passed"]:
                        print(f"        - {check['kind']}: {check['detail']}")
            if not verbose and len(failures) > 25:
                print(f"    ... and {len(failures) - 25} more; use --verbose")

        if r["runner"] == "ours" and counts["fail"]:
            everything_expected_passed = False

    # Coverage: every behavioural claim needs at least one case.
    covered = Counter()
    for case in cases:
        for claim in case.get("covers", []):
            covered[claim] += 1
    print(f"\n=== coverage ===")
    print(f"  {len(cases)} cases covering {len(covered)} behavioural claims")
    behaviour_doc = REPO / "docs" / "original-behaviour.md"
    if behaviour_doc.exists():
        import re
        declared = set(re.findall(r"^\*\*([A-Z]\d+)\*\*", behaviour_doc.read_text(), re.M))
        missing = sorted(declared - set(covered))
        if missing:
            print(f"  claims with no case: {', '.join(missing)}")
        else:
            print(f"  every one of the {len(declared)} declared claims has a case")

    return everything_expected_passed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner", choices=["original", "ours", "both"], default="both")
    parser.add_argument("--case", help="only cases whose id starts with this")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--json", help="write a machine-readable report here")
    args = parser.parse_args()

    cases = load_cases(args.case)
    if not cases:
        print("no cases found", file=sys.stderr)
        return 1
    print(f"{len(cases)} cases")

    runners = ["original", "ours"] if args.runner == "both" else [args.runner]
    reports = [score(cases, runner, args.verbose) for runner in runners]

    ok = report(reports, cases, args.verbose)

    if args.json:
        Path(args.json).write_text(json.dumps({"reports": reports}, indent=2))
        print(f"\nwrote {args.json}")

    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
