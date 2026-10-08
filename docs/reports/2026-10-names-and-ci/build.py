#!/usr/bin/env python3
"""Build the one-page summary of the follow-up fixes (October 2026).

Question this answers
---------------------
    Why did a user's Regular weight fail Font Book, why had CI never finished, and
    what does each look like before and after the fix?

Everything on the page is measured when it is built, from two builds of the web app,
each driven in headless Chromium:

* the user's case: Google Sans Flex sliced as a condensed Regular, with names 1, 3, 4 and
  6 typed and the subfamily left as the font's own "Regular" -- the saved font's name ID 2
  and fsSelection, read straight out of the bytes;
* the bundled Recursive sliced with every axis pinned and nothing else touched -- every
  name and both style fields of the saved font, against what the editors showed;
* the Regular's own link reopened, and what the Subfamily cell then says;
* CI's run history, from `ci-runs.json` -- a snapshot of the GitHub API, refreshed with
  --refresh-ci.

Usage
-----
    docs/reports/2026-10-names-and-ci/build.py \\
        --gsf 'GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf' \\
        --before-dist ../slice-before/dist --after-dist dist

The two dist directories are `./build.sh` output: `--before-dist` from e7d44f7, the last
build before the name fixes (a `git worktree` gives one), `--after-dist` from this
checkout.
Writes one-pager.pdf beside this script and a PNG preview with --png. Needs chromium
and target/release/slice; nothing else outside the standard library.
"""

from __future__ import annotations

import argparse
import base64
import datetime
import html
import importlib.util
import json
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
SLICE = REPO / "target" / "release" / "slice"
CI_RUNS = HERE / "ci-runs.json"
CI_API = ("https://api.github.com/repos/googlefonts/slice-web/actions/workflows/ci.yml/"
          "runs?per_page=100")

# The condensed Regular's names, as the user typed them. The subfamily stays the font's
# own "Regular", so the link the app writes leaves it out.
REGULAR_NAMES = {
    1: "Google Sans Flex Condensed",
    3: "4.005;GOOG;GoogleSansFlex-Condensed-Regular",
    4: "Google Sans Flex Condensed Regular",
    6: "GoogleSansFlex-Condensed-Regular",
}
REGULAR_AXES = "opsz=18,wdth=80,wght=400,GRAD=0,ROND=0,slnt=0"
SAMPLE_AXES = "MONO=0,CASL=0,wght=400,slnt=0,CRSV=0.5"
ROW_LABELS = {1: "Family", 2: "Subfamily", 3: "Unique", 4: "Full", 6: "PostScript",
              16: "Typo Family", 17: "Typo Subfamily", 21: "WWS Family", 22: "WWS Subfamily"}
NAME_ROWS = [1, 2, 3, 4, 6, 16, 17, 21, 22]


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


first = load(HERE.parent / "2026-10-google-sans-flex" / "build.py", "first_report")
bst = load(REPO / "tools" / "browser-slice-test.py", "browser_slice_test")
INK, SOFT, FAINT, LINE = first.INK, first.SOFT, first.FAINT, first.LINE
ACCENT, ACCENT_SOFT, BAD, GOOD = first.ACCENT, first.ACCENT_SOFT, first.BAD, first.GOOD


# --------------------------------------------------------------------------- measuring

def name_record(font: bytes, name_id: int) -> str | None:
    """The 3/1/0x409 string for `name_id`, read from the bytes; None when absent."""
    count = struct.unpack(">H", font[4:6])[0]
    for i in range(count):
        tag, _, offset, _ = struct.unpack(">4sIII", font[12 + 16 * i: 28 + 16 * i])
        if tag == b"name":
            break
    else:
        return None
    _, records, strings = struct.unpack(">HHH", font[offset: offset + 6])
    for i in range(records):
        plat, enc, lang, nid, length, start = struct.unpack(
            ">HHHHHH", font[offset + 6 + 12 * i: offset + 18 + 12 * i])
        if (plat, enc, lang, nid) == (3, 1, 0x409, name_id):
            raw = font[offset + strings + start: offset + strings + start + length]
            return raw.decode("utf-16-be")
    return None


