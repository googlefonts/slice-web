#!/usr/bin/env python3
"""Build the conformance-corpus font fixtures into ``tests/suite/fixtures/out/``.

Every font in the roster in ``tests/suite/README.md`` is produced here, from
source, with fontTools.  Nothing is collected from the wild except
``recursive-vf``, which is copied verbatim from ``testdata/fonts/``.

The fonts are deliberately tiny.  Each one isolates a single property that a
slicing tool has to get right; none of them is trying to be a typeface.  See
``README.md`` next to this file for what each fixture is for.

Determinism
-----------
The output is byte-for-byte reproducible.  ``head.created`` and
``head.modified`` are pinned to :data:`FIXED_TIMESTAMP` and every ``TTFont`` is
opened or created with ``recalcTimestamp=False``, because fontTools otherwise
stamps ``head.modified`` with the wall clock on save.  The script builds
everything twice in one process and refuses to write anything if the two runs
disagree.

Verification
------------
Nothing is written until it has passed :func:`verify_font`:

* it loads in fontTools and survives a ``TTFont(...).save()`` round trip;
* the tables a font needs in order to be openable are all present;
* every glyph has a positive advance width;
* if it has ``fvar``, ``varLib.instancer.instantiateVariableFont`` can pin it
  at the default, at every axis minimum and at every axis maximum, and each
  resulting static font saves.

The ``overlapping`` fixture gets a further check: the winding number of every
region of every glyph is computed from the compiled outlines and compared with
what the glyph is supposed to look like, at the default *and* at the extreme of
its axis.  See :func:`check_overlapping_windings`.

Usage
-----
    python3 tests/suite/fixtures/build.py            # build and verify
    python3 tests/suite/fixtures/build.py --check    # verify only, write nothing
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import io
import sys
from array import array
from pathlib import Path

from fontTools.designspaceLib import (
    AxisDescriptor,
    DesignSpaceDocument,
    InstanceDescriptor,
    SourceDescriptor,
)
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.otlLib.builder import buildStatTable
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables import otTables
from fontTools.ttLib.tables.ttProgram import Program
from fontTools.varLib import build as varlib_build
from fontTools.varLib.instancer import instantiateVariableFont

HERE = Path(__file__).resolve().parent
OUT = HERE / "out"
REPO = HERE.parents[2]
RECURSIVE_TTF = REPO / "testdata" / "fonts" / "Recursive-VF.subset.ttf"
#: A Recursive instance that was already sliced: the same design, no ``fvar``.
RECURSIVE_STATIC = REPO / "testdata" / "fonts" / "Recursive-Sliced.subset.ttf"

#: 2000-01-01T00:00:00Z expressed in the ``head`` epoch (seconds since
#: 1904-01-01).  96 years * 365 days + 24 leap days = 35064 days.
FIXED_TIMESTAMP = 35064 * 86400  # 3029529600

UPEM = 1000
ASCENT = 800
DESCENT = -200

#: Tables without which a font is not openable / not meaningfully a font.
REQUIRED_TABLES = {"head", "hhea", "hmtx", "maxp", "cmap", "name", "OS/2", "post"}


# --------------------------------------------------------------------------
# small geometry helpers
# --------------------------------------------------------------------------

def ccw_rect(x0, y0, x1, y1):
    """A rectangle wound counter-clockwise in a y-up frame: positive area.

    Bottom edge left to right, up the right side, back along the top.  This is
    the PostScript/CFF convention for an *outer* contour, and the TrueType
    convention for a *counter*.
    """
    return [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]


def cw_rect(x0, y0, x1, y1):
    """A rectangle wound clockwise: negative signed area.

    The TrueType convention for an *outer* contour, and the PostScript/CFF
    convention for a *counter*.
    """
    return list(reversed(ccw_rect(x0, y0, x1, y1)))


def draw_contours(pen, contours):
    for contour in contours:
        pen.moveTo(contour[0])
        for point in contour[1:]:
            pen.lineTo(point)
        pen.closePath()


def simple_glyph(contours, glyph_set=None):
    pen = TTGlyphPen(glyph_set)
    draw_contours(pen, contours)
    return pen.glyph()


def signed_area(contour):
    """Shoelace.  Positive = counter-clockwise in a y-up frame."""
    total = 0.0
    n = len(contour)
    for i in range(n):
        x0, y0 = contour[i]
        x1, y1 = contour[(i + 1) % n]
        total += x0 * y1 - x1 * y0
    return total / 2.0


def winding_number(point, contours):
    """Non-zero winding number of ``point`` with respect to ``contours``.

    The usual signed crossing count: an upward edge passing to the left of the
    point contributes +1, a downward edge passing to the left contributes -1.
    Counter-clockwise contours therefore give positive numbers.  A point is
    inside under the non-zero rule -- the rule ``glyf`` outlines are filled
    with -- exactly when this is not zero.
    """
    px, py = point
    total = 0
    for contour in contours:
        n = len(contour)
        for i in range(n):
            x0, y0 = contour[i]
            x1, y1 = contour[(i + 1) % n]
            # (x0,y0)->(x1,y1) cross (x0,y0)->(px,py): >0 means point is left
            side = (x1 - x0) * (py - y0) - (px - x0) * (y1 - y0)
            if y0 <= py:
                if y1 > py and side > 0:
                    total += 1
            else:
                if y1 <= py and side < 0:
                    total -= 1
    return total


def glyph_contours(font, glyph_name):
    """Contours of a *line-only* glyph, read back out of the compiled ``glyf``."""
    glyf = font["glyf"]
    glyph = glyf[glyph_name]
    coords, end_points, flags = glyph.getCoordinates(glyf)
    coords = list(coords)
    if any(not (f & 0x01) for f in flags):
        raise AssertionError(f"{glyph_name}: expected only on-curve points")
    contours = []
    start = 0
    for end in end_points:
        contours.append([tuple(p) for p in coords[start : end + 1]])
        start = end + 1
    return contours


# --------------------------------------------------------------------------
# master construction
# --------------------------------------------------------------------------

def new_master(
    glyph_order,
    cmap,
    advances,
    family,
    style="Regular",
    is_ttf=True,
):
    fb = FontBuilder(UPEM, isTTF=is_ttf)
    fb.setupGlyphOrder(list(glyph_order))
    fb.setupCharacterMap(cmap)
    fb.setupHorizontalMetrics({name: advances[name] for name in glyph_order})
    fb.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT)
    ps_name = f"{family}-{style}".replace(" ", "")
    fb.setupNameTable(
        {
            "familyName": family,
            "styleName": style,
            "psName": ps_name,
            "version": "Version 1.000",
            "uniqueFontIdentifier": f"{family}:{style}:2000",
            "fullName": f"{family} {style}",
        }
    )
    fb.setupOS2(
        sTypoAscender=ASCENT,
        sTypoDescender=DESCENT,
        sTypoLineGap=0,
        usWinAscent=ASCENT,
        usWinDescent=-DESCENT,
        sxHeight=500,
        sCapHeight=700,
        achVendID="SLCE",
    )
    fb.setupPost(keepGlyphNames=True)
    fb.setupHead(
        unitsPerEm=UPEM,
        created=FIXED_TIMESTAMP,
        modified=FIXED_TIMESTAMP,
    )
    fb.font.recalcTimestamp = False
    return fb


def axis(tag, name, minimum, default, maximum, mapping=None):
    a = AxisDescriptor()
    a.tag = tag
    a.name = name
    a.minimum = minimum
    a.default = default
    a.maximum = maximum
    if mapping:
        a.map = list(mapping)
    return a


def source(font, location, name, is_default=False):
    s = SourceDescriptor()
    s.font = font
    s.location = dict(location)
    s.name = name
    s.familyName = "Fixture"
    s.styleName = name
    if is_default:
        s.copyInfo = True
    return s


def make_vf(axes, sources, instances=()):
    ds = DesignSpaceDocument()
    for a in axes:
        ds.addAxis(a)
    for s in sources:
        ds.addSource(s)
    for i in instances:
        ds.addInstance(i)
    vf, _, _ = varlib_build(ds, optimize=True)
    finalize(vf)
    return vf


def finalize(font):
    """Pin the timestamps.  Called on every font just before it is serialised."""
    font.recalcTimestamp = False
    head = font["head"]
    head.created = FIXED_TIMESTAMP
    head.modified = FIXED_TIMESTAMP
    return font


def to_bytes(font, flavor=None):
    font.flavor = flavor
    buf = io.BytesIO()
    font.save(buf)
    return buf.getvalue()


# --------------------------------------------------------------------------
# the fixtures
# --------------------------------------------------------------------------

def _normalise_head(data):
    """``head`` bytes with the two fields a container may legitimately change.

    ``checkSumAdjustment`` (offset 8) is a checksum over the whole file, and
    the WOFF2 writer sets ``flags`` bit 11 ("lossless") at offset 16.  Neither
    says anything about the font itself.
    """
    flags = int.from_bytes(data[16:18], "big") & ~0x0800
    return data[:8] + b"\0\0\0\0" + data[12:16] + flags.to_bytes(2, "big") + data[18:]


def build_recursive_family():
    """``recursive-vf`` and its two compressed containers.

    The real font, copied verbatim.  The WOFF and WOFF2 are re-containered from
    exactly those bytes so that all three are provably the same font.
    """
    raw = RECURSIVE_TTF.read_bytes()
    results = [("recursive-vf.ttf", raw)]
    reference = TTFont(io.BytesIO(raw), recalcTimestamp=False, lazy=True)
    for suffix, flavor in (("woff", "woff"), ("woff2", "woff2")):
        font = TTFont(
            io.BytesIO(raw), recalcTimestamp=False, recalcBBoxes=False, lazy=True
        )
        data = to_bytes(font, flavor)
        font.close()
        # the container must carry exactly the same font
        packed = TTFont(io.BytesIO(data), recalcTimestamp=False, lazy=True)
        if set(packed.keys()) != set(reference.keys()):
            raise AssertionError(
                f"recursive-vf-{suffix}: table set differs from the source ttf"
            )
        for tag in reference.keys():
            if tag == "GlyphOrder":
                continue
            a, b = packed.reader[tag], reference.reader[tag]
            if tag == "head":
                a, b = _normalise_head(a), _normalise_head(b)
            if a != b:
                raise AssertionError(
                    f"recursive-vf-{suffix}: table {tag!r} differs from the source ttf"
                )
        packed.close()
        results.append((f"recursive-vf-{suffix}.{suffix}", data))
    reference.close()
    return results


TWO_AXIS_GLYPHS = [".notdef", "H", "O", "period"]
TWO_AXIS_CMAP = {0x48: "H", 0x4F: "O", 0x2E: "period"}


def two_axis_master(stem, width_scale, family):
    """A stem-weight / width parametric master with simple contours only."""
    adv = {
        ".notdef": (600, 50),
        "H": (int(600 * width_scale), 60),
        "O": (int(620 * width_scale), 60),
        "period": (300, 90),
    }
    fb = new_master(TWO_AXIS_GLYPHS, TWO_AXIS_CMAP, adv, family)
    sx = width_scale

    def x(value):
        return int(round(value * sx))

    glyphs = {}
    glyphs[".notdef"] = simple_glyph(
        [cw_rect(50, 0, 550, 700), ccw_rect(120, 70, 480, 630)]
    )
    # H: two stems and a crossbar, three separate rectangles (overlap-free)
    glyphs["H"] = simple_glyph(
        [
            cw_rect(x(60), 0, x(60 + stem), 700),
            cw_rect(x(480 - stem), 0, x(480), 700),
            cw_rect(x(60 + stem), 300, x(480 - stem), 300 + stem // 2),
        ]
    )
    # O: a rectangular ring
    glyphs["O"] = simple_glyph(
        [
            cw_rect(x(60), 0, x(560), 700),
            ccw_rect(x(60 + stem), stem, x(560 - stem), 700 - stem),
        ]
    )
    glyphs["period"] = simple_glyph([cw_rect(x(90), 0, x(90 + stem), stem)])
    fb.setupGlyf(glyphs)
    return fb.font


def build_two_axis(with_avar, family=None):
    """wght x wdth, simple contours only.  ``with_avar`` toggles the axis maps.

    ``wdth`` spans the whole registered range, 50 to 200 with a default of 100.
    That is deliberate and load-bearing: ``OS/2.usWidthClass`` is derived from a
    pinned ``wdth`` through a nine-node piecewise-linear map over exactly that
    interval, and fontTools clamps a pin into the *axis* extent before consulting
    the map.  A narrower fixture would therefore answer a question nobody asked --
    a case pinning ``wdth=175`` on a 75/100/125 axis silently measures 125.
    """
    if family is None:
        family = "FixtureTwoAxis" if with_avar else "FixtureNoAvar"
    wght_map = [(100, 100), (400, 400), (700, 800), (900, 900)] if with_avar else None
    wdth_map = [(50, 50), (100, 100), (200, 200)] if with_avar else None
    axes = [
        axis("wght", "Weight", 100, 400, 900, wght_map),
        axis("wdth", "Width", 50, 100, 200, wdth_map),
    ]
    designs = [
        ({"Weight": 400, "Width": 100}, "default", 90, 1.00, True),
        ({"Weight": 100, "Width": 100}, "thin", 40, 1.00, False),
        ({"Weight": 900, "Width": 100}, "black", 200, 1.00, False),
        ({"Weight": 400, "Width": 50}, "narrow", 90, 0.62, False),
        ({"Weight": 400, "Width": 200}, "wide", 90, 1.60, False),
    ]
    sources = [
        source(two_axis_master(stem, scale, family), loc, name, is_default)
        for loc, name, stem, scale, is_default in designs
    ]
    vf = make_vf(axes, sources)
    if with_avar and "avar" not in vf:
        raise AssertionError("two-axis: expected an avar table")
    if not with_avar and "avar" in vf:
        raise AssertionError("no-avar: an avar table was produced anyway")
    extents = {a.axisTag: (a.minValue, a.defaultValue, a.maxValue) for a in vf["fvar"].axes}
    if extents.get("wdth") != (50.0, 100.0, 200.0):
        raise AssertionError(
            f"two-axis: wdth must span the registered 50/100/200, got {extents.get('wdth')}"
        )
    if extents.get("wght", (0, 0, 0))[0] > 400 or extents.get("wght", (0, 0, 0))[2] < 400:
        raise AssertionError("two-axis: wght must include 400")
    return vf


def build_centred_default():
    """One axis, 100 / 400 / 900: the default sits off-centre.

    The two halves of the axis normalize with different scale factors, which is
    what a renormalizing slicer has to get right.
    """
    axes = [axis("wght", "Weight", 100, 400, 900)]
    designs = [
        ({"Weight": 400}, "default", 90, True),
        ({"Weight": 100}, "thin", 30, False),
        ({"Weight": 900}, "black", 220, False),
    ]
    sources = [
        source(two_axis_master(stem, 1.0, "FixtureCentred"), loc, name, is_default)
        for loc, name, stem, is_default in designs
    ]
    return make_vf(axes, sources)


def build_single_axis_min_default():
    """Default == minimum, the shape Recursive's ``wght`` has (300/300/1000)."""
    axes = [axis("wght", "Weight", 300, 300, 1000)]
    designs = [
        ({"Weight": 300}, "light", 60, True),
        ({"Weight": 1000}, "black", 230, False),
    ]
    sources = [
        source(two_axis_master(stem, 1.0, "FixtureMinDefault"), loc, name, is_default)
        for loc, name, stem, is_default in designs
    ]
    return make_vf(axes, sources)


