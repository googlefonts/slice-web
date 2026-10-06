//! Merging overlapping contours.
//!
//! This is the thing the original Slice never did, and the reason it matters is not
//! rendering: browsers and rasterisers have filled non-zero winding correctly for
//! decades. It matters because the fonts come out the other end and go *into* design
//! applications, and support for overlapping contours there is still patchy a decade on.
//! A sliced instance whose stems overlap will show seams when outlined, misbehave under
//! boolean operations, and export badly to formats that assume simple contours.
//!
//! The approach follows `fontTools.ttLib.removeOverlaps`: take a glyph's contours,
//! union them, and write the result back. fontTools delegates the union to Skia's path
//! ops via `skia-pathops`, which has no WebAssembly build; here it is `linesweeper`,
//! a robust Bentley--Ottmann sweep line that does the same job on Bézier paths in
//! pure Rust.
//!
//! Both outline formats are handled, and the difference between them is in the last step
//! rather than the union: the contours come out of skrifa, which draws `glyf` and `CFF2`
//! alike, and go back either as a simple glyph or as a charstring.
//!
//! Three things about this are worth knowing:
//!
//! * TrueType outlines are quadratic, and boolean path arithmetic works in cubics.
//!   Quadratic to cubic is exact; the way back is an approximation, so a glyph that is
//!   modified comes back with slightly different curves and more points. Glyphs that do
//!   not need modifying are therefore left completely alone. CFF stores cubics, so there
//!   is no refit on that path and the merged outline goes back exactly as it came out.
//! * Removing overlaps invalidates hinting, because the point numbers it refers to no
//!   longer mean anything. fontTools drops hinting from every glyph when this runs, on
//!   the grounds that a half-hinted font looks worse than an unhinted one, and this does
//!   the same. For CFF the hints are inside the charstring and go with the redraw; the
//!   Private DICT's alignment zones and stem widths survive, because they describe the
//!   design rather than individual points.
//! * The two formats wind their contours opposite ways -- TrueType runs outer contours
//!   clockwise, PostScript counter-clockwise -- so the re-winding pass takes which one it
//!   is aiming for. The fill is the same either way; the convention is not.

use kurbo::{BezPath, CubicBez, ParamCurve, PathEl, Point, Shape};
use read_fonts::tables::glyf::CurvePoint;
use read_fonts::{FontRef, TableProvider};
use write_fonts::tables::glyf::{Contour, GlyfLocaBuilder, Glyph as WGlyph, SimpleGlyph};
use write_fonts::types::{GlyphId, Tag};
use write_fonts::{from_obj::ToOwnedTable, FontBuilder};

use linesweeper::topology::ContourIdx;
use linesweeper::{binary_op, BinaryOp, FillRule};

use crate::SliceError;

/// How closely the cubic result must be refitted with quadratics, in font units.
const QUAD_ACCURACY: f64 = 0.05;

/// An area too small to be ink, in square font units.
///
/// It decides whether a merge changed a glyph at all, and whether a contour the merge
/// produced encloses anything. The margin either side of it is wide. On Google Sans Flex
/// Condensed Black, merging a glyph that has no overlaps moves its area by at most
/// 6.3e-8; merging one that has them moves it by at least 60.
const NEGLIGIBLE_AREA: f64 = 1.0;

/// What happened to a font when overlaps were removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverlapReport {
    /// Glyphs whose contours were actually merged.
    pub modified: Vec<u16>,
    /// Glyphs that were examined and found not to need it.
    pub untouched: usize,
    /// Glyphs the boolean operation could not handle, left as they were.
    pub failed: Vec<(u16, String)>,
}

impl OverlapReport {
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} glyph{} simplified, {} left as they were",
            self.modified.len(),
            if self.modified.len() == 1 { "" } else { "s" },
            self.untouched
        );
        if !self.failed.is_empty() {
            text.push_str(&format!(", {} could not be processed", self.failed.len()));
        }
        text
    }
}

