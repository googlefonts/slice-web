#!/usr/bin/env python3
"""Build the one-page summary of the Google Sans Flex fixes (October 2026).

Question this answers
---------------------
    What did one user's overlap report uncover, and what does each bug look like?

Everything drawn is real: the outlines come out of the two builds being compared, the
lines of text are shaped by HarfBuzz from the fonts those builds produce, and the
variable font is shaped at the same location as the reference. The numbers quoted on the
page come from the tools named in its footer, whose results are in tools/README.md.

Usage
-----
    docs/reports/2026-10-google-sans-flex/build.py \\
        --gsf 'GoogleSansFlex[GRAD,ROND,opsz,slnt,wdth,wght].ttf' \\
        --before /path/to/73f9f96/target/release/slice

`--before` is a `slice` binary built from 73f9f96, the commit the report was made
against (a `git worktree` and `cargo build --release -p slice-cli` give one). `--after`
defaults to this checkout's target/release/slice. Writes one-pager.pdf beside this
script, and a PNG preview wherever --png says. Needs chromium, and network access the first time
to set up `.report-venv/` at the repository root.
"""

from __future__ import annotations

import argparse
import base64
import html
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
VENV = REPO / ".report-venv"
PACKAGES = ["fonttools==4.62.1", "uharfbuzz==0.56.3", "skia-pathops==0.9.2"]

# The reporter's instance: Google Sans Flex Condensed Black.
LOCATION = dict(opsz=18, wdth=80, wght=900, GRAD=0, ROND=0, slnt=0)

INK, SOFT, FAINT, LINE = "#18181b", "#52525b", "#8a8a94", "#d8d8de"
ACCENT, ACCENT_SOFT, BAD, GOOD = "#c2410c", "#fff1e9", "#dc2626", "#15803d"


def reexec_in_venv() -> None:
    try:
        import pathops, uharfbuzz, fontTools  # noqa: F401,E401
        return
    except ImportError as missing:
        # Once is enough: a venv left half-installed would otherwise re-execute forever.
        if os.environ.get("SLICE_TOOL_REEXECUTED"):
            sys.exit(f"{VENV} cannot import {missing.name}; delete it and run this again")
    python = VENV / "bin" / "python"
    if not python.exists():
        subprocess.run([sys.executable, "-m", "venv", str(VENV)], check=True)
        subprocess.run([str(python), "-m", "pip", "install", "-q", *PACKAGES], check=True)
    os.environ["SLICE_TOOL_REEXECUTED"] = "1"
    os.execv(str(python), [str(python), str(Path(__file__).resolve()), *sys.argv[1:]])


def axes(location: dict, **override) -> list[str]:
    merged = {**location, **override}
    return [f"--axis={tag}={value:g}" for tag, value in merged.items()]


# --------------------------------------------------------------------------- drawing

def outline_d(glyphset, name: str) -> str:
    from fontTools.pens.recordingPen import DecomposingRecordingPen
    from fontTools.pens.svgPathPen import SVGPathPen

    recording = DecomposingRecordingPen(glyphset)
    glyphset[name].draw(recording)
    pen = SVGPathPen(None)
    recording.replay(pen)
    return pen.getCommands()


def doubled_region_d(glyphset, name: str) -> str:
    """The area a glyph fills twice: its non-zero fill minus its even-odd fill."""
    import pathops
    from fontTools.pens.recordingPen import DecomposingRecordingPen
    from fontTools.pens.svgPathPen import SVGPathPen

    def fill(rule):
        recording = DecomposingRecordingPen(glyphset)
        glyphset[name].draw(recording)
        path = pathops.Path()
        recording.replay(path.getPen())
        path.fillType = rule
        return pathops.simplify(path)

    doubled = pathops.op(fill(pathops.FillType.WINDING), fill(pathops.FillType.EVEN_ODD),
                         pathops.PathOp.DIFFERENCE)
    pen = SVGPathPen(None)
    doubled.draw(pen)
    return pen.getCommands()