COMPOSITE_GLYPHS = [
    ".notdef", "square", "dot", "squaredot", "double", "comp.overlap", "comp.plain",
]
COMPOSITE_CMAP = {
    0x41: "square",
    0x42: "dot",
    0x43: "squaredot",
    0x44: "double",
    0x45: "comp.overlap",
    0x46: "comp.plain",
}

#: ``comp.overlap``'s second ``square`` sits here, far enough in to overlap the
#: first in both masters (``square`` is 400 units in the default and 470 in the
#: black) and far enough out that the pair still fits inside the 760-unit advance.
COMP_OVERLAP_OFFSET = (180, 160)
#: ``comp.plain``'s two ``dot`` components.  ``dot`` is 120 units square and does
#: not vary, so 0 and 300 leave a 180-unit gap at every location on the axis.
COMP_PLAIN_OFFSETS = (0, 300)


def composite_master(size, dx, dy):
    adv = {name: (760, 0) for name in COMPOSITE_GLYPHS}
    adv[".notdef"] = (600, 50)
    fb = new_master(COMPOSITE_GLYPHS, COMPOSITE_CMAP, adv, "FixtureComposites")
    glyphs = {}
    glyphs[".notdef"] = simple_glyph(
        [cw_rect(50, 0, 550, 700), ccw_rect(120, 70, 480, 630)]
    )
    glyphs["square"] = simple_glyph([cw_rect(0, 0, size, size)])
    glyphs["dot"] = simple_glyph([cw_rect(0, 0, 120, 120)])
    # depth 1: two simple components
    pen = TTGlyphPen(glyphs)
    pen.addComponent("square", (1, 0, 0, 1, 0, 0))
    pen.addComponent("dot", (1, 0, 0, 1, dx, 520 + dy))
    glyphs["squaredot"] = pen.glyph()
    # depth 2: a component that is itself a composite
    pen = TTGlyphPen(glyphs)
    pen.addComponent("squaredot", (1, 0, 0, 1, 0, 0))
    pen.addComponent("dot", (1, 0, 0, 1, 500 + dx, 0))
    glyphs["double"] = pen.glyph()
    # two components that overlap each other.  The merged boundary cannot be
    # written as component references, so removing the overlap has to decompose
    # the glyph first.  The offsets do not vary, so the overlap is the same at
    # every location on the axis.
    pen = TTGlyphPen(glyphs)
    pen.addComponent("square", (1, 0, 0, 1, 0, 0))
    pen.addComponent("square", (1, 0, 0, 1, *COMP_OVERLAP_OFFSET))
    glyphs["comp.overlap"] = pen.glyph()
    # two components that never touch: nothing to merge, so the glyph must come
    # out the far side of overlap removal still a composite.
    pen = TTGlyphPen(glyphs)
    for offset in COMP_PLAIN_OFFSETS:
        pen.addComponent("dot", (1, 0, 0, 1, offset, 0))
    glyphs["comp.plain"] = pen.glyph()
    fb.setupGlyf(glyphs)
    return fb.font