/// Merge overlapping contours throughout a font.
///
/// The font must already be static: overlap removal rewrites outlines, and a `gvar`
/// table's deltas are indexed by point number, so they would no longer line up.
pub fn remove_overlaps(font_bytes: &[u8]) -> Result<(Vec<u8>, OverlapReport), SliceError> {
    let font = FontRef::new(font_bytes).map_err(|e| SliceError::Read(e.to_string()))?;

    if font.glyf().is_err() {
        if font.cff2().is_ok() {
            return remove_overlaps_cff2(font_bytes);
        }
        return Err(SliceError::Unsupported(
            "Overlap removal handles 'glyf' and 'CFF2' outlines; this font has CFF 1.0 \
             outlines, which this build does not write."
                .into(),
        ));
    }
    if font.gvar().is_ok() {
        return Err(SliceError::Unsupported(
            "Overlap removal needs a static font: gvar deltas are indexed by point \
             number, and merging contours renumbers the points. Pin every axis, or turn \
             overlap removal off."
                .into(),
        ));
    }

    let num_glyphs = font.maxp()?.num_glyphs();
    let loca = font.loca(None)?;
    let glyf = font.glyf()?;

    let mut report = OverlapReport::default();
    let mut glyphs: Vec<WGlyph> = Vec::with_capacity(num_glyphs as usize);

    for gid in 0..num_glyphs {
        let gid_value = gid;
        let gid = GlyphId::new(gid as u32);
        let original = read_original(&loca, &glyf, gid);

        match simplify_glyph(&font, gid) {
            Ok(Some(glyph)) => {
                report.modified.push(gid_value);
                glyphs.push(glyph);
            }
            Ok(None) => {
                report.untouched += 1;
                glyphs.push(original);
            }
            Err(e) => {
                report.failed.push((gid_value, e.to_string()));
                glyphs.push(original);
            }
        }
    }

    // Hinting no longer describes these outlines. TrueType instructions address points
    // by index -- SRP0, MDAP, IUP all take point numbers -- and merging contours
    // renumbers them, so a surviving program would move the wrong points. Drop it from
    // every glyph, not just the modified ones, so the font is consistently unhinted
    // rather than partly so.
    //
    // The overlap flags go with them. OVERLAP_SIMPLE and OVERLAP_COMPOUND tell the
    // rasterizer "this glyph self-overlaps, composite it before filling"; leaving them
    // set on outlines that no longer overlap is a false statement about the font, and it
    // gives away the rendering speed the removal was for.
    for glyph in &mut glyphs {
        match glyph {
            WGlyph::Simple(simple) => {
                simple.instructions.clear();
                simple.overlaps = false;
            }
            WGlyph::Composite(composite) => {
                composite.set_instructions(&[]);
                for component in composite.components_mut() {
                    component.flags.overlap_compound = false;
                }
            }
            WGlyph::Empty => {}
        }
    }

    let mut builder = GlyfLocaBuilder::new();
    for glyph in &glyphs {
        builder
            .add_glyph(glyph)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }
    let (new_glyf, new_loca, loca_format) = builder.build();

    let mut out = FontBuilder::new();
    out.add_table(&new_glyf)
        .map_err(|e| SliceError::Write(e.to_string()))?;
    out.add_table(&new_loca)
        .map_err(|e| SliceError::Write(e.to_string()))?;
    let mut head: write_fonts::tables::head::Head = font.head()?.to_owned_table();
    head.index_to_loc_format = loca_format as i16;
    out.add_table(&head)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    // The hinting programs go the same way as the per-glyph instructions, and for the
    // same reason: `prep` and `fpgm` are written against a point numbering that no
    // longer exists, and `cvt ` holds the control values they read. Keeping any of them
    // would leave a font that hints itself into the wrong shape at small sizes.
    let hinting = [Tag::new(b"prep"), Tag::new(b"fpgm"), Tag::new(b"cvt ")];

    // maxp must stop advertising instruction space the font no longer has.
    let mut maxp: write_fonts::tables::maxp::Maxp = font.maxp()?.to_owned_table();
    maxp.max_size_of_instructions = Some(0);
    maxp.max_zones = Some(0);
    maxp.max_twilight_points = Some(0);
    maxp.max_storage = Some(0);
    maxp.max_function_defs = Some(0);
    maxp.max_instruction_defs = Some(0);
    maxp.max_stack_elements = Some(0);
    out.add_table(&maxp)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    crate::instancer::statics::copy_remaining_tables(&mut out, &font, &hinting);
    Ok((out.build(), report))
}

/// Merge overlapping contours in a CFF2 font.
///
/// This is where CFF gains most: the Type 2 charstring specification says outright that
/// overlapping subpaths are not permitted, so an instance with them is not merely
/// awkward downstream, it is outside what the format allows. fontTools takes the same
/// view and removes overlaps as part of any CFF2-to-CFF downgrade, "as CFF does not
/// support overlaps but CFF2 does".
///
/// Unlike the `glyf` path there is no refit: the boolean arithmetic works in cubics and
/// CFF stores cubics, so a merged outline goes back exactly as it came out, with
/// fractional coordinates preserved through the charstring's 16.16 form. What is lost is
/// the hinting, which lives in the charstrings themselves and cannot survive a redrawn
/// outline. The Private DICT's alignment zones and stem widths do survive, since they
/// describe the design rather than individual points.
fn remove_overlaps_cff2(font_bytes: &[u8]) -> Result<(Vec<u8>, OverlapReport), SliceError> {
    use crate::instancer::cff2::table;

    let font = FontRef::new(font_bytes).map_err(|e| SliceError::Read(e.to_string()))?;
    let source = table::read(&font)?;
    if source.var_store.is_some() {
        return Err(SliceError::Unsupported(
            "Overlap removal needs a static font: a CFF2 charstring's blends are stated \
             against a variation store, and merging contours discards the operators that \
             carry them. Pin every axis, or turn overlap removal off."
                .into(),
        ));
    }

    let mut report = OverlapReport::default();
    let mut charstrings: Vec<Vec<u8>> = Vec::with_capacity(source.charstrings.len());

    for (index, original) in source.charstrings.iter().enumerate() {
        let gid = GlyphId::new(index as u32);
        // PostScript's convention is the opposite of TrueType's: outer contours run
        // counter-clockwise.
        match merged_contours(&font, gid, false) {
            Ok(Some(paths)) => {
                report.modified.push(index as u16);
                charstrings.push(paths_to_charstring(&paths));
            }
            Ok(None) => {
                report.untouched += 1;
                charstrings.push(original.to_vec());
            }
            Err(e) => {
                report.failed.push((index as u16, e.to_string()));
                charstrings.push(original.to_vec());
            }
        }
    }

    let builder = table::Cff2Builder {
        top_dict_extra: table::top_dict_extra(&source.top_dict),
        global_subrs: source.global_subrs.iter().map(|s| s.to_vec()).collect(),
        charstrings,
        var_store: None,
        fd_select: source.fd_select.map(<[u8]>::to_vec),
        font_dicts: source
            .font_dicts
            .iter()
            .map(|fd| table::FontDictBuilder {
                other_entries: fd.other_entries.iter().map(|e| e.raw.clone()).collect(),
                private: table::private_dict_without_subrs(&fd.private),
                local_subrs: fd.local_subrs.iter().map(|s| s.to_vec()).collect(),
            })
            .collect(),
    };

    let mut out = FontBuilder::new();
    out.add_raw(table::CFF2_TAG, builder.build()?);
    crate::instancer::statics::copy_remaining_tables(&mut out, &font, &[]);
    Ok((out.build(), report))
}

