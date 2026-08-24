#!/usr/bin/env python3
"""Evaluate a case's expectations against a font, using fontTools.

Both runners hand their output here. Neither implementation evaluates its own result:
the checker is written once, against fontTools, and knows nothing about which program
produced the bytes it is given.

Every check kind in `tests/suite/README.md` is implemented here, and nowhere else. If a
case names a kind this does not implement, the case fails with `unknown check kind`
rather than silently passing — a corpus that quietly ignores what it does not understand
is worse than no corpus.
"""

from __future__ import annotations

import io

import math
from dataclasses import dataclass, field
from typing import Any

from fontTools import ttLib
from fontTools.pens.recordingPen import DecomposingRecordingPen


# --------------------------------------------------------------------------- result

@dataclass
class CheckResult:
    kind: str
    passed: bool
    detail: str = ""


@dataclass
class CaseResult:
    case_id: str
    passed: bool
    reason: str = ""
    checks: list[CheckResult] = field(default_factory=list)

    @property
    def failures(self) -> list[CheckResult]:
        return [c for c in self.checks if not c.passed]


# ------------------------------------------------------------------------- geometry

def _flatten(pen_value, steps: int = 16) -> list[list[tuple[float, float]]]:
    """Turn recorded pen output into closed polylines.

    Curves are flattened. Every geometric check here is about which points are inside a
    glyph, or whether edges cross, and both survive flattening at this resolution: the
    tolerances used are whole font units and the error from 16 segments per curve on a
    1000-unit em is far below that.
    """
    contours: list[list[tuple[float, float]]] = []
    current: list[tuple[float, float]] = []
    start = (0.0, 0.0)
    here = (0.0, 0.0)

    def quad(p0, p1, p2):
        for i in range(1, steps + 1):
            t = i / steps
            u = 1 - t
            yield (
                u * u * p0[0] + 2 * u * t * p1[0] + t * t * p2[0],
                u * u * p0[1] + 2 * u * t * p1[1] + t * t * p2[1],
            )

    def cubic(p0, p1, p2, p3):
        for i in range(1, steps + 1):
            t = i / steps
            u = 1 - t
            yield (
                u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
                u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1],
            )

    for op, args in pen_value:
        if op == "moveTo":
            if len(current) > 2:
                contours.append(current)
            here = start = tuple(args[0])
            current = [here]
        elif op == "lineTo":
            here = tuple(args[0])
            current.append(here)
        elif op == "qCurveTo":
            points = [tuple(p) for p in args if p is not None]
            # A qCurveTo may carry several off-curve points with implied on-curve
            # midpoints between them, which is how TrueType stores runs of curves.
            for i, control in enumerate(points[:-1]):
                end = points[i + 1] if i + 1 == len(points) - 1 else (
                    (control[0] + points[i + 1][0]) / 2,
                    (control[1] + points[i + 1][1]) / 2,
                )
                current.extend(quad(here, control, end))
                here = end
        elif op == "curveTo":
            points = [tuple(p) for p in args]
            current.extend(cubic(here, points[0], points[1], points[2]))
            here = points[2]
        elif op in ("closePath", "endPath"):
            if len(current) > 2:
                contours.append(current)
            current = []
            here = start
    if len(current) > 2:
        contours.append(current)
    return contours


def _winding(contours, x: float, y: float) -> int:
    """Winding number of a point, the rule `glyf` is filled with."""
    total = 0
    for contour in contours:
        n = len(contour)
        for i in range(n):
            x0, y0 = contour[i]
            x1, y1 = contour[(i + 1) % n]
            if y0 <= y:
                if y1 > y and (x1 - x0) * (y - y0) - (x - x0) * (y1 - y0) > 0:
                    total += 1
            elif y1 <= y and (x1 - x0) * (y - y0) - (x - x0) * (y1 - y0) < 0:
                total -= 1
    return total