def _component_boxes(font, glyph_name, location=None):
    """Bounding box of each of a composite's components, in font units."""
    static = font
    if location is not None and "fvar" in font:
        static = instantiateVariableFont(copy.deepcopy(font), location, inplace=False)
    glyf = static["glyf"]
    boxes = []
    for component in glyf[glyph_name].components:
        base = glyf[component.glyphName]
        base.recalcBounds(glyf)
        dx, dy = component.getComponentInfo()[1][4:6]
        boxes.append((base.xMin + dx, base.yMin + dy, base.xMax + dx, base.yMax + dy))
    return boxes


def _components_overlap(font, glyph_name):
    """True when any two of a composite's components share area.

    Bounding boxes are the whole story here because every component in this fixture
    is a filled rectangle, so a box intersection *is* an area intersection.  Checked
    at the default and at the axis maximum, because a component offset that varies
    could otherwise separate a pair at one end of the axis and not the other.
    """
    results = set()
    for location in ({"wght": 400}, {"wght": 900}):
        boxes = _component_boxes(font, glyph_name, location)
        overlap = any(
            a[0] < b[2] and b[0] < a[2] and a[1] < b[3] and b[1] < a[3]
            for i, a in enumerate(boxes)
            for b in boxes[i + 1 :]
        )
        results.add(overlap)
    if len(results) != 1:
        raise AssertionError(
            f"composites: {glyph_name} overlaps at one end of the axis but not the other"
        )
    return results.pop()


def build_composites():
    """Composite glyphs, nested two deep, with gvar deltas on component offsets.

    ``double`` references ``squaredot``, which references ``square`` and ``dot``.
    The component offsets differ between masters, so ``gvar`` carries deltas on
    the component offsets rather than on point coordinates.
    """
    axes = [axis("wght", "Weight", 400, 400, 900)]
    designs = [
        ({"Weight": 400}, "default", 400, 0, 0, True),
        ({"Weight": 900}, "black", 470, 130, 70, False),
    ]
    sources = [
        source(composite_master(size, dx, dy), loc, name, is_default)
        for loc, name, size, dx, dy, is_default in designs
    ]
    vf = make_vf(axes, sources)
    glyf = vf["glyf"]
    if glyf["double"].components[0].glyphName != "squaredot":
        raise AssertionError("composites: `double` must reference `squaredot`")
    if not glyf["squaredot"].isComposite() or not glyf["double"].isComposite():
        raise AssertionError("composites: expected composite glyphs")
    def component_depth(name):
        glyph = glyf[name]
        if not glyph.isComposite():
            return 0
        return 1 + max(component_depth(c.glyphName) for c in glyph.components)

    depth = component_depth("double")
    if depth < 2:
        raise AssertionError(f"composites: `double` nests {depth} deep, want >= 2")
    for name, want_overlap in (("comp.overlap", True), ("comp.plain", False)):
        glyph = glyf[name]
        if not glyph.isComposite() or len(glyph.components) != 2:
            raise AssertionError(f"composites: {name} must be a 2-component composite")
        got = _components_overlap(vf, name)
        if got != want_overlap:
            raise AssertionError(
                f"composites: {name} components overlap={got}, want {want_overlap}"
            )
    for name in ("squaredot", "double"):
        variations = vf["gvar"].variations.get(name)
        if not variations:
            raise AssertionError(f"composites: no gvar deltas for composite {name}")
        # a composite's gvar deltas are one per component plus four phantoms
        n_components = len(glyf[name].components)
        for var in variations:
            if len(var.coordinates) != n_components + 4:
                raise AssertionError(
                    f"composites: {name} gvar tuple has {len(var.coordinates)} deltas, "
                    f"want {n_components} components + 4 phantom points"
                )
            if all(d in (None, (0, 0)) for d in var.coordinates[:n_components]):
                raise AssertionError(
                    f"composites: {name} has no gvar delta on any component offset"
                )
    return vf