/// Write merged contours back out as a CFF2 charstring.
///
/// Only the three operators a redrawn outline needs: `rmoveto`, `rlineto` and
/// `rrcurveto`. CFF2 charstrings carry no width prefix and no `endchar`, so the program
/// is exactly the path and then stops.
fn paths_to_charstring(paths: &[BezPath]) -> Vec<u8> {
    use crate::instancer::cff2::num::write_charstring_number;

    const RMOVETO: u8 = 21;
    const RLINETO: u8 = 5;
    const RRCURVETO: u8 = 8;

    let mut out = Vec::new();
    // Every coordinate in a charstring is relative to the point before it, and a
    // charstring starts at the origin. Tracking the exact position rather than a rounded
    // one keeps the error from accumulating along a contour. Note that the point an
    // `rmoveto` is relative to is the last point *drawn*, not the start of the contour
    // just closed -- `.notdef` in the `cff2-vf` fixture is built that way.
    let mut current = Point::ZERO;

    for path in paths {
        let elements = trim_redundant_close(path);
        for element in &elements {
            let (points, op) = match *element {
                PathEl::MoveTo(p) => (vec![p], RMOVETO),
                PathEl::LineTo(p) => (vec![p], RLINETO),
                // CFF has no quadratic operator. Raising a quadratic to a cubic is
                // exact, so nothing is lost on the way.
                PathEl::QuadTo(q, p) => {
                    let cubic = kurbo::QuadBez::new(current, q, p).raise();
                    (vec![cubic.p1, cubic.p2, cubic.p3], RRCURVETO)
                }
                PathEl::CurveTo(c1, c2, p) => (vec![c1, c2, p], RRCURVETO),
                // A CFF subpath closes implicitly at the next `rmoveto` or at the end of
                // the program, so there is nothing to write and the current point does
                // not move.
                PathEl::ClosePath => continue,
            };
            for point in points {
                write_charstring_number(point.x - current.x, &mut out);
                write_charstring_number(point.y - current.y, &mut out);
                current = point;
            }
            out.push(op);
        }
    }
    out
}

/// Drop a final straight segment that only returns to the contour's start.
///
/// CFF closes a subpath with exactly that line, so writing it as well leaves a
/// zero-length segment behind at the join.
fn trim_redundant_close(path: &BezPath) -> Vec<PathEl> {
    let mut elements: Vec<PathEl> = path.elements().to_vec();
    while elements.len() > 2 {
        let start = match elements.first() {
            Some(PathEl::MoveTo(p)) => *p,
            _ => break,
        };
        let last = elements[elements.len() - 1];
        let redundant = match last {
            PathEl::ClosePath => true,
            PathEl::LineTo(p) => (p - start).hypot() < 1e-9,
            _ => false,
        };
        if !redundant {
            break;
        }
        elements.pop();
    }
    elements
}

fn read_original(
    loca: &read_fonts::tables::loca::Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    gid: GlyphId,
) -> WGlyph {
    match loca.get_glyf(gid, glyf) {
        Ok(Some(read_fonts::tables::glyf::Glyph::Simple(simple))) => {
            WGlyph::Simple(simple.to_owned_table())
        }
        Ok(Some(read_fonts::tables::glyf::Glyph::Composite(composite))) => {
            WGlyph::Composite(composite.to_owned_table())
        }
        _ => WGlyph::Empty,
    }
}

/// Merge one glyph's contours, or report that it did not need it.
///
/// Format-agnostic: the contours come out of skrifa, which draws `glyf` and `CFF2`
/// alike, and go back as cubic Bézier paths that either outline format can hold.
/// `outer_clockwise` is the winding convention the caller's format wants.
fn merged_contours(
    font: &FontRef,
    gid: GlyphId,
    outer_clockwise: bool,
) -> Result<Option<Vec<BezPath>>, SliceError> {
    let contours = glyph_contours(font, gid)?;
    // The merge reports the glyph it failed on through the caller, which knows it.
    merge(&contours, outer_clockwise).map_err(|e| match e {
        SliceError::RemoveOverlaps { reason, .. } => SliceError::RemoveOverlaps {
            glyph: format!("{}", gid.to_u32()),
            reason,
        },
        other => other,
    })
}

/// Merge contours into the outline of the region they fill, or report that they did not
/// need it.
///
/// Every glyph goes through the sweep line; nothing tries to rule a glyph out more
/// cheaply first. Something used to, and it failed exactly where it mattered. It
/// compared segments' bounding boxes by the *area* they shared, and the box of a
/// horizontal or vertical line has no area, so a stem edge could never be seen to cross
/// anything. A single contour whose shoulder tucks into its stem -- `n`, `m`, `u`, `r`,
/// `e` at heavy weights -- was passed over as having nothing to merge, and 37 glyphs of
/// Google Sans Flex Condensed Black came out with their overlaps intact. A screen that
/// gets this right is not cheap either: adjacent segments can overlap past the point they
/// share, and a single cubic can loop over itself. The sweep is cheap enough without
/// one: all 682 glyphs of that font take about 0.2 s in a native release build
/// (`probe_area_change` below times it).
fn merge(contours: &[BezPath], outer_clockwise: bool) -> Result<Option<Vec<BezPath>>, SliceError> {
    if contours.is_empty() {
        return Ok(None);
    }
    let merged_paths = union_nonzero(contours, outer_clockwise)?;

    if merged_paths.is_empty() {
        // Contours that enclose nothing -- a stray two-point line -- merge to nothing,
        // and the outline was never going to be drawn as anything else.
        if total_area(contours) < NEGLIGIBLE_AREA {
            return Ok(None);
        }
        return Err(SliceError::RemoveOverlaps {
            glyph: String::from("(unknown)"),
            reason: "the merge produced no contours".into(),
        });
    }

    // If the merge changed nothing meaningful, keep the original outline rather than
    // paying for a refit that would only add points.
    if same_area(contours, &merged_paths) && merged_paths.len() == contours.len() {
        return Ok(None);
    }
    Ok(Some(merged_paths))
}