def glyph_panel(font, name: str, box, px_width: float, doubled: bool,
                circle=None) -> str:
    """A glyph in 'outline view': every contour drawn, so a path running through the
    inside of the shape -- an overlap -- shows as a line where none should be."""
    gs = font.getGlyphSet()
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    px_height = px_width * h / w
    parts = [f'<svg viewBox="{x0} {-y1} {w} {h}" width="{px_width:.1f}" '
             f'height="{px_height:.1f}" xmlns="http://www.w3.org/2000/svg">',
             '<g transform="scale(1,-1)">']
    parts.append(f'<path d="{outline_d(gs, name)}" fill="#ececef" stroke="{INK}" '
                 f'stroke-width="1.3" vector-effect="non-scaling-stroke" '
                 f'stroke-linejoin="round"/>')
    if doubled:
        parts.append(f'<path d="{doubled_region_d(gs, name)}" fill="{BAD}" '
                     f'fill-opacity="0.55" stroke="{BAD}" stroke-width="1.3" '
                     f'vector-effect="non-scaling-stroke"/>')
    if circle:
        cx, cy, r = circle
        parts.append(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{ACCENT}" '
                     f'stroke-width="1.6" vector-effect="non-scaling-stroke" '
                     f'stroke-dasharray="4 3"/>')
    parts.append("</g></svg>")
    return "".join(parts)


def shape(font_path: Path, text: str, location: dict | None = None):
    import uharfbuzz as hb

    font = hb.Font(hb.Face(font_path.read_bytes()))
    if location:
        font.set_variations(location)
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    hb.shape(font, buf)
    return [(info.codepoint, pos.x_advance, pos.x_offset, pos.y_offset)
            for info, pos in zip(buf.glyph_infos, buf.glyph_positions)]


def kerning_rows(draw_font, text: str, before, after, px_width: float) -> tuple[str, int]:
    """Two settings of one line, with a tick joining each glyph to itself in the other.

    `draw_font` supplies the outlines -- they are identical in both builds -- and `before`
    and `after` the positions. Ticks that lean are glyphs that moved."""
    gs = draw_font.getGlyphSet()
    order = draw_font.getGlyphOrder()
    upm = draw_font["head"].unitsPerEm
    cap = draw_font["OS/2"].sCapHeight
    gap = int(upm * 0.95)
    total = max(sum(g[1] for g in before), sum(g[1] for g in after))
    margin = int(upm * 0.05)
    width = total + 2 * margin
    height = 2 * cap + gap + int(upm * 0.35)
    base_after = cap + int(upm * 0.1)
    base_before = base_after + gap + cap

    def placed(run, baseline):
        out, xs, x = [], [], margin
        for gid, advance, dx, dy in run:
            name = order[gid]
            out.append(f'<path transform="translate({x + dx},{baseline - dy}) scale(1,-1)" '
                       f'd="{outline_d(gs, name)}"/>')
            xs.append(x + dx)
            x += advance
        return out, xs, x

    a_paths, a_xs, a_end = placed(after, base_after)
    b_paths, b_xs, b_end = placed(before, base_before)
    ticks = []
    for xa, xb in zip(a_xs, b_xs):
        colour = INK if xa == xb else BAD
        ticks.append(f'<line x1="{xa}" y1="{base_after + upm * 0.06}" x2="{xb}" '
                     f'y2="{base_before - cap - upm * 0.06}" stroke="{colour}" '
                     f'stroke-width="{1.0 if xa == xb else 1.4}" '
                     f'vector-effect="non-scaling-stroke"/>')
    drift = b_end - a_end
    svg = (f'<svg viewBox="0 0 {width} {height}" width="{px_width:.1f}" '
           f'height="{px_width * height / width:.1f}" xmlns="http://www.w3.org/2000/svg">'
           f'<g fill="{INK}">{"".join(a_paths)}</g>'
           f'<g fill="{INK}">{"".join(b_paths)}</g>'
           f'<g>{"".join(ticks)}</g>'
           f'<line x1="{a_end}" y1="{base_after - cap * 1.05}" x2="{a_end}" '
           f'y2="{base_before + upm * 0.05}" stroke="{ACCENT}" stroke-width="1.2" '
           f'stroke-dasharray="4 3" vector-effect="non-scaling-stroke"/>'
           f'</svg>')
    return svg, drift