#: Windows / Unicode BMP, US English -- the only key the Slice name editor reads or
#: writes, which is exactly what makes a record on any other key worth having here.
WINDOWS_ENGLISH = (3, 1, 0x0409)
#: Windows / Unicode BMP, German (Germany).  1031 == 0x0407.
WINDOWS_GERMAN = (3, 1, 0x0407)

#: The four optional family-name records, at 3/1/1033.  No other fixture in the
#: roster has any of them, so without these the claim that an implementation strips
#: IDs 16, 17, 21 and 22 has nothing to be tested against.  The strings are
#: deliberately different from IDs 1 and 2 so a case can tell which record was read.
OPTIONAL_FAMILY_NAMES = {
    16: "Fixture Family",            # typographic family
    17: "Named Instances",           # typographic subfamily
    21: "Fixture WWS Family",        # WWS family
    22: "Named Instances Regular",   # WWS subfamily
}

#: A German localisation, at 3/1/1031.  Two of the strings are non-ASCII, so the
#: fixture also carries a UTF-16BE round trip through a record the user never typed.
#: Only IDs the editor exposes are localised, and none of them is mirrored onto the
#: Macintosh platform, whose single-byte encodings could not hold "Uberschrift" with
#: its umlaut anyway.
GERMAN_NAMES = {
    1: "FixtureNamedInstances Deutsch",
    2: "Standard",
    16: "Fixture Familie",
    17: "\u00dcberschrift",
}

#: A stylistic-set label, referenced from GSUB rather than from fvar or STAT.  Name
#: IDs above 255 are pruned when the variation tables stop pointing at them, and this
#: one is the control for that: nothing in fvar or STAT ever pointed at it, so it must
#: survive every slice.  276 is the first ID above everything varLib and buildStatTable
#: allocate for this fixture, and :func:`add_stylistic_set` asserts it was free.
SS01_NAME_ID = 276
SS01_LABEL = "Alternate H"

SS01_FEA = """
languagesystem DFLT dflt;
languagesystem latn dflt;

feature ss01 {
    sub H by O;
} ss01;
"""


def add_localised_names(font):
    """Add the optional family names and the German localisation."""
    name = font["name"]
    for name_id, string in sorted(OPTIONAL_FAMILY_NAMES.items()):
        name.setName(string, name_id, *WINDOWS_ENGLISH)
    for name_id, string in sorted(GERMAN_NAMES.items()):
        name.setName(string, name_id, *WINDOWS_GERMAN)
    # fontTools sorts on compile, but sorting here too keeps the in-memory table
    # in the order the specification requires, so the assertion below is about the
    # fixture rather than about when fontTools happens to tidy up.
    name.names.sort()


def add_stylistic_set(font):
    """A GSUB ``ss01`` whose ``FeatureParams`` names a name record.

    feaLib would allocate the label's name ID itself, from wherever the table happens
    to end; the ID is written out explicitly instead so that it is a stable, quotable
    number a case can name, and so that adding an instance or a STAT value later
    cannot silently move it.
    """
    if any(record.nameID == SS01_NAME_ID for record in font["name"].names):
        raise AssertionError(f"named-instances: name ID {SS01_NAME_ID} is already taken")
    addOpenTypeFeaturesFromString(font, SS01_FEA)
    font["name"].setName(SS01_LABEL, SS01_NAME_ID, *WINDOWS_ENGLISH)
    for record in font["GSUB"].table.FeatureList.FeatureRecord:
        if record.FeatureTag == "ss01":
            params = otTables.FeatureParamsStylisticSet()
            params.Version = 0
            params.UINameID = SS01_NAME_ID
            record.Feature.FeatureParams = params
            break
    else:
        raise AssertionError("named-instances: feaLib built no ss01 feature")
    font["name"].names.sort()


def build_named_instances():
    """Several fvar named instances and a STAT with values on every axis."""
    family = "FixtureNamedInstances"
    axes = [
        axis("wght", "Weight", 100, 400, 900),
        axis("wdth", "Width", 75, 100, 125),
    ]
    designs = [
        ({"Weight": 400, "Width": 100}, "default", 90, 1.00, True),
        ({"Weight": 100, "Width": 100}, "thin", 40, 1.00, False),
        ({"Weight": 900, "Width": 100}, "black", 200, 1.00, False),
        ({"Weight": 400, "Width": 75}, "narrow", 90, 0.78, False),
        ({"Weight": 400, "Width": 125}, "wide", 90, 1.22, False),
    ]
    sources = [
        source(two_axis_master(stem, scale, family), loc, name, is_default)
        for loc, name, stem, scale, is_default in designs
    ]
    named = [
        ("Thin", 100, 100),
        ("Light", 300, 100),
        ("Regular", 400, 100),
        ("Bold", 700, 100),
        ("Black", 900, 100),
        ("Condensed Regular", 400, 75),
        ("Condensed Bold", 700, 75),
        ("Expanded Regular", 400, 125),
    ]
    instances = []
    for style, wght, wdth in named:
        inst = InstanceDescriptor()
        inst.familyName = family
        inst.styleName = style
        inst.postScriptFontName = f"{family}-{style.replace(' ', '')}"
        inst.location = {"Weight": wght, "Width": wdth}
        instances.append(inst)
    vf = make_vf(axes, sources, instances)

    ELIDABLE = 0x0002
    buildStatTable(
        vf,
        [
            {
                "tag": "wght",
                "name": "Weight",
                "ordering": 0,
                "values": [
                    {"value": 100, "name": "Thin"},
                    {"value": 300, "name": "Light"},
                    {"value": 400, "name": "Regular", "flags": ELIDABLE},
                    {"value": 700, "name": "Bold"},
                    {"value": 900, "name": "Black"},
                ],
            },
            {
                "tag": "wdth",
                "name": "Width",
                "ordering": 1,
                "values": [
                    {"value": 75, "name": "Condensed"},
                    {"value": 100, "name": "Normal", "flags": ELIDABLE},
                    {"value": 125, "name": "Expanded"},
                ],
            },
        ],
        elidedFallbackName="Regular",
    )
    if len(vf["fvar"].instances) != len(named):
        raise AssertionError("named-instances: fvar instance count is wrong")
    stat = vf["STAT"].table
    tagged = {rec.AxisTag for rec in stat.DesignAxisRecord.Axis}
    if tagged != {"wght", "wdth"}:
        raise AssertionError(f"named-instances: STAT design axes are {tagged}")
    valued = {
        stat.DesignAxisRecord.Axis[v.AxisIndex].AxisTag
        for v in stat.AxisValueArray.AxisValue
    }
    if valued != {"wght", "wdth"}:
        raise AssertionError(
            f"named-instances: STAT has axis values only on {valued}, want every axis"
        )

    add_localised_names(vf)
    add_stylistic_set(vf)
    check_named_instance_names(vf)
    return finalize(vf)