/// Simplify one `glyf` glyph, or report that it did not need it.
fn simplify_glyph(font: &FontRef, gid: GlyphId) -> Result<Option<WGlyph>, SliceError> {
    // TrueType's convention: outer contours clockwise.
    let Some(merged_paths) = merged_contours(font, gid, true)? else {
        return Ok(None);
    };
    let glyph = paths_to_simple_glyph(&merged_paths).ok_or_else(|| SliceError::RemoveOverlaps {
        glyph: format!("{}", gid.to_u32()),
        reason: "the merged outline could not be written as a simple glyph".into(),
    })?;
    Ok(Some(glyph))
}

/// A glyph's contours as kurbo paths, with composites resolved.
fn glyph_contours(font: &FontRef, gid: GlyphId) -> Result<Vec<BezPath>, SliceError> {
    use skrifa::instance::{LocationRef, Size};
    use skrifa::outline::DrawSettings;
    use skrifa::MetadataProvider;

    let mut pen = ContourPen::default();
    let outlines = font.outline_glyphs();
    if let Some(glyph) = outlines.get(gid) {
        glyph
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut pen,
            )
            .map_err(|e| SliceError::RemoveOverlaps {
                glyph: format!("{}", gid.to_u32()),
                reason: e.to_string(),
            })?;
    }
    Ok(pen.finish())
}

/// Collects drawing operations into one `BezPath` per contour.
#[derive(Default)]
struct ContourPen {
    contours: Vec<BezPath>,
    current: Option<BezPath>,
}

impl ContourPen {
    fn finish(mut self) -> Vec<BezPath> {
        self.flush();
        self.contours
    }

    fn flush(&mut self) {
        if let Some(mut path) = self.current.take() {
            if !path.elements().is_empty() {
                path.close_path();
                self.contours.push(path);
            }
        }
    }
}

impl skrifa::outline::OutlinePen for ContourPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        let mut path = BezPath::new();
        path.move_to(Point::new(x as f64, y as f64));
        self.current = Some(path);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        if let Some(path) = &mut self.current {
            path.line_to(Point::new(x as f64, y as f64));
        }
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        if let Some(path) = &mut self.current {
            path.quad_to(
                Point::new(cx as f64, cy as f64),
                Point::new(x as f64, y as f64),
            );
        }
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        if let Some(path) = &mut self.current {
            path.curve_to(
                Point::new(c1x as f64, c1y as f64),
                Point::new(c2x as f64, c2y as f64),
                Point::new(x as f64, y as f64),
            );
        }
    }
    fn close(&mut self) {
        self.flush();
    }
}

/// Did a merge leave the total area where it was?
fn same_area(before: &[BezPath], after: &[BezPath]) -> bool {
    (total_area(before) - total_area(after)).abs() < NEGLIGIBLE_AREA
}

/// The sum of every contour's area, each counted as positive.
fn total_area(contours: &[BezPath]) -> f64 {
    contours.iter().map(|p| p.area().abs()).sum()
}

/// Merge a glyph's contours into the outline of the region they fill.
///
/// `linesweeper` implements a robust Bentley--Ottmann sweep line over Bezier paths and
/// takes the fill rule as an argument, so the whole operation is one call: the union of
/// the glyph with nothing, evaluated under the non-zero rule, is the glyph normalized.
///
/// This replaced `flo_curves`, whose `path_remove_interior_points` looks like the right
/// function and is not one. It documents itself as the non-zero rule, but
/// `GraphPath::from_path` reverses every contour to a single direction before doing
/// anything, so nothing can cancel: an 'o' came back as a solid blob. Driving its ray
/// caster directly worked, at the cost of carrying per-contour labels, restoring the
/// signs by signed area, and reconstructing nesting depth afterwards by ray-casting a
/// point on each contour's boundary. All of that is gone.
///
/// The result comes back as contours with an explicit `parent`, so nesting depth is read
/// off the tree rather than recovered from geometry. Depth still has to be turned into a
/// direction, because `glyf` and `CFF` are filled with the non-zero rule and disagree
/// about which way an outer contour runs: TrueType clockwise, PostScript
/// counter-clockwise. Fill is unaffected by a global flip -- the rule is symmetric -- but
/// a font whose contours run against its format's convention confuses tools that read
/// direction as meaning, and the hinting engines were written around it.
fn union_nonzero(contours: &[BezPath], outer_clockwise: bool) -> Result<Vec<BezPath>, SliceError> {
    let mut subject = BezPath::new();
    for contour in contours {
        subject.extend(contour.iter());
    }

    let merged = binary_op(
        &subject,
        &BezPath::new(),
        FillRule::NonZero,
        BinaryOp::Union,
    )
    .map_err(|e| SliceError::RemoveOverlaps {
        glyph: String::from("(unknown)"),
        reason: format!("the sweep line failed: {e:?}"),
    })?;

    // Depth is the length of the parent chain: even is an outer contour, odd is a hole.
    let depth_of = |mut index: ContourIdx| {
        let mut depth = 0;
        while let Some(parent) = merged[index].parent {
            depth += 1;
            index = parent;
            // A cycle cannot arise from a correct sweep, but a bound here is cheap and
            // turns a hypothetical hang into a wrong answer that a test can catch.
            if depth > merged.contours().count() {
                break;
            }
        }
        depth
    };

    Ok(merged
        .contours()
        .enumerate()
        // A hairline spike in the source -- a curve that runs out by less than a unit and
        // comes straight back, which interpolation leaves behind in real fonts -- comes
        // out of the sweep as a contour of its own that encloses nothing. Written back,
        // it is a stray path sitting on the edge of the glyph. Nothing can be nested
        // inside a contour with no area, so dropping one leaves every depth alone.
        .filter(|(_, contour)| contour.path.area().abs() >= NEGLIGIBLE_AREA)
        .map(|(i, contour)| {
            let want_clockwise = (depth_of(ContourIdx(i)) % 2 == 0) == outer_clockwise;
            // kurbo's signed area is positive for a counter-clockwise contour.
            let is_clockwise = contour.path.area() < 0.0;
            if is_clockwise == want_clockwise {
                contour.path.clone()
            } else {
                reverse_contour(&contour.path)
            }
        })
        .collect())
}