class Browser:
    def __init__(self, workdir: Path):
        self.port = bst.free_port()
        self.chrome = subprocess.Popen(
            [bst.find_browser(), "--headless", "--disable-gpu", "--no-sandbox",
             f"--remote-debugging-port={self.port}", f"--user-data-dir={workdir / 'profile'}",
             "--no-first-run", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        target = {}

        def up() -> bool:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/json/list",
                                            timeout=1) as response:
                    for page in json.load(response):
                        if page.get("type") == "page":
                            target.update(page)
                            return True
            except Exception:
                pass
            return False

        bst.wait_for(up, "the browser")
        self.page = bst.Devtools(target["webSocketDebuggerUrl"])
        for domain in ("Page", "Runtime", "DOM"):
            self.page.call(f"{domain}.enable")
        self.page.call("Emulation.setDeviceMetricsOverride", width=980, height=1400,
                       deviceScaleFactor=2, mobile=False)

    def open(self, url: str, font: Path | None):
        self.page.call("Page.navigate", url=url)
        bst.wait_for(lambda: self.page.evaluate(
            "!!document.querySelector('input[type=file]')", timeout=10), "the page", 60)
        if font is not None:
            root = self.page.call("DOM.getDocument")["root"]["nodeId"]
            node = self.page.call("DOM.querySelector", nodeId=root,
                                  selector="input[type=file]")["nodeId"]
            self.page.call("DOM.setFileInputFiles", nodeId=node, files=[str(font.resolve())])
        # The Name Editor shows its rows before any font is loaded; the Axis Editor's
        # rows are what says the font has arrived.
        bst.wait_for(lambda: self.page.evaluate(
            "document.querySelectorAll('.axis-editor tbody tr').length", timeout=10) > 0,
            "the font", 120)

    def names(self) -> list[str]:
        return self.page.evaluate(
            "[...document.querySelectorAll('.name-editor tbody tr input')].map(i => i.value)")

    def type_names(self, names: dict[int, str]):
        for name_id, value in names.items():
            row = NAME_ROWS.index(name_id) + 1
            self.page.evaluate(f"({bst.SET_INPUT})('.name-editor tbody tr:nth-child({row}) "
                               f"input', {json.dumps(value)})")

    def type_axes(self, axes: str):
        for index, item in enumerate(axes.split(",")):
            self.page.evaluate(f"({bst.SET_INPUT})('.axis-editor tbody tr:nth-child("
                               f"{index + 1}) input', {json.dumps(item.split('=')[1])})")

    def slice(self) -> bytes:
        self.page.evaluate(bst.CAPTURE_DOWNLOAD)
        self.page.evaluate("document.querySelector('button.slice').click()")
        bst.wait_for(lambda: self.page.evaluate("!!window.__sliceCaptured", timeout=20),
                     "the slice", 120, 1.0)
        return base64.b64decode(self.page.evaluate(bst.READ_CAPTURED, timeout=300))

    def close(self):
        self.page.close()
        self.chrome.terminate()
        self.chrome.wait(timeout=30)


def serve(directory: Path) -> tuple[subprocess.Popen, str]:
    port = bst.free_port()
    server = subprocess.Popen([sys.executable, "-m", "http.server", "--directory",
                               str(directory), str(port)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/index.html"

    def up() -> bool:
        try:
            urllib.request.urlopen(url, timeout=1)
            return True
        except Exception:
            return False

    bst.wait_for(up, f"the server for {directory}")
    return server, url


def style_bits(font: bytes) -> tuple[str, str]:
    """fsSelection and macStyle of an sfnt, as 16-character binary strings."""
    fields = {}
    count = struct.unpack(">H", font[4:6])[0]
    for i in range(count):
        tag, _, offset, _ = struct.unpack(">4sIII", font[12 + 16 * i: 28 + 16 * i])
        if tag == b"OS/2":
            fields["fs"] = struct.unpack(">H", font[offset + 62: offset + 64])[0]
        if tag == b"head":
            fields["mac"] = struct.unpack(">H", font[offset + 44: offset + 46])[0]
    return format(fields.get("fs", 0), "016b"), format(fields.get("mac", 0), "016b")


def bit_names(field: str) -> str:
    """The fsSelection bits set in a 16-character binary string, by name."""
    names = {0: "ITALIC", 5: "BOLD", 6: "REGULAR", 7: "USE_TYPO_METRICS", 8: "WWS",
             9: "OBLIQUE"}
    value = int(field, 2)
    found = [label for bit, label in names.items() if value >> bit & 1]
    return " · ".join(found) if found else "none"


def sample_bits(dist: Path) -> tuple[str, str]:
    """The bundled sample's own fsSelection and macStyle, via `slice info` (it is WOFF2)."""
    sample = next((dist / "fonts").glob("*.woff2"))
    report = subprocess.run([str(SLICE), "info", str(sample)], capture_output=True,
                            text=True, check=True).stdout
    fs = re.search(r"OS/2\.fsSelection\s+([01]{16})", report).group(1)
    mac = re.search(r"head\.macStyle\s+([01]{16})", report).group(1)
    return fs, mac


def measure(dist: Path, gsf: Path, workdir: Path) -> dict:
    """What one build saves when names are typed, when nothing is edited, and what it
    does with its own link reopened."""
    server, base = serve(dist)
    browser = Browser(workdir / dist.name)
    try:
        # The user's Regular: type four names, leave the subfamily as the font's "Regular",
        # slice once.
        browser.open(base, gsf)
        browser.type_axes(REGULAR_AXES)
        browser.type_names(REGULAR_NAMES)
        regular = browser.slice()
        link = browser.page.evaluate("location.search")

        # The second path: reopen the link the app just wrote.
        browser.open(base + link, gsf)
        bst.wait_for(lambda: browser.names()[0] == REGULAR_NAMES[1], "the link's names", 60)
        reopened_subfamily = browser.names()[1]

        # The bundled Recursive with every axis pinned and nothing else touched.
        browser.open(base + "?sample", None)
        browser.type_axes(SAMPLE_AXES)
        sample_cells = browser.names()
        sample = browser.slice()
        stamp = " ".join(browser.page.evaluate(
            "document.querySelector('.statusbar').innerText").split())
    finally:
        browser.close()
        server.terminate()
    return {
        "stamp": stamp,
        "link": link,
        "regular_id2": name_record(regular, 2),
        "regular_bits": style_bits(regular),
        "reopened_subfamily": reopened_subfamily,
        "sample_cells": dict(zip(NAME_ROWS, sample_cells)),
        "sample_names": {row: name_record(sample, row) for row in NAME_ROWS},
        "sample_bits": style_bits(sample),
    }


def ci_history(refresh: bool) -> list[dict]:
    if refresh:
        with urllib.request.urlopen(CI_API, timeout=30) as response:
            runs = json.load(response)["workflow_runs"]
        kept = [{"sha": r["head_sha"][:7], "conclusion": r["conclusion"],
                 "created": r["created_at"], "updated": r["updated_at"]}
                for r in runs if r["status"] == "completed"]
        kept.sort(key=lambda r: r["created"])
        CI_RUNS.write_text(json.dumps(kept, indent=1) + "\n")
    return json.loads(CI_RUNS.read_text())


# --------------------------------------------------------------------------- drawing

def duration(run: dict) -> float:
    parse = lambda s: datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))  # noqa: E731
    return (parse(run["updated"]) - parse(run["created"])).total_seconds()


def clock(seconds: float) -> str:
    seconds = int(round(seconds))
    if seconds >= 3600:
        return f"{seconds // 3600} h {seconds % 3600 // 60:02d} min"
    return f"{seconds // 60} min {seconds % 60:02d} s"


def ci_chart(runs: list[dict]) -> str:
    """One bar per CI run, on a linear axis to six hours."""
    width, left, bar_h, gap, room = 470, 92, 10, 3, 112
    limit = 6.1 * 3600
    rows = []
    for i, run in enumerate(runs):
        y = 14 + i * (bar_h + gap)
        seconds = duration(run)
        w = max(2.0, (width - left - room) * seconds / limit)
        ok = run["conclusion"] == "success"
        colour = GOOD if ok else BAD
        label = f"{clock(seconds)}, {'passed' if ok else 'cancelled'}"
        rows.append(
            f'<text x="{left - 6}" y="{y + 9}" text-anchor="end" font-size="8.4" '
            f'fill="{SOFT}">{run["created"][5:10]} {run["sha"]}</text>'
            f'<rect x="{left}" y="{y}" width="{w:.1f}" height="{bar_h}" rx="2" '
            f'fill="{colour}" fill-opacity="{0.85 if ok else 0.55}"/>'
            f'<text x="{left + w + 5:.1f}" y="{y + 9}" font-size="8.4" fill="{colour}" '
            f'font-family="BodyBold, sans-serif">{label}</text>')
    height = 14 + len(runs) * (bar_h + gap) + 14
    six = left + (width - left - room) * 6 * 3600 / limit
    return (f'<svg viewBox="0 0 {width} {height}" width="100%" xmlns="http://www.w3.org/2000/svg"'
            f' font-family="Body, sans-serif">'
            f'<line x1="{six:.1f}" y1="8" x2="{six:.1f}" y2="{height - 10}" stroke="{FAINT}" '
            f'stroke-dasharray="3 2"/><text x="{six:.1f}" y="{height - 1}" text-anchor="middle"'
            f' font-size="8" fill="{FAINT}">GitHub’s six-hour limit</text>'
            f'<text x="{left}" y="{height - 1}" font-size="8" fill="{FAINT}">0</text>'
            + "".join(rows) + "</svg>")


def loop_diagram() -> str:
    """run.py re-executing itself into a venv that cannot satisfy it."""
    return f'''<svg viewBox="0 0 470 100" width="100%" xmlns="http://www.w3.org/2000/svg"
      font-family="Body, sans-serif" font-size="9.4" fill="{SOFT}">
      <defs><marker id="a2" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6"
        markerHeight="6" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="{SOFT}"/></marker>
        <marker id="a3" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6"
        markerHeight="6" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="{BAD}"/></marker></defs>
      <rect x="2" y="6" width="104" height="34" rx="5" fill="#fff" stroke="{LINE}"/>
      <text x="54" y="20" text-anchor="middle" fill="{INK}" font-family="BodyBold, sans-serif">run.py</text>
      <text x="54" y="33" text-anchor="middle">import PyQt5 → fails</text>
      <line x1="106" y1="23" x2="150" y2="23" stroke="{SOFT}" marker-end="url(#a2)"/>
      <rect x="152" y="6" width="150" height="34" rx="5" fill="#fff" stroke="{LINE}"/>
      <text x="227" y="20" text-anchor="middle" fill="{INK}" font-family="BodyBold, sans-serif">.suite-venv exists?</text>
      <text x="227" y="33" text-anchor="middle">yes → re-execute inside it</text>
      <line x1="302" y1="23" x2="346" y2="23" stroke="{SOFT}" marker-end="url(#a2)"/>
      <rect x="348" y="6" width="120" height="34" rx="5" fill="{ACCENT_SOFT}" stroke="{ACCENT}"/>
      <text x="408" y="20" text-anchor="middle" fill="{INK}" font-family="BodyBold, sans-serif">venv: fontTools only</text>
      <text x="408" y="33" text-anchor="middle">PyQt5 still missing</text>
      <path d="M408,40 C408,62 54,62 54,44" fill="none" stroke="{BAD}" stroke-width="1.6"
        marker-end="url(#a3)"/>
      <text x="231" y="76" text-anchor="middle" fill="{BAD}" font-family="BodyBold, sans-serif">again — 141 times in 10 s, for six hours</text>
      <text x="2" y="96" fill="{GOOD}"><tspan font-family="BodyBold, sans-serif">Now:</tspan>
        install what the venv lacks, re-execute once at most; CI also checks out the original Slice.</text>
    </svg>'''


def saved_cell(value, editor: str) -> str:
    """One saved value, compared with what the Name Editor showed."""
    if value is None:
        return f'<td class="bad">deleted</td>' if editor else '<td class="dim">—</td>'
    if value == "" and editor:
        return '<td class="bad">“” empty</td>'
    return f'<td class="ok">{html.escape(value)}</td>'


def names_table(before: dict, after: dict, source_bits: tuple[str, str]) -> str:
    rows = []
    for row in [1, 2, 3, 4, 6, 16, 17]:
        editor = after["sample_cells"].get(row, "")
        rows.append(f'<tr><th>{row:02d} {ROW_LABELS[row]}</th>'
                    f'<td>{html.escape(editor) or "—"}</td>'
                    f'{saved_cell(before["sample_names"][row], editor)}'
                    f'{saved_cell(after["sample_names"][row], editor)}</tr>')
    for index, label in enumerate(["OS/2 fsSelection", "head macStyle"]):
        want = source_bits[index]
        cells = []
        for build in (before, after):
            got = build["sample_bits"][index]
            cls = "ok" if got == want else "bad"
            cells.append(f'<td class="{cls} mono">{got}</td>')
        rows.append(f'<tr><th>{label}</th><td class="mono">{want}</td>{"".join(cells)}</tr>')
    return ('<table class="names"><thead><tr><th>nothing edited</th>'
            '<th>editor showed</th>'
            f'<th class="bad">saved, {html.escape(before["stamp"][-7:])}</th>'
            f'<th class="good">saved, {html.escape(after["stamp"][-7:])}</th></tr></thead>'
            f'<tbody>{"".join(rows)}</tbody></table>').replace('<table class="names">', '<table class="names"><colgroup><col class="first"><col><col><col></colgroup>', 1)


def page(work: Path, before: dict, after: dict, runs: list[dict],
         source_bits: tuple[str, str], gsf_bits: tuple[str, str]) -> str:
    icon = (REPO / "docs" / "assets" / "slice-icon.svg").read_text()
    icon = icon[icon.index("<svg"):].replace('width="75px" height="60px"',
                                             'width="44" height="35"')
    faces = "\n".join([first.font_face("Body", work / "text-400.ttf"),
                       first.font_face("BodyBold", work / "text-700.ttf"),
                       first.font_face("Black", work / "black.ttf")])
    hung = [r for r in runs if r["conclusion"] != "success"]
    passed = [r for r in runs if r["conclusion"] == "success"]
    old, new = before["stamp"][-7:], after["stamp"][-7:]

    def text(value):
        return "absent" if value is None else (f"“{html.escape(value)}”" if value else "“”")

    lost = sum(1 for row in (1, 2, 3, 4, 6, 16, 17)
               if before["sample_cells"].get(row) and before["sample_names"][row] != before["sample_cells"][row])
    lost_now = sum(1 for row in (1, 2, 3, 4, 6, 16, 17)
                   if after["sample_cells"].get(row) and after["sample_names"][row] != after["sample_cells"][row])

    return f"""<!doctype html><html><head><meta charset="utf-8">
<title>Slice — two more fixes and a move</title>
<style>
{faces}
@page {{ size: A4; margin: 0; }}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; }}
body {{ width: 210mm; height: 296mm; padding: 10mm 12mm 8mm; font-family: Body, sans-serif;
  font-size: 8.6pt; line-height: 1.32; color: {INK}; background: #fff;
  display: flex; flex-direction: column; gap: 3mm; }}
b, strong {{ font-family: BodyBold, sans-serif; font-weight: normal; }}
p {{ margin: 0; }}
header {{ display: flex; align-items: center; gap: 4mm; }}
h1 {{ font-family: Black, sans-serif; font-weight: normal; font-size: 24pt; line-height: 1;
  margin: 0; }}
.sub {{ color: {SOFT}; margin-top: 1.2mm; }}
.meta {{ margin-left: auto; text-align: right; color: {FAINT}; font-size: 7.4pt;
  white-space: nowrap; }}
section {{ border: 1px solid {LINE}; border-radius: 3mm; padding: 2.8mm 4mm 3mm;
  display: grid; gap: 2mm 6mm; align-items: start; }}
h2 {{ font-family: Black, sans-serif; font-weight: normal; font-size: 13pt;
  line-height: 1.05; margin: 0 0 0.6mm; grid-column: 1 / -1; }}
h2 .no {{ color: {ACCENT}; margin-right: 1.6mm; }}
.cap {{ color: {SOFT}; font-size: 7.4pt; }}
.bad {{ color: {BAD}; }} .good {{ color: {GOOD}; }}
.k {{ font-family: BodyBold, sans-serif; }}
.col {{ display: flex; flex-direction: column; gap: 1.8mm; }}
.quote {{ border-left: 2.4px solid {BAD}; padding: 0.4mm 0 0.4mm 2.4mm; }}
table.names {{ border-collapse: collapse; width: 100%; font-size: 7.5pt; table-layout: fixed; }}
table.names th, table.names td {{ border-bottom: 1px solid {LINE}; padding: 0.9mm 1.4mm;
  text-align: left; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
table.names thead th {{ font-family: BodyBold, sans-serif; font-size: 6.8pt;
  letter-spacing: 0.04em; text-transform: uppercase; color: {FAINT}; }}
table.names thead th.bad {{ color: {BAD}; }} table.names thead th.good {{ color: {GOOD}; }}
table.names tbody th {{ font-family: BodyBold, sans-serif; font-weight: normal; }}
table.names col.first {{ width: 25%; }}
table.names td.bad {{ background: #fdecec; font-family: BodyBold, sans-serif; }}
table.names td.ok {{ color: {INK}; }} table.names td.dim {{ color: {FAINT}; }}
table.names .mono {{ font-size: 6.6pt; letter-spacing: 0.02em; }}
.case {{ border: 1px solid {LINE}; border-radius: 2mm; padding: 1.6mm 2.2mm; }}
.case .row {{ display: grid; grid-template-columns: 17mm 1fr; gap: 1mm; }}
.label {{ font-family: BodyBold, sans-serif; font-size: 7pt; letter-spacing: 0.05em;
  text-transform: uppercase; color: {FAINT}; }}
.case .row + .row {{ margin-top: 0.8mm; }}
.stats {{ display: flex; flex-direction: column; gap: 1.8mm; }}
.stat {{ display: grid; grid-template-columns: 1fr; gap: 0.6mm; }}
.big {{ font-family: Black, sans-serif; font-size: 15pt; line-height: 1; white-space: nowrap; }}
.big .to {{ color: {FAINT}; font-family: Body, sans-serif; font-size: 11pt; padding: 0 0.6mm; }}
.s1 {{ grid-template-columns: 1fr 58mm; }}
.s2 {{ grid-template-columns: 1fr 52mm; }}
.s3 {{ grid-template-columns: 1fr 1fr; }}
footer {{ margin-top: auto; color: {SOFT}; font-size: 7.2pt; border-top: 1px solid {LINE};
  padding-top: 2mm; display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 5mm; }}
footer b {{ color: {INK}; }}
code {{ font-family: BodyBold, sans-serif; font-size: 0.95em; color: {INK}; }}
</style></head><body>

<header>
  {icon}
  <div>
    <h1>Two more fixes, and a move</h1>
    <div class="sub">After the first three fixes shipped, the same user’s Regular weight
      failed Font Book, and checking that the fixes had landed showed CI had never once
      finished.</div>
  </div>
  <div class="meta">slice-web<br><b>e7d44f7 → {html.escape(new)}</b><br>8 October 2026</div>
</header>

<section class="s1">
  <h2><span class="no">4</span>Names nobody edited were saved empty</h2>
  <div class="col">
    {names_table(before, after, source_bits)}
    <p class="cap">Measured by driving both builds in a browser: slice the bundled font
      with every axis pinned and nothing else touched, then read the saved file.</p>
    <div class="label">The user’s condensed Regular: four names typed, Subfamily left as
      the font’s “Regular”, sliced once</div>
    <div class="case">
      <div class="row"><span class="k">Subfamily</span><span>editor “Regular” → saved
        <span class="bad">{text(before["regular_id2"])}</span>, now
        <span class="good">{text(after["regular_id2"])}</span></span></div>
      <div class="row"><span class="k">Style bits</span><span>font {bit_names(gsf_bits[0])} → saved
        <span class="bad">{bit_names(before["regular_bits"][0])}</span>, now
        <span class="good">{bit_names(after["regular_bits"][0])}</span></span></div>
    </div>
  </div>
  <div class="col">
    <p class="quote">Font Book, on the user’s condensed Regular: <b>’name’ table
      structure</b> — every other weight passed.</p>
    <p><span class="k">Cause.</span> To keep links short the page records only what you
      changed, and since 24 August it gave the slice job only that too. The job writes
      names 1–6 from what it gets and deletes the rest. The CLI and the corpus fill the
      job from the font, so neither saw it.</p>
    <p><span class="k">Fix.</span> The job gets the whole editor; the browser test reads
      every name and bit back from the saved file. Reopening the app’s own link, a
      second path to the blank, is fixed too.</p>
    <div class="stat"><span class="big">{lost}<span class="to">→</span>{lost_now}</span>
      <span>names lost from an untouched slice</span></div>
  </div>
</section>

<section class="s2">
  <h2><span class="no">5</span>CI never finished: every run “cancelled” at six hours</h2>
  <div class="col">
    {ci_chart(runs)}
    {loop_diagram()}
  </div>
  <div class="col stats">
    <div class="stat"><span class="big">{len(hung)}<span class="to">×</span>6 h</span>
      <span>every CI run from 24 August to 6 October, killed in the conformance job</span></div>
    <div class="stat"><span class="big">{clock(duration(passed[0])) if passed else "—"}</span>
      <span>the first run after the fix, all four jobs green</span></div>
    <p class="cap">“Cancelled” read like someone pressing a button. It was a loop: the
      fixture step makes the test venv with fontTools only, and the runner, missing PyQt5,
      re-executed into it forever. On a workstation the venv already had PyQt5.</p>
  </div>
</section>

<section class="s3">
  <h2><span class="no">6</span>Moved to googlefonts</h2>
  <p><b>github.com/googlefonts/slice-web</b> and <b>googlefonts.github.io/slice-web</b>.
    GitHub redirects the repository but not Pages, so every
    <code>felipesanches.github.io/slice-web</code> link now answers 404. Every link in the
    app, the docs and the manual points at the new home.</p>
  <p><b>New checks behind these:</b> <code>tools/live-check.py</code> slices on the
    deployed site through a user’s own link; <code>tools/ci-replay.sh</code> runs the
    conformance job from CI’s starting point — 141 restarts in 10 s before, one after.</p>
</section>

<footer>
  <div><b>Measured, not drawn.</b> Names and bits are read from fonts the two builds
    saved in headless Chromium (Recursive, and Google Sans Flex 4.005 for the user’s
    case); the CI bars come from GitHub’s run history (<code>ci-runs.json</code>).</div>
  <div><b>Caught by tests now.</b> The browser test reads every name and bit back out of
    the saved font, and reopens the page’s own link; both checks fail on the old code.
    CI checks out the original Slice, so the corpus compares against the real program.</div>
  <div><b>Reproduce.</b> <code>docs/reports/2026-10-names-and-ci/</code>; fixes
    <code>7fe0c72</code>, <code>d48bba1</code>, <code>4e2c99c</code>, <code>4445257</code>,
    <code>a0f1b31</code>. Set in Google Sans Flex, sliced by the fixed Slice.</div>
</footer>
</body></html>"""


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gsf", type=Path, required=True)
    parser.add_argument("--before-dist", type=Path, required=True)
    parser.add_argument("--after-dist", type=Path, default=REPO / "dist")
    parser.add_argument("--refresh-ci", action="store_true",
                        help="refetch ci-runs.json from the GitHub API first")
    parser.add_argument("--png", type=Path, help="also write a PNG preview here")
    args = parser.parse_args()

    chromium = shutil.which("chromium") or shutil.which("google-chrome")
    if not chromium:
        sys.exit("needs chromium")

    with tempfile.TemporaryDirectory(prefix="slice-report2-", ignore_cleanup_errors=True) as tmp:
        work = Path(tmp)
        location = ["--axis=opsz=18", "--axis=GRAD=0", "--axis=ROND=0", "--axis=slnt=0"]
        for name, extra in [("text-400.ttf", ["--axis=wdth=100", "--axis=wght=400"]),
                            ("text-700.ttf", ["--axis=wdth=100", "--axis=wght=700"]),
                            ("black.ttf", ["--axis=wdth=80", "--axis=wght=900"])]:
            subprocess.run([str(SLICE), "cut", str(args.gsf), str(work / name), *location,
                            *extra, "--remove-overlaps"], check=True, stdout=subprocess.DEVNULL)

        before = measure(args.before_dist.resolve(), args.gsf, work / "before")
        after = measure(args.after_dist.resolve(), args.gsf, work / "after")
        source_bits = sample_bits(args.after_dist.resolve())
        gsf_bits = style_bits(args.gsf.read_bytes())
        for label, m in (("before", before), ("after", after)):
            print(f"{label}: {m['stamp']}\n  Regular typed once: name ID 2 {m['regular_id2']!r}, "
                  f"fsSelection {m['regular_bits'][0]} (font {gsf_bits[0]})\n"
                  f"  Regular's link reopened: Subfamily {m['reopened_subfamily']!r}\n"
                  f"  untouched sample saved: {m['sample_names']}, bits {m['sample_bits']} "
                  f"(font {source_bits})")
        runs = ci_history(args.refresh_ci)

        html_path = work / "one-pager.html"
        html_path.write_text(page(work, before, after, runs, source_bits, gsf_bits))
        pdf = HERE / "one-pager.pdf"
        common = [chromium, "--headless", "--disable-gpu", "--no-pdf-header-footer",
                  "--virtual-time-budget=5000"]
        subprocess.run([*common, f"--print-to-pdf={pdf}", html_path.as_uri()], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        info = subprocess.run(["pdfinfo", str(pdf)], capture_output=True, text=True).stdout
        pages = int(next(l.split()[-1] for l in info.splitlines() if l.startswith("Pages:")))
        print(f"wrote {pdf} ({pages} page{'s' if pages != 1 else ''})")
        if args.png:
            subprocess.run([*common, "--force-device-scale-factor=2", "--window-size=794,1123",
                            f"--screenshot={args.png}", html_path.as_uri()], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if pages != 1:
            print("the page overflowed; tighten the layout", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