def check_named_instance_names(font):
    """The name records and the GSUB label the corpus expects, all four keys checked."""
    name = font["name"]
    for name_id, string in OPTIONAL_FAMILY_NAMES.items():
        record = name.getName(name_id, *WINDOWS_ENGLISH)
        if record is None or record.toUnicode() != string:
            raise AssertionError(f"named-instances: 3/1/1033 name {name_id} is not {string!r}")
        if name.getName(name_id, 1, 0, 0) is not None:
            raise AssertionError(
                f"named-instances: name {name_id} must not be mirrored onto platform 1"
            )
    for name_id, string in GERMAN_NAMES.items():
        record = name.getName(name_id, *WINDOWS_GERMAN)
        if record is None or record.toUnicode() != string:
            raise AssertionError(f"named-instances: 3/1/1031 name {name_id} is not {string!r}")
        if record.getEncoding() != "utf_16_be":
            raise AssertionError(
                f"named-instances: 3/1/1031 name {name_id} is {record.getEncoding()},"
                " want utf_16_be"
            )
    counts = {1: 3, 2: 3, 16: 2, 17: 2, 21: 1, 22: 1}
    for name_id, want in counts.items():
        got = sum(1 for r in name.names if r.nameID == name_id)
        if got != want:
            raise AssertionError(
                f"named-instances: {got} records with name ID {name_id}, want {want}"
            )
    label = name.getName(SS01_NAME_ID, *WINDOWS_ENGLISH)
    if label is None or label.toUnicode() != SS01_LABEL:
        raise AssertionError(f"named-instances: name {SS01_NAME_ID} is not the ss01 label")
    features = {
        record.FeatureTag: record.Feature
        for record in font["GSUB"].table.FeatureList.FeatureRecord
    }
    if "ss01" not in features:
        raise AssertionError("named-instances: no ss01 feature")
    params = features["ss01"].FeatureParams
    if params is None or params.UINameID != SS01_NAME_ID:
        raise AssertionError(
            "named-instances: ss01 FeatureParams does not point at name "
            f"{SS01_NAME_ID}"
        )
    if not features["ss01"].LookupListIndex:
        raise AssertionError("named-instances: ss01 has no lookups to hang the label on")
    keys = [(r.platformID, r.platEncID, r.langID, r.nameID) for r in name.names]
    if keys != sorted(keys):
        raise AssertionError("named-instances: name records are not in the required order")


# -- overlapping ----------------------------------------------------------

OVERLAP_GLYPHS = [".notdef", "bars", "o", "circled", "bowtie", "clean"]
OVERLAP_CMAP = {
    0x2B: "bars",
    0x6F: "o",
    0x40: "circled",
    0x78: "bowtie",
    0x63: "clean",
}


def overlap_contours(g):
    """The five overlap glyphs, parameterised by a single growth amount ``g``.

    ``g`` is 0 in the default master and positive in the heavy master; every
    boundary moves outward by ``g`` and every counter inward by ``g``, so the
    topology -- and therefore the probe points used to verify the winding --
    is the same at both ends of the axis.
    """
    return {
        # two crossing bars.  Both wound clockwise, so the overlap in the
        # middle has winding -2: filled under non-zero, a hole under even-odd.
        "bars": [
            cw_rect(50, 300 - g, 750, 400 + g),
            cw_rect(350 - g, 0, 450 + g, 700),
        ],
        # an 'o': clockwise outer square with a counter-clockwise counter.
        "o": [
            cw_rect(50 - g, 50 - g, 750 + g, 650 + g),
            ccw_rect(200 + g, 200 + g, 600 - g, 500 - g),
        ],
        # nesting depth 3: ring, its counter, a filled square inside that
        # counter, and that square's own counter.  Directions alternate.
        "circled": [
            cw_rect(20 - g, 20 - g, 780 + g, 680 + g),
            ccw_rect(110, 90, 690, 610),
            cw_rect(200, 160, 600, 540),
            ccw_rect(290 + g, 230 + g, 510 - g, 470 - g),
        ],
        # one self-intersecting contour: the two diagonals cross at (350, 350),
        # making two lobes of opposite winding, both filled under non-zero.
        "bowtie": [
            [(100 - g, 100 - g), (600 + g, 600 + g), (100 - g, 600 + g), (600 + g, 100 - g)]
        ],
        # the control: one clockwise triangle.  No second contour to overlap, and
        # three straight edges that cannot cross each other, so there is nothing
        # for an overlap remover to do and it must hand the glyph back untouched.
        "clean": [
            [(400, 650 + g), (700 + g, 0), (100 - g, 0)]
        ],
    }


#: (glyph, probe point, expected |winding|).  The probes sit in every distinct
#: region of every glyph, including the ones that must be empty.  They are
#: chosen to stay in the same region across the whole axis.
OVERLAP_PROBES = [
    ("bars", (400, 350), 2, "both bars overlap"),
    ("bars", (150, 350), 1, "horizontal bar only"),
    ("bars", (400, 620), 1, "vertical bar only"),
    ("bars", (150, 620), 0, "outside both bars"),
    ("bars", (900, 350), 0, "right of everything"),
    ("o", (100, 350), 1, "the ring band"),
    ("o", (400, 350), 0, "the counter"),
    ("o", (900, 350), 0, "outside"),
    ("circled", (60, 350), 1, "outer ring band"),
    ("circled", (150, 350), 0, "the outer counter"),
    ("circled", (240, 350), 1, "the inner filled square"),
    ("circled", (400, 350), 0, "the counter inside the counter"),
    ("circled", (900, 350), 0, "outside"),
    ("bowtie", (350, 500), 1, "upper lobe"),
    ("bowtie", (350, 200), 1, "lower lobe"),
    ("bowtie", (150, 350), 0, "outside, level with the crossing"),
    ("bowtie", (900, 350), 0, "outside"),
    ("clean", (400, 100), 1, "inside the triangle, once and only once"),
    ("clean", (400, 400), 1, "inside, high up where the sides close in"),
    ("clean", (150, 500), 0, "outside, left of the rising edge"),
    ("clean", (650, 500), 0, "outside, right of the falling edge"),
    ("clean", (400, 720), 0, "outside, above the apex"),
    ("clean", (900, 350), 0, "outside"),
]

#: Expected orientation of each contour, as the sign of its signed area.
#: -1 is clockwise (a TrueType outer contour), +1 counter-clockwise (a counter).
OVERLAP_ORIENTATIONS = {
    "bars": [-1, -1],
    "o": [-1, +1],
    "circled": [-1, +1, -1, +1],
    "clean": [-1],
}

#: Glyph -> the largest winding magnitude that may appear anywhere in it.  1 means
#: "nothing anywhere in this glyph is covered twice", which is the property that
#: makes ``clean`` a control; ``bars`` is listed as its opposite number.
OVERLAP_MAX_WINDING = {"clean": 1, "bars": 2, "o": 1, "circled": 1, "bowtie": 1}


def overlap_master(g):
    adv = {name: (820, 40) for name in OVERLAP_GLYPHS}
    adv[".notdef"] = (600, 50)
    fb = new_master(OVERLAP_GLYPHS, OVERLAP_CMAP, adv, "FixtureOverlapping")
    glyphs = {
        ".notdef": simple_glyph([cw_rect(50, 0, 550, 700), ccw_rect(120, 70, 480, 630)])
    }
    for name, contours in overlap_contours(g).items():
        glyphs[name] = simple_glyph(contours)
    fb.setupGlyf(glyphs)
    return fb.font


def build_overlapping():
    axes = [axis("wght", "Weight", 400, 400, 900)]
    sources = [
        source(overlap_master(0), {"Weight": 400}, "default", True),
        source(overlap_master(40), {"Weight": 900}, "black", False),
    ]
    return make_vf(axes, sources)