/// Reverse a closed contour's direction, keeping its start point.
fn reverse_contour(path: &BezPath) -> BezPath {
    let mut points: Vec<(Point, Point, Point)> = Vec::new();
    let mut start = Point::ZERO;
    let mut current = Point::ZERO;

    for element in path.elements() {
        match *element {
            PathEl::MoveTo(p) => {
                start = p;
                current = p;
            }
            PathEl::LineTo(p) => {
                let c1 = current.lerp(p, 1.0 / 3.0);
                let c2 = current.lerp(p, 2.0 / 3.0);
                points.push((c1, c2, p));
                current = p;
            }
            PathEl::QuadTo(q, p) => {
                let c1 = current + (q - current) * (2.0 / 3.0);
                let c2 = p + (q - p) * (2.0 / 3.0);
                points.push((c1, c2, p));
                current = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                points.push((c1, c2, p));
                current = p;
            }
            PathEl::ClosePath => {}
        }
    }

    let mut out = BezPath::new();
    if points.is_empty() {
        return out;
    }

    // Walk the segments backwards, swapping each one's control points.
    let last_end = points[points.len() - 1].2;
    out.move_to(last_end);
    for index in (0..points.len()).rev() {
        let (c1, c2, _) = points[index];
        let segment_start = if index == 0 {
            start
        } else {
            points[index - 1].2
        };
        out.curve_to(c2, c1, segment_start);
    }
    out.close_path();
    out
}

/// Write cubic contours back out as a TrueType simple glyph.
///
/// Cubics are refitted with quadratics, which is where the approximation lives. Straight
/// lines are detected and kept as lines rather than becoming degenerate curves.
fn paths_to_simple_glyph(paths: &[BezPath]) -> Option<WGlyph> {
    let mut contours: Vec<Contour> = Vec::new();

    for path in paths {
        let mut points: Vec<CurvePoint> = Vec::new();
        let mut current = Point::ZERO;
        let mut first: Option<Point> = None;

        for element in path.elements() {
            match *element {
                PathEl::MoveTo(p) => {
                    current = p;
                    first = Some(p);
                    points.push(CurvePoint::on_curve(round(p.x), round(p.y)));
                }
                PathEl::LineTo(p) => {
                    current = p;
                    points.push(CurvePoint::on_curve(round(p.x), round(p.y)));
                }
                PathEl::QuadTo(q, p) => {
                    points.push(CurvePoint::off_curve(round(q.x), round(q.y)));
                    points.push(CurvePoint::on_curve(round(p.x), round(p.y)));
                    current = p;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let cubic = CubicBez::new(current, c1, c2, p);
                    if is_straight(&cubic) {
                        points.push(CurvePoint::on_curve(round(p.x), round(p.y)));
                    } else {
                        for (_, _, quad) in cubic.to_quads(QUAD_ACCURACY) {
                            points.push(CurvePoint::off_curve(round(quad.p1.x), round(quad.p1.y)));
                            points.push(CurvePoint::on_curve(round(quad.p2.x), round(quad.p2.y)));
                        }
                    }
                    current = p;
                }
                PathEl::ClosePath => {}
            }
        }

        // A closing point that repeats the start of the contour is redundant: glyf
        // contours close implicitly.
        if let Some(first) = first {
            while points.len() > 1 {
                let last = points[points.len() - 1];
                if last.on_curve && last.x == round(first.x) && last.y == round(first.y) {
                    points.pop();
                } else {
                    break;
                }
            }
        }

        if points.len() >= 2 {
            contours.push(points.into());
        }
    }

    if contours.is_empty() {
        return Some(WGlyph::Empty);
    }

    let mut glyph = SimpleGlyph {
        bbox: Default::default(),
        contours,
        instructions: Vec::new(),
        // The whole point of this pass is that the contours no longer overlap.
        overlaps: false,
    };
    glyph.recompute_bounding_box();
    Some(WGlyph::Simple(glyph))
}