def bbox_cause_diagram() -> str:
    """Why the screen missed it: a vertical and a horizontal segment cross, and both of
    their bounding boxes are lines, whose overlap has no area."""
    return f'''<svg viewBox="0 0 400 84" width="100%" xmlns="http://www.w3.org/2000/svg"
      font-family="Body, sans-serif" font-size="10" fill="{SOFT}">
      <rect x="117" y="4" width="6" height="76" fill="{ACCENT_SOFT}" stroke="{ACCENT}"
        stroke-dasharray="3 2"/>
      <rect x="70" y="39" width="104" height="6" fill="{ACCENT_SOFT}" stroke="{ACCENT}"
        stroke-dasharray="3 2"/>
      <line x1="120" y1="4" x2="120" y2="80" stroke="{INK}" stroke-width="2.4"/>
      <line x1="70" y1="42" x2="174" y2="42" stroke="{INK}" stroke-width="2.4"/>
      <circle cx="120" cy="42" r="6.5" fill="none" stroke="{BAD}" stroke-width="2"/>
      <text x="112" y="14" text-anchor="end" fill="{INK}">stem edge</text>
      <text x="112" y="26" text-anchor="end">box 0 wide</text>
      <text x="64" y="40" text-anchor="end" fill="{INK}">bowl edge</text>
      <text x="64" y="52" text-anchor="end">box 0 tall</text>
      <text x="190" y="30" font-family="BodyBold, sans-serif" fill="{BAD}">the boxes overlap by 0 area</text>
      <text x="190" y="44" fill="{BAD}">⇒ pre-check: “these can’t cross”</text>
      <text x="190" y="58" fill="{BAD}">⇒ the glyph is never merged</text>
    </svg>'''


def store_cause_diagram(kern_before: int, kern_after: int, pair: str) -> str:
    """GPOS holds the default master's value and a pointer into GDEF's store."""
    delta = kern_after - kern_before

    def num(value: int, sign: bool = False) -> str:
        return (f"{value:+d}" if sign else f"{value:d}").replace("-", "\u2212")

    kern_before, kern_after, delta = num(kern_before), num(kern_after), num(delta, True)
    return f'''<svg viewBox="0 0 640 46" width="100%" xmlns="http://www.w3.org/2000/svg"
      font-family="Body, sans-serif" font-size="10.4" fill="{SOFT}">
      <defs><marker id="arr" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6"
        markerHeight="6" orient="auto"><path d="M0,0 L8,4 L0,8 z" fill="{SOFT}"/></marker></defs>
      <rect x="1" y="2" width="150" height="40" rx="5" fill="#fff" stroke="{LINE}"/>
      <text x="10" y="18" font-family="BodyBold, sans-serif" fill="{INK}">GPOS: kern {pair}</text>
      <text x="10" y="33">{kern_before} (Regular’s) + pointer</text>
      <line x1="151" y1="22" x2="178" y2="22" stroke="{SOFT}" marker-end="url(#arr)"/>
      <rect x="180" y="2" width="140" height="40" rx="5" fill="#fff" stroke="{LINE}"/>
      <text x="189" y="18" font-family="BodyBold, sans-serif" fill="{INK}">GDEF variation store</text>
      <text x="189" y="33">Δ at this location: {delta}</text>
      <text x="334" y="18" fill="{BAD}"><tspan font-family="BodyBold, sans-serif">Before:</tspan> copied
        as-is; with no axes left, nothing</text>
      <text x="334" y="33" fill="{BAD}">computes Δ, so {pair} kerns {kern_before} at every
        weight.</text>
      <text x="334" y="46" fill="{GOOD}"><tspan font-family="BodyBold, sans-serif">Now:</tspan>
        {kern_before} {delta} = {kern_after} written in; store removed.</text>
    </svg>'''


def extension_diagram() -> str:
    """The residual walk visited every lookup but stopped at the extension wrapper."""
    boxes = [("MarkToBase", False)] * 4 + [("MarkToMark", False)] * 2 + [("Pair", False),
                                                                          ("Extension", True)]
    cells = []
    for i, (label, is_ext) in enumerate(boxes):
        x = 4 + i * 60
        stroke = ACCENT if is_ext else LINE
        cells.append(f'<rect x="{x}" y="10" width="56" height="26" rx="4" fill="#fff" '
                     f'stroke="{stroke}" stroke-width="{1.6 if is_ext else 1}"/>'
                     f'<text x="{x + 28}" y="27" text-anchor="middle" font-size="8.6" '
                     f'fill="{INK}">{label}</text>')
        if not is_ext:
            cells.append(f'<text x="{x + 28}" y="50" text-anchor="middle" fill="{GOOD}" '
                         f'font-size="12">✓</text>')
    ex = 4 + 7 * 60
    cells.append(f'<rect x="{ex + 4}" y="58" width="80" height="26" rx="4" '
                 f'fill="{ACCENT_SOFT}" stroke="{ACCENT}"/>'
                 f'<text x="{ex + 44}" y="70" text-anchor="middle" font-size="8.6" '
                 f'fill="{INK}" font-family="BodyBold, sans-serif">Pair</text>'
                 f'<text x="{ex + 44}" y="80" text-anchor="middle" font-size="7.6" '
                 f'fill="{SOFT}">all the kerning</text>'
                 f'<line x1="{ex + 28}" y1="36" x2="{ex + 28}" y2="58" stroke="{ACCENT}" '
                 f'stroke-dasharray="3 2"/>'
                 f'<text x="{ex - 4}" y="78" text-anchor="end" fill="{BAD}" font-size="10" '
                 f'font-family="BodyBold, sans-serif">before: never entered ✗</text>'
                 f'<text x="{ex - 4}" y="91" text-anchor="end" fill="{GOOD}" font-size="10" '
                 f'font-family="BodyBold, sans-serif">now: walked like the rest ✓</text>')
    return (f'<svg viewBox="0 0 {4 + 8 * 60 + 30} 96" width="100%" '
            f'xmlns="http://www.w3.org/2000/svg" font-family="Body, sans-serif" fill="{SOFT}">'
            f'<text x="4" y="6" font-size="8.6" fill="{FAINT}">GPOS lookups of Google Sans '
            f'Flex, in order — the walk adds each value’s Δ</text>'
            + "".join(cells) + "</svg>")