def scan_max_winding(contours, samples=(53, 47)):
    """Largest |winding| anywhere in the glyph, from a grid over its bounding box.

    The probe list says what the winding is in the regions the author thought of.
    This says what it is everywhere, which is the only way to state the negative
    property ``clean`` exists for: *no* part of it is covered twice.  The grid is
    offset by half a step and uses two counts that share no factor, so a sample
    landing exactly on an edge -- where the winding is not defined -- is unlikely,
    and the strides never line up with an axis-aligned edge of these glyphs.
    """
    xs = [x for contour in contours for x, _ in contour]
    ys = [y for contour in contours for _, y in contour]
    nx, ny = samples
    worst = 0
    where = None
    for i in range(nx):
        x = min(xs) - 20 + (max(xs) - min(xs) + 40) * ((i + 0.5) / nx)
        for j in range(ny):
            y = min(ys) - 20 + (max(ys) - min(ys) + 40) * ((j + 0.5) / ny)
            w = abs(winding_number((x, y), contours))
            if w > worst:
                worst, where = w, (x, y)
    return worst, where


def check_overlapping_windings(font, report):
    """Prove the five overlap glyphs are filled the way they are meant to be.

    Reads the contours back out of the compiled ``glyf`` (not out of the source
    data structures), checks contour orientation by signed area, and evaluates
    the non-zero winding number at a probe point in every distinct region --
    at the axis default and at the axis maximum.
    """
    for location, label in (({"wght": 400}, "wght=400 (default)"), ({"wght": 900}, "wght=900 (max)")):
        static = instantiateVariableFont(copy.deepcopy(font), location, inplace=False)
        for glyph_name, expected in OVERLAP_ORIENTATIONS.items():
            contours = glyph_contours(static, glyph_name)
            got = [1 if signed_area(c) > 0 else -1 for c in contours]
            if got != expected:
                raise AssertionError(
                    f"overlapping/{glyph_name} at {label}: contour orientations "
                    f"{got}, expected {expected}"
                )
            report(
                f"    {label} {glyph_name}: orientations "
                + ", ".join("CW" if s < 0 else "CCW" for s in got)
            )
        # the bowtie is self-intersecting, so a single orientation is
        # meaningless; report its (near-zero) signed area instead.
        bowtie = glyph_contours(static, "bowtie")
        assert len(bowtie) == 1, "bowtie must be a single contour"
        report(
            f"    {label} bowtie: 1 self-intersecting contour, "
            f"signed area {signed_area(bowtie[0]):.0f} (two opposing lobes)"
        )
        for glyph_name, point, expected_abs, why in OVERLAP_PROBES:
            contours = glyph_contours(static, glyph_name)
            w = winding_number(point, contours)
            if abs(w) != expected_abs:
                raise AssertionError(
                    f"overlapping/{glyph_name} at {label}: winding at {point} "
                    f"is {w}, expected magnitude {expected_abs} ({why})"
                )
            report(
                f"    {label} {glyph_name} {point}: winding {w:+d} -> "
                f"{'FILLED' if w else 'empty '}  ({why})"
            )
        for glyph_name, expected_max in sorted(OVERLAP_MAX_WINDING.items()):
            contours = glyph_contours(static, glyph_name)
            worst, where = scan_max_winding(contours)
            if worst != expected_max:
                raise AssertionError(
                    f"overlapping/{glyph_name} at {label}: largest |winding| over "
                    f"the glyph is {worst} (at {where}), expected {expected_max}"
                )
            verdict = (
                "no region is covered twice -- nothing to remove"
                if worst <= 1
                else f"{worst} layers deep somewhere -- there is an overlap to remove"
            )
            report(
                f"    {label} {glyph_name}: largest |winding| anywhere is "
                f"{worst}, {verdict}"
            )


# -- hinted ---------------------------------------------------------------

HINTED_GLYPHS = [".notdef", "H", "I"]
HINTED_CMAP = {0x48: "H", 0x49: "I"}

FPGM_ASM = [
    # function 0: round the point number on the stack to the grid
    "PUSHB[ ]",
    "0",
    "FDEF[ ]",
    "MDAP[1]",
    "ENDF[ ]",
]

PREP_ASM = [
    # turn dropout control on, and pick a scan type
    "PUSHW[ ]",
    "511",
    "SCANCTRL[ ]",
    "PUSHB[ ]",
    "4",
    "SCANTYPE[ ]",
]

#: cvt: 0 = baseline, 1 = cap height, 2 = descender, 3 = stem width
CVT_VALUES = [0, 700, -200, 90]

GLYPH_ASM = [
    # measure along the y axis, then round point 0 to the grid via fpgm fn 0
    "SVTCA[0]",
    "PUSHB[ ]",
    "0",
    "0",
    "CALL[ ]",
]