/// Is this cubic close enough to a straight line to store as one?
fn is_straight(cubic: &CubicBez) -> bool {
    let line = kurbo::Line::new(cubic.p0, cubic.p3);
    let length = (cubic.p3 - cubic.p0).hypot();
    if length < 1e-6 {
        return true;
    }
    [0.25, 0.5, 0.75].iter().all(|&t| {
        let point = cubic.eval(t);
        distance_to_line(point, line) < QUAD_ACCURACY
    })
}

fn distance_to_line(point: Point, line: kurbo::Line) -> f64 {
    let direction = line.p1 - line.p0;
    let length = direction.hypot();
    if length < 1e-9 {
        return (point - line.p0).hypot();
    }
    ((point - line.p0).cross(direction) / length).abs()
}

fn round(value: f64) -> i16 {
    crate::instancer::glyphs::ot_round(value).clamp(-32768, 32767) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two overlapping squares, drawn in the same direction.
    fn overlapping_squares() -> Vec<BezPath> {
        let mut a = BezPath::new();
        a.move_to((0.0, 0.0));
        a.line_to((100.0, 0.0));
        a.line_to((100.0, 100.0));
        a.line_to((0.0, 100.0));
        a.close_path();

        let mut b = BezPath::new();
        b.move_to((50.0, 50.0));
        b.line_to((150.0, 50.0));
        b.line_to((150.0, 150.0));
        b.line_to((50.0, 150.0));
        b.close_path();

        vec![a, b]
    }

    /// A square with a square hole, wound in opposite directions: an 'o'.
    fn square_with_counter() -> Vec<BezPath> {
        let mut outer = BezPath::new();
        outer.move_to((0.0, 0.0));
        outer.line_to((100.0, 0.0));
        outer.line_to((100.0, 100.0));
        outer.line_to((0.0, 100.0));
        outer.close_path();

        let mut inner = BezPath::new();
        inner.move_to((25.0, 25.0));
        inner.line_to((25.0, 75.0));
        inner.line_to((75.0, 75.0));
        inner.line_to((75.0, 25.0));
        inner.close_path();

        vec![outer, inner]
    }

    /// Is a point inside the filled region, under the non-zero winding rule?
    fn filled(paths: &[BezPath], point: Point) -> bool {
        let winding: i32 = paths.iter().map(|p| p.winding(point)).sum();
        winding != 0
    }

    fn union(paths: &[BezPath]) -> Vec<BezPath> {
        union_nonzero(paths, true).expect("the sweep line should handle these fixtures")
    }

    /// Sample a grid and confirm two path sets fill exactly the same points.
    ///
    /// The x and y strides differ, and neither divides the other. A grid with equal
    /// strides puts a whole diagonal of samples exactly on top of a 45-degree edge,
    /// where winding is undefined and both answers are defensible; that shows up as a
    /// row of spurious failures rather than a real one.
    fn assert_same_filled_region(before: &[BezPath], after: &[BezPath], label: &str) {
        let mut mismatches: Vec<Point> = Vec::new();
        let mut checked = 0;
        for i in 0..80 {
            for j in 0..80 {
                let point = Point::new(-12.3 + i as f64 * 2.531, -9.7 + j as f64 * 2.417);
                checked += 1;
                if filled(before, point) != filled(after, point) {
                    mismatches.push(point);
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{label}: {} of {checked} sampled points changed fill state; first few: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(6)]
        );
    }

    #[test]
    fn union_of_overlapping_squares_covers_the_same_region() {
        let before = overlapping_squares();
        let after = union(&before);
        assert_eq!(
            after.len(),
            1,
            "two overlapping squares should merge into one"
        );
        assert_same_filled_region(&before, &after, "overlapping squares");
    }

    #[test]
    fn a_counter_survives_the_merge() {
        // The critical case: an 'o' must not have its hole filled in.
        let before = square_with_counter();
        let after = union(&before);
        assert!(
            !filled(&after, Point::new(50.0, 50.0)),
            "the counter was filled in: overlap removal would destroy every 'o' in the font"
        );
        assert!(
            filled(&after, Point::new(10.0, 50.0)),
            "the ring should still be filled"
        );
        assert_same_filled_region(&before, &after, "square with counter");
    }

    /// A rectangle, wound in the given direction.
    fn rect(x0: f64, y0: f64, x1: f64, y1: f64, clockwise: bool) -> BezPath {
        let mut path = BezPath::new();
        path.move_to((x0, y0));
        if clockwise {
            // y-up, so this order traces clockwise.
            path.line_to((x0, y1));
            path.line_to((x1, y1));
            path.line_to((x1, y0));
        } else {
            path.line_to((x1, y0));
            path.line_to((x1, y1));
            path.line_to((x0, y1));
        }
        path.close_path();
        path
    }

    #[test]
    fn a_counter_inside_a_counter_survives() {
        // The shape of a circled letter: an outer ring, and inside its hole another
        // filled shape with its own hole. Nesting depth reaches three.
        //
        // This is the case that rules out the obvious shortcut of unioning all the
        // outer-wound contours and subtracting all the inner-wound ones: that would
        // subtract the ring's hole from the letter and erase it.
        let before = vec![
            rect(0.0, 0.0, 300.0, 300.0, true), // depth 0: outer ring, filled
            rect(40.0, 40.0, 260.0, 260.0, false), // depth 1: its hole
            rect(80.0, 80.0, 220.0, 220.0, true), // depth 2: the letter, filled
            rect(120.0, 120.0, 180.0, 180.0, false), // depth 3: the letter's counter
        ];

        // Sanity-check the fixture itself before trusting what it proves.
        assert!(
            filled(&before, Point::new(20.0, 150.0)),
            "outer ring should be solid"
        );
        assert!(
            !filled(&before, Point::new(60.0, 150.0)),
            "the ring's hole should be empty"
        );
        assert!(
            filled(&before, Point::new(100.0, 150.0)),
            "the letter should be solid"
        );
        assert!(
            !filled(&before, Point::new(150.0, 150.0)),
            "the letter's counter should be empty"
        );

        let after = union(&before);
        assert_same_filled_region(&before, &after, "counter inside a counter");
    }

    #[test]
    fn overlapping_rings_merge_without_losing_their_holes() {
        // Two 'o' shapes overlapping: the rings must join, and both holes must stay.
        let before = vec![
            rect(0.0, 0.0, 100.0, 100.0, true),
            rect(20.0, 20.0, 80.0, 80.0, false),
            rect(60.0, 0.0, 160.0, 100.0, true),
            rect(80.0, 20.0, 140.0, 80.0, false),
        ];

        let after = union(&before);
        assert_same_filled_region(&before, &after, "overlapping rings");
        assert!(
            !filled(&after, Point::new(40.0, 50.0)),
            "the left hole should survive"
        );
        assert!(
            !filled(&after, Point::new(120.0, 50.0)),
            "the right hole should survive"
        );
    }

    #[test]
    fn a_self_intersecting_contour_is_resolved() {
        // A bow tie: one contour that crosses itself. Under the non-zero rule both
        // lobes are filled, and the merged outline must fill them too.
        let mut bow = BezPath::new();
        bow.move_to((0.0, 0.0));
        bow.line_to((100.0, 100.0));
        bow.line_to((0.0, 100.0));
        bow.line_to((100.0, 0.0));
        bow.close_path();

        let before = vec![bow];
        let after = union(&before);
        assert_same_filled_region(&before, &after, "self-intersecting bow tie");
    }

    #[test]
    fn a_contour_wholly_inside_another_of_the_same_direction_stays_filled() {
        // Same direction means the windings add rather than cancel, so under the
        // non-zero rule the inner contour is not a hole and the result is a solid
        // rectangle.
        let before = vec![
            rect(0.0, 0.0, 100.0, 100.0, true),
            rect(25.0, 25.0, 75.0, 75.0, true),
        ];
        assert!(filled(&before, Point::new(50.0, 50.0)));

        let after = union(&before);
        assert!(
            filled(&after, Point::new(50.0, 50.0)),
            "same-direction nesting is not a hole under the non-zero rule"
        );
        assert_same_filled_region(&before, &after, "same-direction nesting");
    }

    #[test]
    fn disjoint_contours_are_left_alone() {
        let mut a = BezPath::new();
        a.move_to((0.0, 0.0));
        a.line_to((10.0, 0.0));
        a.line_to((10.0, 10.0));
        a.close_path();

        let mut b = BezPath::new();
        b.move_to((100.0, 100.0));
        b.line_to((110.0, 100.0));
        b.line_to((110.0, 110.0));
        b.close_path();

        assert!(
            merge(&[a, b], true).unwrap().is_none(),
            "contours that do not touch have nothing to merge, and should not be refitted"
        );
    }

    /// `n` from Google Sans Flex at opsz 18, wdth 80, wght 900, as the static instance
    /// draws it. One contour, and at the top of the stem it crosses itself: the edge runs
    /// down to (489, 835), back up into the stem to (449, 891), and out along y = 891 to
    /// meet the shoulder. That last edge crosses the stem's vertical one at (489, 891),
    /// and the triangle it closes off is filled twice.
    fn heavy_n() -> BezPath {
        let mut n = BezPath::new();
        n.move_to((106.0, 0.0));
        n.line_to((106.0, 1033.0));
        n.line_to((489.0, 1033.0));
        n.line_to((489.0, 835.0));
        n.line_to((449.0, 891.0));
        n.line_to((497.0, 891.0));
        n.quad_to((548.0, 979.0), (636.0, 1023.5));
        n.quad_to((724.0, 1068.0), (819.0, 1068.0));
        n.quad_to((974.0, 1068.0), (1066.0, 972.0));
        n.quad_to((1158.0, 876.0), (1158.0, 716.0));
        n.line_to((1158.0, 0.0));
        n.line_to((758.0, 0.0));
        n.line_to((758.0, 619.0));
        n.quad_to((758.0, 682.0), (726.0, 716.5));
        n.quad_to((694.0, 751.0), (635.0, 751.0));
        n.quad_to((578.0, 751.0), (542.0, 711.0));
        n.quad_to((506.0, 671.0), (506.0, 606.0));
        n.line_to((506.0, 0.0));
        n.close_path();
        n
    }

    #[test]
    fn a_contour_crossing_itself_along_straight_edges_is_merged() {
        // This is what a user saw in Illustrator: the bounding-box screen that used to
        // run first gave every horizontal or vertical segment a box of zero area, so it
        // never saw this crossing and the glyph was passed over.
        let before = vec![heavy_n()];
        assert_eq!(
            before[0].winding(Point::new(480.0, 870.0)).abs(),
            2,
            "the fixture should cover the triangle twice"
        );

        let after = merge(&before, true)
            .unwrap()
            .expect("a contour that crosses itself has something to merge");
        assert_eq!(after.len(), 1, "an n is one contour once merged");

        // A contour that does not cross itself encloses exactly the area it fills. The
        // one going in counted the triangle twice, and the triangle is
        // 1/2 * 56 * 40 = 1120 square units.
        let doubled = total_area(&before) - total_area(&after);
        assert!(
            (doubled - 1120.0).abs() < NEGLIGIBLE_AREA,
            "the merge should remove exactly the triangle's second layer, removed {doubled}"
        );

        // And the glyph fills what it filled before. The strides avoid every edge.
        for i in 0..90 {
            for j in 0..88 {
                let point = Point::new(-20.0 + i as f64 * 13.7, -15.0 + j as f64 * 12.9);
                assert_eq!(
                    filled(&before, point),
                    filled(&after, point),
                    "fill changed at {point:?}"
                );
            }
        }
    }

    /// `acutecomb.viet` from the same instance, drawn the way skrifa draws it. Every
    /// corner carries a curve of zero length, and at both bottom corners the outline makes
    /// a hairline spike -- a curve that runs half a unit straight up and comes back --
    /// which interpolation leaves in the font.
    fn acute_with_spikes() -> BezPath {
        let mut acute = BezPath::new();
        acute.move_to((-241.0, 1106.0));
        acute.quad_to((-241.0, 1106.0), (-241.0, 1106.5));
        acute.quad_to((-241.0, 1107.0), (-241.0, 1106.0));
        acute.line_to((-104.0, 1473.0));
        acute.quad_to((-104.0, 1473.0), (-104.0, 1473.0));
        acute.quad_to((-104.0, 1473.0), (-104.0, 1473.0));
        acute.line_to((241.0, 1473.0));
        acute.quad_to((241.0, 1473.0), (241.0, 1473.0));
        acute.quad_to((241.0, 1473.0), (241.0, 1473.0));
        acute.line_to((43.0, 1106.0));
        acute.quad_to((43.0, 1107.0), (43.0, 1106.5));
        acute.quad_to((43.0, 1106.0), (43.0, 1106.0));
        acute.close_path();
        acute
    }

    #[test]
    fn a_hairline_spike_does_not_become_a_stray_contour() {
        let spiked = acute_with_spikes();

        // The sweep splits the spike off as a contour of its own. It is dropped, so an
        // outline with nothing else to merge is left exactly as it was.
        assert_eq!(union(std::slice::from_ref(&spiked)).len(), 1);
        assert!(merge(std::slice::from_ref(&spiked), true)
            .unwrap()
            .is_none());

        // And when something else does need merging, the spike does not come along.
        let overlapping = rect(0.0, 1300.0, 300.0, 1600.0, true);
        let before = vec![spiked, overlapping];
        let after = merge(&before, true)
            .unwrap()
            .expect("the acute and the rectangle overlap");
        for contour in &after {
            assert!(
                contour.area().abs() >= NEGLIGIBLE_AREA,
                "a contour enclosing {} square units was written back",
                contour.area().abs()
            );
        }
    }

    /// How far does merging move a real font's glyph areas?
    ///
    /// Answers whether `NEGLIGIBLE_AREA` sits in a real gap. A glyph with no overlaps
    /// should come back from the sweep with its area unchanged to within floating-point
    /// noise, and one with overlaps should lose at least the area it filled twice. This
    /// prints the largest change below the threshold and the smallest above it, and how
    /// long sweeping every glyph took.
    ///
    /// ```sh
    /// SLICE_PROBE_FONT=plain.ttf cargo test --release -p slice-core --lib \
    ///     probe_area_change -- --ignored --nocapture
    /// ```
    ///
    /// The font must be static. The recipe for the instance the threshold was measured on
    /// is in `tools/README.md`, under `overlap-check.py`.
    #[test]
    #[ignore = "a probe: needs SLICE_PROBE_FONT pointing at a static font"]
    fn probe_area_change() {
        let path = std::env::var("SLICE_PROBE_FONT").expect("set SLICE_PROBE_FONT");
        let bytes = std::fs::read(&path).expect("the font should be readable");
        let font = FontRef::new(&bytes).expect("the file should be a font");
        let (mut largest_below, mut smallest_above) = (0.0f64, f64::MAX);
        let (mut below, mut above) = (0, 0);
        let started = std::time::Instant::now();
        for gid in 0..font.maxp().expect("maxp").num_glyphs() {
            let contours = glyph_contours(&font, GlyphId::new(gid as u32)).unwrap();
            if contours.is_empty() {
                continue;
            }
            let merged = union_nonzero(&contours, true).unwrap();
            let change = (total_area(&contours) - total_area(&merged)).abs();
            if change < NEGLIGIBLE_AREA {
                largest_below = largest_below.max(change);
                below += 1;
            } else {
                smallest_above = smallest_above.min(change);
                above += 1;
            }
        }
        println!("{below} glyphs changed area by less than {NEGLIGIBLE_AREA}; the most by {largest_below:e}");
        println!("{above} glyphs changed area by more; the least by {smallest_above:.1}");
        println!(
            "drawing and sweeping every glyph took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_contour_that_encloses_nothing_is_left_alone() {
        // A two-point contour is a line, and fills nothing. The merge returns nothing for
        // it, which is not a failure: there was nothing there to draw.
        let mut line = BezPath::new();
        line.move_to((0.0, 0.0));
        line.line_to((100.0, 100.0));
        line.close_path();
        assert!(merge(&[line], true).unwrap().is_none());
    }

    #[test]
    fn a_straight_cubic_is_recognised() {
        let straight = CubicBez::new(
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(20.0, 0.0),
            Point::new(30.0, 0.0),
        );
        assert!(is_straight(&straight));

        let curved = CubicBez::new(
            Point::new(0.0, 0.0),
            Point::new(10.0, 40.0),
            Point::new(20.0, 40.0),
            Point::new(30.0, 0.0),
        );
        assert!(!is_straight(&curved));
    }
}