# --------------------------------------------------------------------------- page

def font_face(name: str, path: Path) -> str:
    data = base64.b64encode(path.read_bytes()).decode()
    return (f"@font-face {{ font-family: '{name}'; "
            f"src: url(data:font/ttf;base64,{data}) format('truetype'); }}")


def page(work: Path, gsf: Path) -> str:
    from fontTools.ttLib import TTFont

    before_merged = TTFont(work / "before-merged.ttf")
    after_merged = TTFont(work / "after-merged.ttf")
    plain = TTFont(work / "plain.ttf")

    # One scale and one vertical range for every glyph, so they share a baseline.
    scale = 0.0815
    e_box = (0, -60, 1130, 1470)
    p_box = (90, -60, 1290, 1470)
    glyphs = {
        "e_before": glyph_panel(before_merged, "e", e_box, 1130 * scale, doubled=True),
        "p_before": glyph_panel(before_merged, "P", p_box, 1200 * scale, doubled=True),
        "e_after": glyph_panel(after_merged, "e", e_box, 1130 * scale, doubled=False),
        "p_after": glyph_panel(after_merged, "P", p_box, 1200 * scale, doubled=False),
    }

    text = "AVATAR Typeface"
    before_run = shape(work / "before-static.ttf", text)
    after_run = shape(gsf, text, LOCATION)
    kern_svg, drift = kerning_rows(plain, text, before_run, after_run, 420)

    order = plain.getGlyphOrder()
    hmtx = plain["hmtx"]

    def pair_kern(run):
        gid, advance = run[0][0], run[0][1]
        return advance - hmtx[order[gid]][0]

    k_before = pair_kern(shape(work / "before-static.ttf", "Ty"))
    k_after = pair_kern(shape(gsf, "Ty", LOCATION))

    icon = (REPO / "docs" / "assets" / "slice-icon.svg").read_text()
    icon = icon[icon.index("<svg"):].replace('width="75px" height="60px"',
                                             'width="44" height="35"')

    faces = "\n".join([font_face("Body", work / "text-400.ttf"),
                       font_face("BodyBold", work / "text-700.ttf"),
                       font_face("Black", work / "after-merged.ttf")])

    return f"""<!doctype html><html><head><meta charset="utf-8">
<title>Slice — one report, three fixes</title>
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
h1 {{ font-family: Black, sans-serif; font-weight: normal; font-size: 24pt; line-height: 1; margin: 0; }}
.sub {{ color: {SOFT}; margin-top: 1.2mm; }}
.meta {{ margin-left: auto; text-align: right; color: {FAINT}; font-size: 7.4pt;
  white-space: nowrap; }}
section {{ border: 1px solid {LINE}; border-radius: 3mm; padding: 2.8mm 4mm 3mm;
  display: grid; gap: 2mm 6mm; align-items: start; }}
h2 {{ font-family: Black, sans-serif; font-weight: normal; font-size: 13pt; line-height: 1.05;
  margin: 0 0 0.6mm; grid-column: 1 / -1; }}
h2 .no {{ color: {ACCENT}; margin-right: 1.6mm; }}
.cap {{ color: {SOFT}; font-size: 7.4pt; }}
.label {{ font-family: BodyBold, sans-serif; font-size: 7pt; letter-spacing: 0.05em;
  text-transform: uppercase; color: {FAINT}; }}
.bad {{ color: {BAD}; }} .good {{ color: {GOOD}; }}
.glyphs {{ display: flex; gap: 2.4mm; align-items: center; }}
.glyphs figure {{ margin: 0; text-align: center; }}
.glyphs .tag {{ font-family: BodyBold, sans-serif; font-size: 7pt; letter-spacing: 0.05em;
  text-transform: uppercase; }}
.arrow {{ font-size: 16pt; color: {FAINT}; }}
.stats {{ display: flex; flex-direction: column; gap: 1.6mm; }}
.stat {{ display: grid; grid-template-columns: auto 1fr; align-items: center; gap: 2.4mm; }}
.stats .stat {{ grid-template-columns: 1fr; gap: 0.6mm; }}
.big {{ font-family: Black, sans-serif; font-size: 15pt; line-height: 1; white-space: nowrap; }}
.big .to {{ color: {FAINT}; font-family: Body, sans-serif; font-size: 11pt; padding: 0 0.6mm; }}
.k {{ font-family: BodyBold, sans-serif; }}
.s1 {{ grid-template-columns: 112mm 1fr; }}
.s2 {{ grid-template-columns: auto 1fr; }}
.s2 .stats {{ padding-top: 4mm; }}
.s3 {{ grid-template-columns: 1fr 62mm; }}
.col {{ display: flex; flex-direction: column; gap: 1.8mm; }}
footer {{ margin-top: auto; color: {SOFT}; font-size: 7.2pt; border-top: 1px solid {LINE};
  padding-top: 2mm; display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 5mm; }}
footer b {{ color: {INK}; }}
code {{ font-family: BodyBold, sans-serif; font-size: 0.95em; color: {INK}; }}
</style></head><body>

<header>
  {icon}
  <div>
    <h1>One report, three fixes</h1>
    <div class="sub">A user outlined Google Sans Flex 4.005 Condensed Black (opsz 18, wdth 80,
      wght 900) in Illustrator and still found overlaps. Following that up turned up two
      more bugs.</div>
  </div>
  <div class="meta">slice-web<br><b>73f9f96 → 26fd388</b><br>6 October 2026</div>
</header>

<section class="s1">
  <h2><span class="no">1</span>“Remove overlapping contours” left overlaps behind</h2>
  <div class="col">
    <div class="glyphs">
      <figure>{glyphs["e_before"]}<div class="tag bad">before</div></figure>
      <figure>{glyphs["p_before"]}<div class="tag bad">before</div></figure>
      <div class="arrow">→</div>
      <figure>{glyphs["e_after"]}<div class="tag good">now</div></figure>
      <figure>{glyphs["p_after"]}<div class="tag good">now</div></figure>
    </div>
    {bbox_cause_diagram()}
  </div>
  <div class="col">
    <p class="cap">Outline view, as Illustrator shows it after “Create Outlines”.
      <span class="bad">Red</span>: area filled twice, fenced by a path running through
      the inside of the letter.</p>
    <p><span class="k">Cause.</span> A pre-check skipped any glyph whose segments’
      bounding boxes did not overlap <i>by area</i>, and a straight stem’s box has none
      (left). The merge also split hairline spikes off as stray zero-area paths.</p>
    <p><span class="k">Fix.</span> No pre-check: every glyph is merged (0.2 s for all
      682). Contours that enclose nothing are dropped.</p>
    <div class="stat"><span class="big">45<span class="to">→</span>0</span>
      <span>glyphs still overlapping (37) or carrying stray paths (8)</span></div>
  </div>
</section>

<section class="s2">
  <h2><span class="no">2</span>Static instances kerned like the Regular master</h2>
  <div class="col">
    <div class="label">Variable font at this location — and now</div>
    {kern_svg}
    <div class="label bad">Before: same glyphs, Regular’s kerning — the line ends
      {abs(drift)} units early</div>
  </div>
  <div class="col stats">
    <div class="stat"><span class="big">20,251<span class="to">→</span>0</span>
      <span>of 110,224 character pairs placed differently from fontTools</span></div>
    <div class="stat"><span class="big">394</span>
      <span>units: worst accent misplacement, where no precomposed letter exists</span></div>
    <div class="stat"><span class="big">758</span>
      <span>of 783 Google Fonts variable fonts carry variable kerning or anchors</span></div>
  </div>
  <div style="grid-column: 1 / -1">{store_cause_diagram(k_before, k_after, "T·y")}</div>
</section>

<section class="s3">
  <h2><span class="no">3</span>Kerning inside “extension” lookups was never adjusted</h2>
  <div style="grid-column: 1 / -1; width: 150mm">{extension_diagram()}</div>
  <p><span class="k">Cause.</span> Compilers often wrap the biggest lookup — usually the
    kerning — in an extension. The walk that adds each value’s Δ treated the wrapper as
    empty, so partial instances that pin an axis off its default kept wrong kerning.
    write-fonts’ <code>RemapVarStore</code> had the same blind spot until 0.54.0.
    <span class="k">Fix.</span> The walk unwraps extensions.</p>
  <div class="col stats">
    <div class="stat"><span class="big">14,298<span class="to">→</span>0</span>
      <span>pairs wrong at every weight sampled (wdth 80, wght 400:900)</span></div>
    <div class="stat"><span class="big">250</span>
      <span>of 783 Google Fonts variable fonts keep their kerning in an extension</span></div>
  </div>
</section>

<footer>
  <div><b>Checked by engines that are not ours.</b> Overlaps with skia-pathops (what
    fontTools uses). Positioning by shaping every character pair with HarfBuzz, against
    fontTools’ instance and the variable font itself.</div>
  <div><b>Beyond this font.</b> Ten more Google Fonts, sampled by survey: 8 had static
    kerning wrong by 35,173–157,980 pairs; none now. Conformance corpus 302/302, and the
    original Slice passes the new case too.</div>
  <div><b>Reproduce.</b> <code>tools/overlap-check.py</code>,
    <code>kerning-compare.py</code>, <code>kerning-sample.sh</code>,
    <code>gdef-store-survey.py</code>; results in <code>tools/README.md</code>. Set in
    Google Sans Flex, sliced by the fixed Slice.</div>
</footer>
</body></html>"""


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gsf", type=Path, required=True)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, default=REPO / "target" / "release" / "slice")
    parser.add_argument("--png", type=Path, help="also write a PNG preview here")
    parser.add_argument("--keep", type=Path, help="keep the work directory here")
    args = parser.parse_args()
    reexec_in_venv()

    chromium = shutil.which("chromium") or shutil.which("google-chrome")
    if not chromium:
        sys.exit("needs chromium to print the page")

    with tempfile.TemporaryDirectory(prefix="slice-report-") as scratch:
        work = args.keep or Path(scratch)
        work.mkdir(parents=True, exist_ok=True)
        jobs = [
            (args.after, "plain.ttf", axes(LOCATION), []),
            (args.before, "before-static.ttf", axes(LOCATION), []),
            (args.before, "before-merged.ttf", axes(LOCATION), ["--remove-overlaps"]),
            (args.after, "after-merged.ttf", axes(LOCATION), ["--remove-overlaps"]),
            (args.after, "text-400.ttf", axes(LOCATION, opsz=12, wdth=100, wght=400),
             ["--remove-overlaps"]),
            (args.after, "text-700.ttf", axes(LOCATION, opsz=12, wdth=100, wght=700),
             ["--remove-overlaps"]),
        ]
        for binary, name, axis_args, extra in jobs:
            subprocess.run([str(binary), "cut", str(args.gsf), str(work / name),
                            *axis_args, *extra], check=True, stdout=subprocess.DEVNULL)

        page_path = work / "one-pager.html"
        page_path.write_text(page(work, args.gsf))
        pdf = HERE / "one-pager.pdf"
        common = [chromium, "--headless", "--disable-gpu", "--no-pdf-header-footer",
                  "--virtual-time-budget=5000"]
        subprocess.run([*common, f"--print-to-pdf={pdf}", page_path.as_uri()], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        pages = pdf.read_bytes().count(b"/Type /Page\n") + pdf.read_bytes().count(b"/Type /Page>")
        if shutil.which("pdfinfo"):
            info = subprocess.run(["pdfinfo", str(pdf)], capture_output=True, text=True).stdout
            pages = int(next(l.split()[-1] for l in info.splitlines() if l.startswith("Pages:")))
        print(f"wrote {pdf} ({pages} page{'s' if pages != 1 else ''})")
        if pages != 1:
            print("the page overflowed; tighten the layout", file=sys.stderr)
            return 1
        if args.png:
            subprocess.run([*common, "--force-device-scale-factor=2",
                            "--window-size=794,1123", f"--screenshot={args.png}",
                            page_path.as_uri()], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            print(f"wrote {args.png}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