def hinted_master(stem):
    adv = {".notdef": (600, 50), "H": (560, 60), "I": (300, 90)}
    fb = new_master(HINTED_GLYPHS, HINTED_CMAP, adv, "FixtureHinted")
    glyphs = {
        ".notdef": simple_glyph([cw_rect(50, 0, 550, 700), ccw_rect(120, 70, 480, 630)]),
        "H": simple_glyph(
            [
                cw_rect(60, 0, 60 + stem, 700),
                cw_rect(500 - stem, 0, 500, 700),
                cw_rect(60 + stem, 300, 500 - stem, 300 + stem // 2),
            ]
        ),
        "I": simple_glyph([cw_rect(90, 0, 90 + stem, 700)]),
    }
    program = Program()
    program.fromAssembly(list(GLYPH_ASM))
    for name in ("H", "I"):
        glyphs[name].program = program
    fb.setupGlyf(glyphs)

    font = fb.font
    fpgm = newTable("fpgm")
    fpgm.program = Program()
    fpgm.program.fromAssembly(list(FPGM_ASM))
    font["fpgm"] = fpgm
    prep = newTable("prep")
    prep.program = Program()
    prep.program.fromAssembly(list(PREP_ASM))
    font["prep"] = prep
    cvt = newTable("cvt ")
    cvt.values = array("h", CVT_VALUES)
    font["cvt "] = cvt
    # maxp fields the hinting needs; these are not recalculated on compile.
    maxp = font["maxp"]
    maxp.maxZones = 2
    maxp.maxTwilightPoints = 16
    maxp.maxStorage = 8
    maxp.maxFunctionDefs = 1
    maxp.maxInstructionDefs = 0
    maxp.maxStackElements = 32
    return font


def build_hinted():
    """``prep``, ``fpgm``, ``cvt `` and per-glyph instructions, on a wght axis."""
    axes = [axis("wght", "Weight", 400, 400, 900)]
    sources = [
        source(hinted_master(90), {"Weight": 400}, "default", True),
        source(hinted_master(200), {"Weight": 900}, "black", False),
    ]
    vf = make_vf(axes, sources)
    for table in ("fpgm", "prep", "cvt "):
        if table not in vf:
            raise AssertionError(f"hinted: missing {table!r}")
    if list(vf["cvt "].values) != CVT_VALUES:
        raise AssertionError("hinted: cvt values did not survive")
    for name in ("H", "I"):
        program = getattr(vf["glyf"][name], "program", None)
        if program is None or not program.getBytecode():
            raise AssertionError(f"hinted: glyph {name} has no instructions")
    if vf["maxp"].maxFunctionDefs < 1:
        raise AssertionError("hinted: maxp.maxFunctionDefs must cover the fpgm FDEF")

    # `maxp.maxSizeOfInstructions` sizes the interpreter's instruction buffer and the
    # specification requires it to cover the largest glyph program in the font. Nothing
    # in the varLib build path computes it -- the merged font inherits the field from a
    # master that was assembled programmatically and never compiled through `glyf` --
    # so it arrives here as 0 in front of live programs. A fixture that ships malformed
    # makes `static.hinting.maxp-instruction-limits-cover-the-programs` unpassable by any
    # implementation that correctly leaves the instruction maxima alone, which is what
    # that case's own rationale requires. Set it here rather than in the case.
    longest = max(
        (len(g.program.getBytecode()) for g in vf["glyf"].glyphs.values()
         if hasattr(g, "program")),
        default=0,
    )
    vf["maxp"].maxSizeOfInstructions = longest
    if longest < 1:
        raise AssertionError("hinted: expected at least one glyph program")
    return vf


# -- gdef-varstore --------------------------------------------------------

KERN_GLYPHS = [".notdef", "A", "V", "T"]
KERN_CMAP = {0x41: "A", 0x56: "V", 0x54: "T"}

KERN_FEA = """
languagesystem DFLT dflt;
languagesystem latn dflt;

feature kern {
    pos A V %(av)d;
    pos V A %(va)d;
    pos T A %(ta)d;
} kern;
"""


def kern_master(stem, av, va, ta):
    adv = {".notdef": (600, 50), "A": (620, 20), "V": (620, 20), "T": (580, 10)}
    fb = new_master(KERN_GLYPHS, KERN_CMAP, adv, "FixtureGdefVarStore")
    glyphs = {
        ".notdef": simple_glyph([cw_rect(50, 0, 550, 700), ccw_rect(120, 70, 480, 630)]),
        "A": simple_glyph([cw_rect(40, 0, 40 + stem, 700)]),
        "V": simple_glyph([cw_rect(560 - stem, 0, 560, 700)]),
        "T": simple_glyph([cw_rect(40, 700 - stem, 540, 700)]),
    }
    fb.setupGlyf(glyphs)
    addOpenTypeFeaturesFromString(fb.font, KERN_FEA % {"av": av, "va": va, "ta": ta})
    return fb.font


def build_gdef_varstore():
    """Variable kerning: a GDEF item variation store referenced from GPOS.

    Two masters with the same ``kern`` feature and different values.  varLib
    merges the pair positioning into variable value records, which means a
    ``Device`` table of format ``VariationIndex`` pointing into the item
    variation store it puts in ``GDEF``.
    """
    axes = [
        axis("wght", "Weight", 400, 400, 900),
        # A second axis, so the fixture can express the case where one axis is pinned
        # away from its default while another is narrowed. That is the one partial job a
        # GDEF store makes impossible without rewriting the GPOS values, and with a single
        # axis there is no way to ask for it.
        axis("wdth", "Width", 50, 100, 100),
    ]
    sources = [
        source(kern_master(90, -40, -20, -60), {"Weight": 400, "Width": 100},
               "default", True),
        source(kern_master(200, -120, -70, -150), {"Weight": 900, "Width": 100},
               "black", False),
        # Condensed kerns tighter, so the store has a wdth region with its own deltas and
        # pinning wdth off its default genuinely moves the kerning.
        source(kern_master(90, -70, -45, -95), {"Weight": 400, "Width": 50},
               "condensed", False),
    ]
    vf = make_vf(axes, sources)
    gdef = vf["GDEF"].table
    if getattr(gdef, "VarStore", None) is None:
        raise AssertionError("gdef-varstore: GDEF has no item variation store")
    if not _gpos_has_variation_index(vf):
        raise AssertionError("gdef-varstore: no GPOS value record references the store")
    if len(vf["fvar"].axes) != 2:
        raise AssertionError("gdef-varstore: expected two axes")
    # The wdth axis has to actually reach the store, or the case it exists for is vacuous.
    regions = gdef.VarStore.VarRegionList.Region
    if not any(r.VarRegionAxis[1].PeakCoord != 0 for r in regions):
        raise AssertionError("gdef-varstore: no variation region touches wdth")
    return vf


def _gpos_has_variation_index(font):
    for lookup in font["GPOS"].table.LookupList.Lookup:
        for sub in lookup.SubTable:
            for pair_set in getattr(sub, "PairSet", None) or []:
                for record in pair_set.PairValueRecord:
                    for value in (record.Value1, record.Value2):
                        if value is None:
                            continue
                        for attr in ("XAdvDevice", "XPlaDevice", "YAdvDevice", "YPlaDevice"):
                            device = getattr(value, attr, None)
                            if device is not None and device.DeltaFormat == 0x8000:
                                return True
    return False


# -- cff2 -----------------------------------------------------------------

CFF2_GLYPHS = [".notdef", "H", "I", "period"]
CFF2_CMAP = {0x48: "H", 0x49: "I", 0x2E: "period"}


def cff2_master(stem):
    adv = {".notdef": (600, 50), "H": (560, 60), "I": (300, 90), "period": (300, 90)}
    fb = new_master(CFF2_GLYPHS, CFF2_CMAP, adv, "FixtureCff2", is_ttf=False)
    # CFF outlines follow the PostScript convention: outer contours
    # counter-clockwise, counters clockwise -- the opposite of TrueType.
    outlines = {
        ".notdef": [ccw_rect(50, 0, 550, 700), cw_rect(120, 70, 480, 630)],
        "H": [
            ccw_rect(60, 0, 60 + stem, 700),
            ccw_rect(500 - stem, 0, 500, 700),
            ccw_rect(60 + stem, 300, 500 - stem, 300 + stem // 2),
        ],
        "I": [ccw_rect(90, 0, 90 + stem, 700)],
        "period": [ccw_rect(90, 0, 90 + stem, stem)],
    }
    charstrings = {}
    for name, contours in outlines.items():
        pen = T2CharStringPen(adv[name][0], None)
        draw_contours(pen, contours)
        charstrings[name] = pen.getCharString()
    fb.setupCFF(
        "FixtureCff2-Regular",
        {
            "FullName": "FixtureCff2 Regular",
            "FamilyName": "FixtureCff2",
            "Weight": "Regular",
            "version": "1.000",
        },
        charstrings,
        {},
    )
    return fb.font


def build_cff2():
    """CFF2 outlines: varLib turns CFF masters into a CFF2 variable font."""
    axes = [axis("wght", "Weight", 400, 400, 900)]
    sources = [
        source(cff2_master(90), {"Weight": 400}, "default", True),
        source(cff2_master(220), {"Weight": 900}, "black", False),
    ]
    vf = make_vf(axes, sources)
    if "CFF2" not in vf:
        raise AssertionError("cff2-vf: no CFF2 table was produced")
    return vf


# -- static-ttf ------------------------------------------------------------

def build_static_ttf():
    """A TrueType font with no ``fvar``: Recursive, already sliced.

    Copied verbatim from ``testdata/fonts/Recursive-Sliced.subset.ttf`` -- an
    instance of the same family ``recursive-vf`` comes from, so a case that hands
    it to a slicer is handing over a real static font rather than a stripped-down
    imitation of one.  Every other fixture in the roster except ``cff2-vf`` is
    variable, so without this there is nothing to test the refusal against.
    """
    data = RECURSIVE_STATIC.read_bytes()
    font = TTFont(io.BytesIO(data), recalcTimestamp=False, lazy=True)
    for table in ("fvar", "gvar", "avar", "HVAR", "MVAR", "STAT"):
        if table in font:
            raise AssertionError(f"static-ttf: {table} is present; it must not be variable")
    if "glyf" not in font:
        raise AssertionError("static-ttf: expected TrueType outlines")
    font.close()
    return data


# -- with-dsig -------------------------------------------------------------

def build_with_dsig():
    """``two-axis`` again, plus a stub ``DSIG``.

    ``DSIG`` is a signature over the whole font file, so instancing invalidates it
    and it has to be dropped rather than carried over.  No fixture in the roster
    had one -- ``recursive-vf`` does not, and it is copied verbatim so it cannot
    gain one without the 170-odd cases that compare against it changing underneath
    them -- which made the deletion claim vacuous: the check passed on a font that
    never had the table.  A separate fixture makes it real without disturbing
    anything.  The stub is the empty, unsigned form (``usNumSigs`` 0) that font
    tools routinely emit, which is enough: the property under test is that the
    table is gone, not what was in it.
    """
    vf = build_two_axis(with_avar=True, family="FixtureWithDsig")
    dsig = newTable("DSIG")
    dsig.ulVersion = 1
    dsig.usFlag = 1
    dsig.usNumSigs = 0
    dsig.signatureRecords = []
    vf["DSIG"] = dsig
    if "DSIG" not in vf:
        raise AssertionError("with-dsig: the DSIG table did not stick")
    return finalize(vf)


# --------------------------------------------------------------------------
# verification
# --------------------------------------------------------------------------

def verify_font(name, data, report):
    """Open, round-trip, structurally check and (if variable) instance a font."""
    flavor = None
    if name.endswith(".woff"):
        flavor = "woff"
    elif name.endswith(".woff2"):
        flavor = "woff2"

    font = TTFont(io.BytesIO(data), recalcTimestamp=False)
    if font.flavor != flavor:
        raise AssertionError(f"{name}: flavor is {font.flavor!r}, expected {flavor!r}")

    outline_tables = {"glyf", "CFF ", "CFF2"} & set(font.keys())
    if not outline_tables:
        raise AssertionError(f"{name}: no outline table")
    missing = REQUIRED_TABLES - set(font.keys())
    if missing:
        raise AssertionError(f"{name}: missing required tables {sorted(missing)}")
    if "glyf" in font and "loca" not in font:
        raise AssertionError(f"{name}: glyf without loca")

    hmtx = font["hmtx"]
    for glyph_name in font.getGlyphOrder():
        advance = hmtx[glyph_name][0]
        if advance <= 0:
            raise AssertionError(f"{name}: glyph {glyph_name} has advance {advance}")

    # round trip
    buf = io.BytesIO()
    font.save(buf)
    buf.seek(0)
    reopened = TTFont(buf, recalcTimestamp=False)
    reopened.getGlyphOrder()
    for table in sorted(reopened.keys()):
        reopened[table]  # force decompile of everything

    detail = f"{sorted(outline_tables)[0]}, {font['maxp'].numGlyphs} glyphs"
    if "fvar" in font:
        axes = font["fvar"].axes
        detail += ", axes " + " ".join(
            f"{a.axisTag} {a.minValue:g}/{a.defaultValue:g}/{a.maxValue:g}" for a in axes
        )
        detail += f", {len(font['fvar'].instances)} named instances"
        locations = [{a.axisTag: a.defaultValue for a in axes}]
        for a in axes:
            for value in (a.minValue, a.maxValue):
                loc = {b.axisTag: b.defaultValue for b in axes}
                loc[a.axisTag] = value
                locations.append(loc)
        for loc in locations:
            static = instantiateVariableFont(copy.deepcopy(font), loc, inplace=False)
            if "fvar" in static:
                raise AssertionError(f"{name}: fvar survived pinning every axis at {loc}")
            sbuf = io.BytesIO()
            static.save(sbuf)
            TTFont(io.BytesIO(sbuf.getvalue()), recalcTimestamp=False).getGlyphOrder()
        detail += f", instanced at {len(locations)} locations"
    else:
        detail += ", static"
    if name == "with-dsig.ttf" and "DSIG" not in font:
        raise AssertionError("with-dsig.ttf: DSIG did not survive serialisation")
    if name == "static-ttf.ttf" and "fvar" in font:
        raise AssertionError("static-ttf.ttf: must not have an fvar")

    optional = sorted({"avar", "gvar", "STAT", "HVAR", "GDEF", "GPOS", "GSUB",
                       "fpgm", "prep", "cvt ", "DSIG"} & set(font.keys()))
    if optional:
        detail += ", has " + " ".join(t.strip() for t in optional)
    report(f"    {detail}")
    return font


def build_all(report):
    """Return ``[(filename, bytes), ...]`` for the whole roster."""
    fonts = []
    fonts.extend(build_recursive_family())
    fonts.append(("two-axis.ttf", to_bytes(build_two_axis(with_avar=True))))
    fonts.append(("no-avar.ttf", to_bytes(build_two_axis(with_avar=False))))
    fonts.append(("centred-default.ttf", to_bytes(build_centred_default())))
    fonts.append(
        ("single-axis-min-default.ttf", to_bytes(build_single_axis_min_default()))
    )
    fonts.append(("composites.ttf", to_bytes(build_composites())))
    fonts.append(("named-instances.ttf", to_bytes(build_named_instances())))

    overlapping = build_overlapping()
    check_overlapping_windings(overlapping, report)
    fonts.append(("overlapping.ttf", to_bytes(overlapping)))

    fonts.append(("hinted.ttf", to_bytes(build_hinted())))
    fonts.append(("gdef-varstore.ttf", to_bytes(build_gdef_varstore())))
    fonts.append(("cff2-vf.otf", to_bytes(build_cff2())))
    fonts.append(("static-ttf.ttf", build_static_ttf()))
    fonts.append(("with-dsig.ttf", to_bytes(build_with_dsig())))
    return fonts


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check",
        action="store_true",
        help="build and verify but do not write anything",
    )
    args = parser.parse_args(argv)

    for path in (RECURSIVE_TTF, RECURSIVE_STATIC):
        if not path.is_file():
            print(f"missing source font: {path}", file=sys.stderr)
            return 2

    lines = []
    report = lines.append

    print("building and verifying the winding of the `overlapping` glyphs")
    fonts = build_all(report)
    for line in lines:
        print(line)

    print("\nrebuilding to confirm determinism")
    again = build_all(lambda _line: None)
    if [n for n, _ in fonts] != [n for n, _ in again]:
        print("  FAIL: the two runs produced different files", file=sys.stderr)
        return 1
    for (name, first), (_, second) in zip(fonts, again):
        if first != second:
            print(f"  FAIL: {name} is not reproducible", file=sys.stderr)
            return 1
    print(f"  ok: {len(fonts)} fonts identical across two runs")

    print("\nverifying")
    for name, data in fonts:
        print(f"  {name}")
        verify_font(name, data, print)

    if args.check:
        print("\n--check: nothing written")
        return 0

    OUT.mkdir(parents=True, exist_ok=True)
    stale = {p.name for p in OUT.iterdir() if p.is_file()} - {n for n, _ in fonts}
    for name in sorted(stale):
        (OUT / name).unlink()
        print(f"\nremoved stale {name}")

    print("\nwriting to", OUT.relative_to(REPO))
    total = 0
    for name, data in fonts:
        (OUT / name).write_bytes(data)
        total += len(data)
        print(f"  {name:30s} {len(data):7d} bytes  sha256:{hashlib.sha256(data).hexdigest()[:16]}")
    print(f"  {'total':30s} {total:7d} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