def _segments_cross(a0, a1, b0, b1) -> bool:
    def orient(p, q, r):
        v = (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
        if abs(v) < 1e-9:
            return 0
        return 1 if v > 0 else -1

    d1, d2 = orient(b0, b1, a0), orient(b0, b1, a1)
    d3, d4 = orient(a0, a1, b0), orient(a0, a1, b1)
    return d1 != d2 and d3 != d4


# --------------------------------------------------------------------------- drawing

def _draw(font: ttLib.TTFont, glyph_name: str, location: dict | None) -> list:
    """Recorded pen output for one glyph, optionally at a variation location.

    The pen decomposes. A plain `RecordingPen` records `addComponent` verbatim, so a
    composite glyph arrives here as a list of glyph *names* and transforms with no
    geometry in it at all: `_flatten` then sees no contours and reports the glyph as
    empty, and `_round_ops` is handed a string and raises. Every check built on this
    function asks a question about the filled shape -- which points are inside, do edges
    cross, do two fonts draw the same thing -- and the answer for a composite is the
    shape its components make. Composite-ness itself is not lost, because
    `glyph_component_count` reads it from the `glyf` table directly.
    """
    if location:
        glyph_set = font.getGlyphSet(location=location)
    else:
        glyph_set = font.getGlyphSet()
    pen = DecomposingRecordingPen(glyph_set)
    glyph_set[glyph_name].draw(pen)
    return pen.value


def _round_ops(value, places: int = 3):
    out = []
    for op, args in value:
        rounded = []
        for point in args:
            rounded.append(None if point is None else tuple(round(c, places) for c in point))
        out.append((op, tuple(rounded)))
    return out


def _compare_outlines(reference, actual, tolerance: float) -> str | None:
    """None when they agree within tolerance, else a description of the disagreement."""
    if len(reference) != len(actual):
        return f"{len(reference)} operations in the source, {len(actual)} in the output"
    for index, ((op_a, args_a), (op_b, args_b)) in enumerate(zip(reference, actual)):
        if op_a != op_b:
            return f"operation {index}: source has {op_a}, output has {op_b}"
        if len(args_a) != len(args_b):
            return f"operation {index}: different number of points"
        for pa, pb in zip(args_a, args_b):
            if pa is None or pb is None:
                if pa is not pb:
                    return f"operation {index}: one point is implied and the other is not"
                continue
            for ca, cb in zip(pa, pb):
                if abs(ca - cb) > tolerance:
                    return (
                        f"operation {index} ({op_a}): {ca} vs {cb}, "
                        f"a difference of {abs(ca - cb):.3f} units"
                    )
    return None


# ---------------------------------------------------------------------------- checks

class Checker:
    def __init__(self, output_path: str, source_path: str, peers: dict | None = None):
        self.font = ttLib.TTFont(output_path)
        self.output_path = output_path
        self.source_path = source_path
        # Outputs of other cases in the same run, by case id. Round-tripping is an
        # equality between two *outputs*, and checking each against the source
        # separately would give each its own slack.
        self.peers = peers or {}
        self._source = None
        self._instanced: dict[tuple, ttLib.TTFont] = {}

    @property
    def source(self) -> ttLib.TTFont:
        if self._source is None:
            self._source = ttLib.TTFont(self.source_path)
        return self._source

    def _source_instanced_at(self, location: dict) -> ttLib.TTFont:
        """The source actually instanced at `location`, as the reference to compare to.

        Drawing the source through `getGlyphSet(location=...)` interpolates in floating
        point and hands back unrounded coordinates -- 224.5 where the outline of any real
        instance holds 225, because `glyf` stores integers. Comparing an instanced font
        against that reference measures the rounding, not the instancing, and it cannot be
        satisfied at all by a correct implementation at a tolerance below half a unit.
        `instantiateVariableFont` is the reference these cases mean: it rounds the same
        way a font must, so a tolerance of zero at a corner of the design space becomes a
        real and meetable claim about the delta arithmetic.
        """
        key = tuple(sorted(location.items()))
        if key not in self._instanced:
            from fontTools.varLib.instancer import instantiateVariableFont

            font = ttLib.TTFont(self.source_path)
            if location and "fvar" in font:
                instantiateVariableFont(font, dict(location), inplace=True)
                # Compile and reload. `instantiateVariableFont` leaves the interpolated
                # `glyf` coordinates as Python floats in memory -- 224.5 where the font
                # on disk will hold 225 -- because rounding happens when the table is
                # compiled. Comparing against the in-memory form measures nothing but
                # that rounding, and no font can reproduce it.
                buffer = io.BytesIO()
                font.save(buffer)
                buffer.seek(0)
                font = ttLib.TTFont(buffer)
            self._instanced[key] = font
        return self._instanced[key]

    # -- structure

    def parses_as_font(self, _):
        return True, f"{len(self.font.keys())} tables"

    def has_table(self, spec):
        tag = spec["table"]
        return tag in self.font, f"tables: {sorted(self.font.keys())}"

    def no_table(self, spec):
        tag = spec["table"]
        return tag not in self.font, f"tables: {sorted(self.font.keys())}"

    def glyph_count(self, spec):
        actual = self.font["maxp"].numGlyphs
        return actual == spec["equals"], f"numGlyphs={actual}"

    def sfnt_flavor(self, spec):
        want = spec["equals"]
        flavor = self.font.flavor  # None, "woff" or "woff2"
        if want in ("woff", "woff2"):
            return flavor == want, f"flavor={flavor}"
        outlines = "cff" if ("CFF " in self.font or "CFF2" in self.font) else "truetype"
        return flavor is None and outlines == want, f"flavor={flavor} outlines={outlines}"

    # -- axes

    def axis_count(self, spec):
        n = len(self.font["fvar"].axes) if "fvar" in self.font else 0
        return n == spec["equals"], f"{n} axes"

    def axis_tags(self, spec):
        tags = [a.axisTag for a in self.font["fvar"].axes] if "fvar" in self.font else []
        return tags == spec["equals"], f"tags={tags}"

    #: One ulp of the 16.16 `Fixed` type that `fvar` stores axis values in. A value
    #: written to an axis extent is quantized to a multiple of this on the way into the
    #: file, so 0.95 comes back as 0.9499969482421875 and no implementation can do
    #: better. Comparing exactly would be testing the storage format, not the program.
    FIXED_ULP = 1.0 / 65536

    def axis_extent(self, spec):
        if "fvar" not in self.font:
            return False, "no fvar"
        for axis in self.font["fvar"].axes:
            if axis.axisTag == spec["tag"]:
                actual = (axis.minValue, axis.defaultValue, axis.maxValue)
                want = (spec["min"], spec["default"], spec["max"])
                agrees = all(
                    abs(a - w) <= self.FIXED_ULP for a, w in zip(actual, want)
                )
                return agrees, f"{spec['tag']} is {actual}, expected {want}"
        return False, f"no {spec['tag']} axis"

    def named_instance_count(self, spec):
        n = len(self.font["fvar"].instances) if "fvar" in self.font else 0
        return n == spec["equals"], f"{n} instances"

    def named_instances_within_extent(self, _):
        if "fvar" not in self.font:
            return True, "no fvar"
        extents = {a.axisTag: (a.minValue, a.maxValue) for a in self.font["fvar"].axes}
        for instance in self.font["fvar"].instances:
            for tag, value in instance.coordinates.items():
                low, high = extents.get(tag, (value, value))
                if value < low or value > high:
                    return False, f"instance at {tag}={value}, outside {low}..{high}"
        return True, f"{len(self.font['fvar'].instances)} instances all in range"

    # -- table fields

    def _field(self, table, spec):
        if table not in self.font:
            return False, f"no {table} table"
        actual = getattr(self.font[table], spec["field"])
        want = spec["equals"]
        if isinstance(want, float) or isinstance(actual, float):
            ok = math.isclose(float(actual), float(want), abs_tol=1e-6)
        else:
            ok = actual == want
        return ok, f"{table}.{spec['field']}={actual}, expected {want}"

    def os2_field(self, spec):
        return self._field("OS/2", spec)

    def head_field(self, spec):
        return self._field("head", spec)

    def hhea_field(self, spec):
        return self._field("hhea", spec)

    def post_field(self, spec):
        return self._field("post", spec)

    def maxp_field(self, spec):
        return self._field("maxp", spec)

    def _bit(self, spec):
        if spec["field"] == "fsSelection":
            value = self.font["OS/2"].fsSelection
        else:
            value = self.font["head"].macStyle
        return value, bool(value & (1 << int(spec["bit"])))

    def bit_set(self, spec):
        value, on = self._bit(spec)
        return on, f"{spec['field']}=0b{value:016b}, bit {spec['bit']} is {int(on)}"

    def bit_clear(self, spec):
        value, on = self._bit(spec)
        return not on, f"{spec['field']}=0b{value:016b}, bit {spec['bit']} is {int(on)}"

    def head_bbox_matches_outlines(self, _):
        if "glyf" not in self.font:
            return True, "not a glyf font"
        glyf = self.font["glyf"]
        bounds = None
        for name in self.font.getGlyphOrder():
            g = glyf[name]
            if not g.numberOfContours:
                continue
            g.recalcBounds(glyf)
            box = (g.xMin, g.yMin, g.xMax, g.yMax)
            bounds = box if bounds is None else (
                min(bounds[0], box[0]), min(bounds[1], box[1]),
                max(bounds[2], box[2]), max(bounds[3], box[3]),
            )
        if bounds is None:
            return True, "no outlines"
        head = self.font["head"]
        actual = (head.xMin, head.yMin, head.xMax, head.yMax)
        return actual == bounds, f"head says {actual}, outlines are {bounds}"

    def maxp_covers_outlines(self, _):
        if "glyf" not in self.font or not hasattr(self.font["maxp"], "maxPoints"):
            return True, "not a glyf font with maxp 1.0"
        glyf = self.font["glyf"]
        points = contours = 0
        for name in self.font.getGlyphOrder():
            g = glyf[name]
            if g.numberOfContours > 0:
                p, c = g.getMaxpValues()
                points, contours = max(points, p), max(contours, c)
        maxp = self.font["maxp"]
        ok = maxp.maxPoints >= points and maxp.maxContours >= contours
        return ok, (
            f"maxp says {maxp.maxPoints} points / {maxp.maxContours} contours, "
            f"glyf needs {points} / {contours}"
        )

    def hhea_matches_hmtx(self, _):
        if "hmtx" not in self.font or "hhea" not in self.font:
            return True, "no metrics"
        widths = [m[0] for m in self.font["hmtx"].metrics.values()]
        actual = self.font["hhea"].advanceWidthMax
        return actual == max(widths), f"hhea says {actual}, widest glyph is {max(widths)}"

    # -- names

    def _name(self, name_id):
        return self.font["name"].getDebugName(name_id) if "name" in self.font else None

    def _win_name(self, name_id):
        if "name" not in self.font:
            return None
        record = self.font["name"].getName(name_id, 3, 1, 0x409)
        return record.toUnicode() if record else None

    def name_record(self, spec):
        actual = self._win_name(int(spec["id"]))
        return actual == spec["equals"], f"nameID {spec['id']} is {actual!r}"

    def name_absent(self, spec):
        actual = self._win_name(int(spec["id"]))
        return actual is None, f"nameID {spec['id']} is {actual!r}"

    def name_present(self, spec):
        actual = self._win_name(int(spec["id"]))
        return actual is not None, f"nameID {spec['id']} is {actual!r}"

    def name_ids_subset_of(self, spec):
        allowed = set(int(i) for i in spec["ids"])
        present = {r.nameID for r in self.font["name"].names}
        extra = sorted(present - allowed)
        return not extra, f"unexpected name IDs: {extra}"

    def no_dangling_name_ids(self, _):
        """Every name ID above 255 must still be referenced by fvar or STAT.

        Below 256 the specification reserves the meanings, so those records stand on
        their own. Above it, an ID is only meaningful because something points at it.
        """
        used = set()
        if "fvar" in self.font:
            for axis in self.font["fvar"].axes:
                used.add(axis.axisNameID)
            for instance in self.font["fvar"].instances:
                used.add(instance.subfamilyNameID)
                if getattr(instance, "postscriptNameID", 0xFFFF) != 0xFFFF:
                    used.add(instance.postscriptNameID)
        if "STAT" in self.font:
            stat = self.font["STAT"].table
            if stat.DesignAxisRecord:
                for axis in stat.DesignAxisRecord.Axis:
                    used.add(axis.AxisNameID)
            if stat.AxisValueArray:
                for value in stat.AxisValueArray.AxisValue:
                    used.add(value.ValueNameID)
            fallback = getattr(stat, "ElidedFallbackNameID", None)
            if fallback is not None:
                used.add(fallback)
        # Feature parameters name stylistic sets and character variants.
        for tag in ("GSUB", "GPOS"):
            if tag not in self.font:
                continue
            table = self.font[tag].table
            if not table.FeatureList:
                continue
            for record in table.FeatureList.FeatureRecord:
                params = getattr(record.Feature, "FeatureParams", None)
                for attr in ("UINameID", "FeatUILabelNameID", "FirstParamUILabelNameID"):
                    value = getattr(params, attr, None)
                    if value:
                        used.add(value)
        present = {r.nameID for r in self.font["name"].names if r.nameID > 255}
        dangling = sorted(present - used)
        return not dangling, f"{len(dangling)} unreferenced name IDs above 255: {dangling[:8]}"

    def name_record_at(self, spec):
        record = self.font["name"].getName(
            int(spec["id"]), int(spec["platform"]), int(spec["encoding"]),
            int(spec["language"]),
        )
        actual = record.toUnicode() if record else None
        return actual == spec["equals"], (
            f"nameID {spec['id']} at "
            f"{spec['platform']}/{spec['encoding']}/{spec['language']} is {actual!r}"
        )

    def name_absent_at(self, spec):
        record = self.font["name"].getName(
            int(spec["id"]), int(spec["platform"]), int(spec["encoding"]),
            int(spec["language"]),
        )
        return record is None, f"the record is {'present' if record else 'absent'}"

    def name_record_count(self, spec):
        n = sum(1 for r in self.font["name"].names if r.nameID == int(spec["id"]))
        return n == spec["equals"], f"{n} records with nameID {spec['id']}"

    def name_encoding(self, spec):
        record = self.font["name"].getName(
            int(spec["id"]), int(spec["platform"]), int(spec["encoding"]),
            int(spec["language"]),
        )
        if record is None:
            return False, "no such record"
        actual = record.getEncoding()
        return actual == spec["equals"], f"encoding is {actual!r}"

    def name_records_sorted(self, _):
        """The specification requires the record array in ascending order.

        Sorted by platform, then encoding, then language, then name ID. A font whose
        records are out of order is malformed even though most software copes.
        """
        keys = [
            (r.platformID, r.platEncID, r.langID, r.nameID)
            for r in self.font["name"].names
        ]
        return keys == sorted(keys), (
            "in order" if keys == sorted(keys) else f"first {min(3, len(keys))} out of order"
        )

    def name_references_resolve(self, _):
        """Every name ID something points at must actually exist.

        The mirror image of `no_dangling_name_ids`: that one finds records nothing
        refers to, this one finds references to records that are not there, which is
        what a too-eager pruning pass produces.
        """
        present = {r.nameID for r in self.font["name"].names}
        wanted: set[int] = set()
        if "fvar" in self.font:
            for axis in self.font["fvar"].axes:
                wanted.add(axis.axisNameID)
            for instance in self.font["fvar"].instances:
                wanted.add(instance.subfamilyNameID)
                if getattr(instance, "postscriptNameID", 0xFFFF) != 0xFFFF:
                    wanted.add(instance.postscriptNameID)
        if "STAT" in self.font:
            stat = self.font["STAT"].table
            if stat.DesignAxisRecord:
                for axis in stat.DesignAxisRecord.Axis:
                    wanted.add(axis.AxisNameID)
            if stat.AxisValueArray:
                for value in stat.AxisValueArray.AxisValue:
                    wanted.add(value.ValueNameID)
            fallback = getattr(stat, "ElidedFallbackNameID", None)
            if fallback is not None:
                wanted.add(fallback)
        missing = sorted(wanted - present)
        return not missing, f"references to absent name IDs: {missing}"

    # -- glyf structure

    def _glyf(self):
        return self.font["glyf"] if "glyf" in self.font else None

    def glyph_component_count(self, spec):
        glyf = self._glyf()
        if glyf is None:
            return False, "not a glyf font"
        if spec["glyph"] not in self.font.getGlyphOrder():
            return False, f"no glyph named {spec['glyph']}"
        glyph = glyf[spec["glyph"]]
        n = len(glyph.components) if glyph.isComposite() else 0
        return n == spec["equals"], (
            f"{spec['glyph']} has {n} components"
            + (" (it is a simple glyph)" if n == 0 else "")
        )

    def _overlap_state(self):
        """(simple flags seen, compound flags seen) across the font."""
        glyf = self._glyf()
        simple, compound = [], []
        for name in self.font.getGlyphOrder():
            glyph = glyf[name]
            if glyph.isComposite():
                if glyph.components:
                    compound.append(bool(glyph.components[0].flags & 0x0400))
            elif glyph.numberOfContours > 0 and len(getattr(glyph, "flags", [])):
                simple.append(bool(glyph.flags[0] & 0x40))
        return simple, compound

    def overlap_flags(self, spec):
        if self._glyf() is None:
            return True, "not a glyf font, so there are no such flags"
        simple, compound = self._overlap_state()
        problems = []
        if "simple" in spec and simple and all(simple) != bool(spec["simple"]):
            problems.append(f"OVERLAP_SIMPLE is {simple.count(True)}/{len(simple)}")
        if "compound" in spec and compound and all(compound) != bool(spec["compound"]):
            problems.append(f"OVERLAP_COMPOUND is {compound.count(True)}/{len(compound)}")
        return not problems, "; ".join(problems) or (
            f"{len(simple)} simple, {len(compound)} composite glyphs as expected"
        )

    def overlap_flags_present(self, spec):
        if self._glyf() is None:
            return True, "not a glyf font"
        simple, compound = self._overlap_state()
        seen = simple + compound
        want = bool(spec["equals"])
        actual = all(seen) if want else not any(seen)
        return actual, f"{sum(seen)} of {len(seen)} glyphs carry an overlap flag"

    def composites_preserved(self, spec):
        """A composite in the source must still be a composite, unless excused.

        Decomposing one costs the size saving it existed for, so a tool should only do
        it when it must — which, for overlap removal, is when its components overlap.
        """
        glyf = self._glyf()
        source_glyf = self.source["glyf"] if "glyf" in self.source else None
        if glyf is None or source_glyf is None:
            return True, "not a glyf font"
        excused = set(spec.get("except_glyphs", []))
        decomposed = []
        for name in self.source.getGlyphOrder():
            if name in excused or name not in self.font.getGlyphOrder():
                continue
            if source_glyf[name].isComposite() and not glyf[name].isComposite():
                decomposed.append(name)
        return not decomposed, f"decomposed: {decomposed}" if decomposed else "all kept"

    def glyph_instructions_preserved(self, _):
        glyf = self._glyf()
        source_glyf = self.source["glyf"] if "glyf" in self.source else None
        if glyf is None or source_glyf is None:
            return True, "not a glyf font"
        lost = []
        for name in self.source.getGlyphOrder():
            if name not in self.font.getGlyphOrder():
                continue
            before = getattr(source_glyf[name], "program", None)
            after = getattr(glyf[name], "program", None)
            before_bytes = before.getBytecode() if before else b""
            after_bytes = after.getBytecode() if after else b""
            if before_bytes and before_bytes != after_bytes:
                lost.append(name)
        return not lost, f"instructions changed or dropped on: {lost}" if lost else "kept"

    def maxp_covers_instructions(self, _):
        """`maxp.maxSizeOfInstructions` must be at least the longest glyph program.

        fontTools does not recalculate this field, so it is inherited from the input and
        can end up too small once glyphs change. A rasteriser sizes its instruction
        buffer from it.
        """
        glyf = self._glyf()
        if glyf is None or not hasattr(self.font["maxp"], "maxSizeOfInstructions"):
            return True, "not a glyf font with maxp 1.0"
        longest = 0
        for name in self.font.getGlyphOrder():
            program = getattr(glyf[name], "program", None)
            if program is not None:
                longest = max(longest, len(program.getBytecode()))
        declared = self.font["maxp"].maxSizeOfInstructions
        return declared >= longest, (
            f"maxp says {declared}, the longest program is {longest} bytes"
        )

    def matches_case_output(self, spec):
        """Compare against another case's output, run with the same program."""
        other = self.peers.get(spec["case"])
        if other is None:
            return False, f"the output of {spec['case']} was not available"
        try:
            peer = ttLib.TTFont(other)
        except Exception as e:  # noqa: BLE001
            return False, f"{spec['case']} did not produce a readable font: {e}"

        # A case can narrow the comparison to particular glyphs. Some obligations are
        # about one glyph while its neighbours in the same font are required to change:
        # a clean glyph must survive overlap removal untouched in a font that also holds
        # glyphs the removal is supposed to rewrite.
        only = spec.get("glyphs")
        names = [n for n in peer.getGlyphOrder() if only is None or n in only]
        if only is not None:
            missing = [n for n in only if n not in peer.getGlyphOrder()]
            if missing:
                return False, f"{spec['case']} has no glyph {missing}"

        for what in spec.get("compare", []):
            if what == "tables":
                a, b = sorted(peer.keys()), sorted(self.font.keys())
                if a != b:
                    return False, f"table sets differ: {set(a) ^ set(b)}"
            elif what == "glyph_order":
                if peer.getGlyphOrder() != self.font.getGlyphOrder():
                    return False, "glyph orders differ"
            elif what == "outlines":
                for name in names:
                    a = _round_ops(_draw(peer, name, None))
                    b = _round_ops(_draw(self.font, name, None))
                    if a != b:
                        return False, f"glyph {name} differs from {spec['case']}"
            elif what == "advances":
                for name in names:
                    if peer["hmtx"][name] != self.font["hmtx"][name]:
                        return False, f"metrics for {name} differ from {spec['case']}"
            elif what == "glyph_points":
                # Stronger than comparing drawn outlines: this asks whether the stored
                # points are the same points, in the same order, with the same on/off
                # curve flags. That is the difference between "the glyph still draws the
                # same shape" and "the glyph was not rebuilt" -- a boolean engine that
                # renumbers points or refits a curve through rounded intersections can
                # preserve the first while destroying the second.
                pg, sg = peer.get("glyf"), self.font.get("glyf")
                if pg is None or sg is None:
                    return False, "glyph_points needs a glyf table in both fonts"
                for name in names:
                    a, b = pg[name], sg[name]
                    if a.numberOfContours != b.numberOfContours:
                        return False, (
                            f"glyph {name}: {a.numberOfContours} contours in "
                            f"{spec['case']}, {b.numberOfContours} here"
                        )
                    if a.isComposite():
                        av = [(c.glyphName, c.x, c.y) for c in a.components]
                        bv = [(c.glyphName, c.x, c.y) for c in b.components]
                        if av != bv:
                            return False, f"glyph {name}: components differ"
                        continue
                    if list(a.endPtsOfContours) != list(b.endPtsOfContours):
                        return False, f"glyph {name}: contour boundaries differ"
                    if list(a.coordinates) != list(b.coordinates):
                        return False, (
                            f"glyph {name}: {len(a.coordinates)} points in "
                            f"{spec['case']}, {len(b.coordinates)} here, and they differ"
                        )
                    # Bit 0 is on-curve; bit 6 is OVERLAP_SIMPLE, which overlap removal
                    # is separately required to clear, so comparing it would contradict
                    # `overlap.no-overlap-flags-after-removal`.
                    if [f & 0x01 for f in a.flags] != [f & 0x01 for f in b.flags]:
                        return False, f"glyph {name}: on-curve flags differ"
            elif what == "instructions":
                pg, sg = peer.get("glyf"), self.font.get("glyf")
                if pg is None or sg is None:
                    continue
                for name in names:
                    a = getattr(pg[name], "program", None)
                    b = getattr(sg[name], "program", None)
                    if (a.getBytecode() if a else b"") != (b.getBytecode() if b else b""):
                        return False, f"instructions for {name} differ"
        return True, f"matches {spec['case']}"

    # -- outlines

    def _location_for(self, font, location):
        """Only axes the font actually has, so a pinned axis in the case is ignored."""
        if "fvar" not in font:
            return {}
        tags = {a.axisTag for a in font["fvar"].axes}
        return {t: v for t, v in (location or {}).items() if t in tags}

    def outlines_match_source_at(self, spec):
        tolerance = float(spec.get("tolerance", 1.0))
        location = spec.get("location", {})
        # Instance the reference at exactly the axes the output no longer has, and draw
        # both sides at whatever is left. Those are the axes the job pinned, so this
        # subjects the reference to the same rounding the output went through: a pinned
        # coordinate is stored as an integer in `glyf`, and comparing it against a
        # float interpolation measures the rounding rather than the instancing. Axes that
        # survived in the output are *not* pre-instanced, because there both sides are
        # meant to interpolate and pinning one of them would compare different things.
        wanted = self._location_for(self.source, location)
        surviving = (
            {a.axisTag for a in self.font["fvar"].axes} if "fvar" in self.font else set()
        )
        reference_font = self._source_instanced_at(
            {t: v for t, v in wanted.items() if t not in surviving}
        )
        source_loc = self._location_for(reference_font, location)
        output_loc = self._location_for(self.font, location)
        worst = 0.0
        for name in reference_font.getGlyphOrder():
            if name not in self.font.getGlyphOrder():
                return False, f"glyph {name} is missing from the output"
            reference = _round_ops(_draw(reference_font, name, source_loc))
            actual = _round_ops(_draw(self.font, name, output_loc))
            problem = _compare_outlines(reference, actual, tolerance)
            if problem:
                return False, f"glyph {name}: {problem}"
        return True, f"all glyphs agree within {tolerance} units (worst {worst})"

    def outlines_match_source_across(self, spec):
        tolerance = float(spec.get("tolerance", 1.0))
        for location in spec["locations"]:
            sub = dict(spec)
            sub["location"] = location
            sub["tolerance"] = tolerance
            ok, detail = self.outlines_match_source_at(sub)
            if not ok:
                return False, f"at {location}: {detail}"
        return True, f"{len(spec['locations'])} locations agree"

    def _hmtx_at(self, font: ttLib.TTFont, location: dict) -> dict:
        """Advance widths as they would be at `location`, after instancing and compiling.

        Both sides go through this. An output that is still variable holds the *default*
        master's advances in its `hmtx`, and the variation lives in `HVAR` or in `gvar`'s
        phantom points; reading `hmtx` straight out of it would compare a default width
        against a reference pinned somewhere else and call the difference a defect.
        """
        from fontTools.varLib.instancer import instantiateVariableFont

        wanted = self._location_for(font, location)
        if wanted and "fvar" in font:
            instantiateVariableFont(font, wanted, inplace=True)
            buffer = io.BytesIO()
            font.save(buffer)
            buffer.seek(0)
            font = ttLib.TTFont(buffer)
        return {name: font["hmtx"][name][0] for name in font.getGlyphOrder()}

    def advances_match_source_at(self, spec):
        tolerance = float(spec.get("tolerance", 1.0))
        location = spec.get("location", {})
        reference = self._hmtx_at(ttLib.TTFont(self.source_path), location)
        actual = self._hmtx_at(ttLib.TTFont(self.output_path), location)
        for name, want in reference.items():
            if name not in actual:
                return False, f"glyph {name} missing"
            if abs(want - actual[name]) > tolerance:
                return False, f"glyph {name}: advance {want} vs {actual[name]}"
        return True, f"{len(reference)} advances agree"

    def advances_match_source_across(self, spec):
        """The same, at several locations.

        The point of sampling more than one is that a font whose advance variation has
        been dropped still has the right widths at whichever location happens to be its
        default. Only an interior or far-end sample can tell "the widths still vary" from
        "the widths are frozen at the master they were built from".
        """
        tolerance = float(spec.get("tolerance", 1.0))
        for location in spec["locations"]:
            sub = {"location": location, "tolerance": tolerance}
            ok, detail = self.advances_match_source_at(sub)
            if not ok:
                return False, f"at {location}: {detail}"
        return True, f"{len(spec['locations'])} locations agree"

    def _pair_kerning_at(self, font: ttLib.TTFont, location: dict) -> dict:
        """Every GPOS pair adjustment, as it would be at `location`.

        Instanced and compiled first, for the same reason the advance check does it: a
        still-variable font holds the default master's values, and the variation lives in
        the GDEF item variation store that GPOS reaches into by delta-set index.
        """
        from fontTools.varLib.instancer import instantiateVariableFont

        wanted = self._location_for(font, location)
        if wanted and "fvar" in font:
            instantiateVariableFont(font, wanted, inplace=True)
            buffer = io.BytesIO()
            font.save(buffer)
            buffer.seek(0)
            font = ttLib.TTFont(buffer)

        pairs: dict = {}
        if "GPOS" not in font:
            return pairs
        for lookup in font["GPOS"].table.LookupList.Lookup:
            for subtable in lookup.SubTable:
                if not hasattr(subtable, "PairSet"):
                    continue
                for index, pair_set in enumerate(subtable.PairSet):
                    first = subtable.Coverage.glyphs[index]
                    for record in pair_set.PairValueRecord:
                        pairs[(first, record.SecondGlyph)] = getattr(
                            record.Value1, "XAdvance", 0
                        )
        return pairs

    def _metrics_at(self, font: ttLib.TTFont, location: dict) -> dict:
        """The font-wide vertical metrics as they would be at `location`."""
        from fontTools.varLib.instancer import instantiateVariableFont

        wanted = self._location_for(font, location)
        if wanted and "fvar" in font:
            instantiateVariableFont(font, wanted, inplace=True)
            buffer = io.BytesIO()
            font.save(buffer)
            buffer.seek(0)
            font = ttLib.TTFont(buffer)

        out = {}
        if "OS/2" in font:
            os2 = font["OS/2"]
            for field in ("sTypoAscender", "sTypoDescender", "sTypoLineGap",
                          "sxHeight", "sCapHeight"):
                if hasattr(os2, field):
                    out[field] = getattr(os2, field)
        if "post" in font:
            out["underlinePosition"] = font["post"].underlinePosition
            out["underlineThickness"] = font["post"].underlineThickness
        return out

    def metrics_match_source_across(self, spec):
        """The font-wide metrics must still vary over a narrowed axis.

        `MVAR` is what makes ascender, x-height, cap height and the underline move with
        weight or optical size. An implementation that applies it at the new default and
        then drops the table leaves metrics that are right at that one location and frozen
        everywhere else -- a font narrowed to wght 300:700 keeping its 300 x-height at 700.
        Only an interior or far-end sample can see that.
        """
        tolerance = float(spec.get("tolerance", 0))
        checked = 0
        for location in spec["locations"]:
            want = self._metrics_at(ttLib.TTFont(self.source_path), location)
            got = self._metrics_at(ttLib.TTFont(self.output_path), location)
            for field, value in want.items():
                if field not in got:
                    return False, f"at {location}: {field} is missing from the output"
                if abs(value - got[field]) > tolerance:
                    return False, (
                        f"at {location}: {field} is {value} in the source, "
                        f"{got[field]} here"
                    )
                checked += 1
        return True, f"{checked} metric readings agree"

    def kerning_matches_source_across(self, spec):
        """Variable kerning must still interpolate over a narrowed axis.

        The failure this exists to catch is not a crash. An implementation that narrows an
        axis without re-tenting the GDEF item variation store leaves kerning that is right
        at the default location and wrong everywhere else, which no check that samples one
        location can see -- and which no outline check can see at all.
        """
        tolerance = float(spec.get("tolerance", 0))
        for location in spec["locations"]:
            want = self._pair_kerning_at(ttLib.TTFont(self.source_path), location)
            got = self._pair_kerning_at(ttLib.TTFont(self.output_path), location)
            if not want:
                return False, f"the source has no pair kerning at {location}"
            for pair, value in want.items():
                if pair not in got:
                    return False, f"at {location}: the pair {pair} is missing"
                if abs(value - got[pair]) > tolerance:
                    return False, (
                        f"at {location}: {pair[0]}/{pair[1]} kerns {value} in the source, "
                        f"{got[pair]} here"
                    )
        return True, (
            f"{len(spec['locations'])} locations agree across "
            f"{len(want)} pairs"
        )

    def filled_region_matches(self, spec):
        """The set of points inside each glyph must be unchanged.

        This is the check for overlap removal, where the outline is meant to change and
        comparing outlines would therefore be meaningless. Points within
        `tolerance_units` of either outline are skipped, because a refitted curve can
        move an edge by a fraction of a unit and the winding right on an edge is not
        defined anyway.

        The reference location is `reference.source_at`, which is what every case in the
        corpus writes and what the README documents. An earlier version read
        `reference.location`, found nothing, and drew the source at its *default*. That
        was invisible for four of the five cases -- they all pin `wght` at 400, which is
        the `overlapping` fixture's default -- and wrong for the fifth, `cff2-vf` at
        wght=700, where it compared a merged bold outline against an unmerged regular
        one and reported 2019 sampled points as having changed.  `location` is still
        accepted as a spelling so that a case written either way means the same thing.
        """
        margin = float(spec.get("tolerance_units", 1.5))
        reference_spec = spec.get("reference", {})
        location = reference_spec.get("source_at") or reference_spec.get("location", {})
        source_loc = self._location_for(self.source, location)

        mismatches = 0
        checked = 0
        for name in self.source.getGlyphOrder():
            if name not in self.font.getGlyphOrder():
                return False, f"glyph {name} missing"
            before = _flatten(_draw(self.source, name, source_loc))
            after = _flatten(_draw(self.font, name, None))
            if not before and not after:
                continue
            xs = [p[0] for c in before + after for p in c]
            ys = [p[1] for c in before + after for p in c]
            if not xs:
                continue
            # Strides that do not divide each other, so the grid never lines up with a
            # diagonal edge and reports a whole row of undefined windings as failures.
            for i in range(41):
                for j in range(37):
                    x = min(xs) - 4 + (max(xs) - min(xs) + 8) * (i / 40)
                    y = min(ys) - 4 + (max(ys) - min(ys) + 8) * (j / 36)
                    inside_before = _winding(before, x, y) != 0
                    inside_after = _winding(after, x, y) != 0
                    if inside_before == inside_after:
                        checked += 1
                        continue
                    if _near_edge(before, x, y, margin) or _near_edge(after, x, y, margin):
                        continue
                    mismatches += 1
                    if mismatches < 4:
                        detail = f"glyph {name} at ({x:.1f}, {y:.1f})"
            checked += 1
        if mismatches:
            return False, f"{mismatches} sampled points changed fill state ({detail})"
        return True, f"{checked} sampled points agree"

    def no_self_intersections(self, _):
        for name in self.font.getGlyphOrder():
            contours = _flatten(_draw(self.font, name, None), steps=8)
            edges = []
            for ci, contour in enumerate(contours):
                for i in range(len(contour)):
                    edges.append((ci, contour[i], contour[(i + 1) % len(contour)]))
            for a in range(len(edges)):
                for b in range(a + 1, len(edges)):
                    ca, a0, a1 = edges[a]
                    cb, b0, b1 = edges[b]
                    if ca == cb and (b == a + 1 or (a == 0 and b == len(edges) - 1)):
                        continue
                    if a0 in (b0, b1) or a1 in (b0, b1):
                        continue
                    if _segments_cross(a0, a1, b0, b1):
                        return False, f"glyph {name}: contours {ca} and {cb} cross"
        return True, "no crossings found"

    def all_coordinates_finite(self, _):
        for name in self.font.getGlyphOrder():
            for op, args in _draw(self.font, name, None):
                for point in args:
                    if point is None:
                        continue
                    for c in point:
                        if not math.isfinite(c):
                            return False, f"glyph {name}: {op} has {c}"
        return True, "all coordinates finite"

    def contour_count(self, spec):
        contours = _flatten(_draw(self.font, spec["glyph"], None))
        return len(contours) == spec["equals"], f"{len(contours)} contours"

    # -- layout

    def _layout(self, tag):
        return self.font[tag].table if tag in self.font else None

    def feature_lookup_count(self, spec):
        table = self._layout(spec["table"])
        if table is None or not table.FeatureList:
            return False, f"no {spec['table']} feature list"
        for record in table.FeatureList.FeatureRecord:
            if record.FeatureTag == spec["feature"]:
                n = len(record.Feature.LookupListIndex)
                if "equals" in spec:
                    return n == spec["equals"], f"{spec['feature']} runs {n} lookups"
                return n >= spec["min"], f"{spec['feature']} runs {n} lookups"
        # A feature that is not in the list runs no lookups, so a case asking for zero is
        # satisfied by its absence. Requiring the record to be present *and* empty would
        # be a stricter claim than any of these cases makes, and a wrong one: fontTools
        # deletes a feature record whose lookups have all been resolved away, so demanding
        # an empty record would fail the reference implementation.
        wanted = spec.get("equals", spec.get("min"))
        if wanted == 0:
            return True, f"no {spec['feature']} feature, so it runs no lookups"
        return False, f"no {spec['feature']} feature"

    def no_feature_variations(self, spec):
        table = self._layout(spec["table"])
        if table is None:
            return True, f"no {spec['table']}"
        has = getattr(table, "FeatureVariations", None) is not None
        return not has, f"FeatureVariations present={has}"

    def feature_variation_axes_valid(self, spec):
        table = self._layout(spec["table"])
        if table is None:
            return True, f"no {spec['table']}"
        variations = getattr(table, "FeatureVariations", None)
        if variations is None:
            return True, "no FeatureVariations"
        axis_count = len(self.font["fvar"].axes) if "fvar" in self.font else 0
        for record in variations.FeatureVariationRecord:
            if not record.ConditionSet:
                continue
            for condition in record.ConditionSet.ConditionTable:
                if getattr(condition, "Format", 1) != 1:
                    continue
                if condition.AxisIndex >= axis_count:
                    return False, (
                        f"a condition names axis index {condition.AxisIndex}, "
                        f"but the font has {axis_count} axes"
                    )
        return True, f"all conditions within {axis_count} axes"

    def substitutes(self, spec):
        table = self._layout(spec["table"])
        if table is None or not table.FeatureList:
            return False, f"no {spec['table']}"
        for record in table.FeatureList.FeatureRecord:
            if record.FeatureTag != spec["feature"]:
                continue
            for index in record.Feature.LookupListIndex:
                lookup = table.LookupList.Lookup[index]
                for sub in lookup.SubTable:
                    mapping = getattr(sub, "mapping", None)
                    if mapping and mapping.get(spec["from"]) == spec["to"]:
                        return True, f"{spec['from']} -> {spec['to']}"
        return False, f"{spec['feature']} does not map {spec['from']} to {spec['to']}"


def _near_edge(contours, x, y, margin) -> bool:
    m2 = margin * margin
    for contour in contours:
        n = len(contour)
        for i in range(n):
            x0, y0 = contour[i]
            x1, y1 = contour[(i + 1) % n]
            dx, dy = x1 - x0, y1 - y0
            length2 = dx * dx + dy * dy
            if length2 == 0:
                if (x - x0) ** 2 + (y - y0) ** 2 <= m2:
                    return True
                continue
            t = max(0.0, min(1.0, ((x - x0) * dx + (y - y0) * dy) / length2))
            px, py = x0 + t * dx, y0 + t * dy
            if (x - px) ** 2 + (y - py) ** 2 <= m2:
                return True
    return False


# --------------------------------------------------------------------------- driver

def _evaluate_editor(case: dict, outcome: dict) -> CaseResult:
    """Score a `load_only` case, whose expectations are about the editor, not a font."""
    rows = outcome.get("editor", {}).get("axes", [])
    results: list[CheckResult] = []
    for spec in case.get("expect", {}).get("checks", []):
        if spec.get("kind") != "editor_axis_rows":
            results.append(CheckResult(spec.get("kind"), False,
                                       "only editor_axis_rows applies to a load_only case"))
            continue
        want = spec["equals"]
        if len(rows) != len(want):
            results.append(CheckResult("editor_axis_rows", False,
                                       f"{len(rows)} rows, expected {len(want)}"))
            continue
        problem = None
        for index, (expected, actual) in enumerate(zip(want, rows)):
            for key, value in expected.items():
                got = actual.get(key)
                if isinstance(value, (int, float)) and isinstance(got, (int, float)):
                    same = math.isclose(float(got), float(value), abs_tol=1e-6)
                else:
                    same = got == value
                if not same:
                    problem = f"row {index} {key}: {got!r}, expected {value!r}"
                    break
            if problem:
                break
        results.append(CheckResult("editor_axis_rows", problem is None, problem or "rows match"))
    failed = [r for r in results if not r.passed]
    return CaseResult(case["id"], not failed,
                      "" if not failed else f"{len(failed)} of {len(results)} checks failed",
                      results)


def evaluate(case: dict, outcome: dict, source_path: str, peers: dict | None = None) -> CaseResult:
    """Score one case against what a runner produced."""
    expect = case.get("expect", {})
    want = expect.get("outcome", "success")
    case_id = case["id"]

    if want == "error":
        if outcome.get("ok"):
            return CaseResult(case_id, False, "expected a refusal, but the tool succeeded")
        message = (outcome.get("error") or "").lower()
        missing = [s for s in expect.get("message_contains", []) if s.lower() not in message]
        if missing:
            return CaseResult(
                case_id, False,
                f"the refusal did not mention {missing}; it said: {outcome.get('error')!r}",
            )
        return CaseResult(case_id, True, "refused, with a message naming the problem")

    if not outcome.get("ok"):
        return CaseResult(case_id, False, f"the tool refused: {outcome.get('error')}")

    if "editor" in outcome:
        return _evaluate_editor(case, outcome)

    try:
        checker = Checker(outcome["path"], source_path, peers)
    except Exception as e:  # noqa: BLE001
        return CaseResult(case_id, False, f"the output could not be opened as a font: {e}")

    results: list[CheckResult] = []
    for spec in expect.get("checks", []):
        kind = spec.get("kind")
        method = getattr(checker, kind, None)
        if method is None or kind.startswith("_"):
            results.append(CheckResult(kind, False, "unknown check kind"))
            continue
        try:
            ok, detail = method(spec)
        except Exception as e:  # noqa: BLE001
            ok, detail = False, f"the check raised {type(e).__name__}: {e}"
        if spec.get("not"):
            ok = not ok
            detail = f"(inverted) {detail}"
        results.append(CheckResult(kind, ok, detail))

    failed = [r for r in results if not r.passed]
    return CaseResult(
        case_id,
        not failed,
        "" if not failed else f"{len(failed)} of {len(results)} checks failed",
        results,
    )
