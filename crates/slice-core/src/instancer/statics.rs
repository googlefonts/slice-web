//! Producing a static instance: every axis pinned, all variation data resolved away.
//!
//! This is the case most people reach for Slice to get — one weight, one width, frozen
//! and ready to hand to a design application — and it is also the case where removing
//! overlaps matters most, because that is where the outlines finally stop moving.

use std::collections::HashMap;

use read_fonts::{FontRef, TableProvider};
use write_fonts::tables::glyf::{GlyfLocaBuilder, Glyph as WGlyph};
use write_fonts::tables::hmtx::{Hmtx, LongMetric};
use write_fonts::types::{GlyphId, Tag};
use write_fonts::{from_obj::ToOwnedTable, FontBuilder};

use super::glyphs::{apply_gvar_deltas, build_glyph, ot_round, read_glyph, GlyphShape};
use super::mvar;
use super::normalize::NormalizedLocation;
use super::regions::Request;
use crate::SliceError;

/// Tables that describe variation and therefore have no place in a static instance.
///
/// `cvar` and the metric variation tables go because there is nothing left to vary.
///
/// `STAT` is *not* here. A static instance keeps it, with its axis values filtered to
/// the pinned location: that is how the instance declares "this is Casual 1, Weight
/// 1000" to a font menu, and fontTools keeps it for the same reason.
pub const VARIATION_TABLES: &[Tag] = &[
    Tag::new(b"fvar"),
    Tag::new(b"avar"),
    Tag::new(b"gvar"),
    Tag::new(b"cvar"),
    Tag::new(b"HVAR"),
    Tag::new(b"VVAR"),
    Tag::new(b"MVAR"),
];

/// Build a static instance of `font` at `location`.
///
/// `plans` describes the same request in user space, and is needed to filter `STAT`:
/// an axis value naming a weight the instance is not pinned at has to go.
///
/// The returned bytes are a complete sfnt. Name records and bit flags are *not* applied
/// here; the caller does that afterwards, so that the same code path serves the partial
/// instancing case too.
pub fn instantiate_static(
    font: &FontRef,
    location: &NormalizedLocation,
    plans: &[super::partial::AxisPlan],
) -> Result<Vec<u8>, SliceError> {
    if font.glyf().is_ok() {
        return static_glyf(font, location, plans);
    }
    if font.cff2().is_ok() {
        return static_cff2(font, location, plans);
    }
    Err(SliceError::Unsupported(
        "This font has CFF 1.0 outlines, which are not variable and which this build \
         cannot rewrite. Only 'glyf' and 'CFF2' outlines can be instanced."
            .into(),
    ))
}

/// A static instance of a `glyf` font: resolve every point at the location and drop
/// `gvar` along with the rest of the variation data.
fn static_glyf(
    font: &FontRef,
    location: &NormalizedLocation,
    plans: &[super::partial::AxisPlan],
) -> Result<Vec<u8>, SliceError> {
    let num_glyphs = font.maxp()?.num_glyphs();

    // Pass one: resolve every glyph's points at the target location, and remember the
    // metrics its phantom points imply.
    let mut shapes: Vec<WGlyph> = Vec::with_capacity(num_glyphs as usize);
    let mut phantom_x: Vec<(f64, f64)> = Vec::with_capacity(num_glyphs as usize);
    let mut is_composite = vec![false; num_glyphs as usize];

    for gid in 0..num_glyphs {
        let gid = GlyphId::new(gid as u32);
        let mut points = read_glyph(font, gid)?;
        apply_gvar_deltas(font, gid, location, &mut points)?;

        let phantoms = points.phantoms();
        phantom_x.push((phantoms[0].0, phantoms[1].0));
        is_composite[gid.to_u32() as usize] = matches!(points.shape, GlyphShape::Composite { .. });

        let mut glyph = build_glyph(&points);
        // A static instance may still have overlapping contours, and the flags that say
        // so are the only hint a rasteriser gets. fontTools sets them here too, under
        // its default OverlapMode::KEEP_AND_SET_FLAGS, which is what the original Slice
        // ran with. If overlaps are removed later, that pass clears them again.
        match &mut glyph {
            WGlyph::Simple(simple) => simple.overlaps = true,
            WGlyph::Composite(composite) => {
                if let Some(first) = composite.components_mut().first_mut() {
                    first.flags.overlap_compound = true;
                }
            }
            WGlyph::Empty => {}
        }
        shapes.push(glyph);
    }

    // Pass two: a composite's bounding box depends on the glyphs it references, so it can
    // only be computed once every simple glyph is final.
    let bboxes = compute_bounding_boxes(&shapes, &is_composite);
    for (gid, glyph) in shapes.iter_mut().enumerate() {
        if let WGlyph::Composite(composite) = glyph {
            *composite = rebuild_with_bbox(composite, bboxes[gid]);
        }
    }

    // Metrics come from the phantom points and the *new* bounds.
    let mut metrics: Vec<(u16, i16)> = Vec::with_capacity(num_glyphs as usize);
    for (gid, glyph) in shapes.iter().enumerate() {
        let (pp1, pp2) = phantom_x[gid];
        let advance = ot_round(pp2 - pp1).max(0) as u16;
        let x_min = match glyph {
            WGlyph::Empty => 0,
            WGlyph::Simple(simple) => simple.bbox.x_min,
            WGlyph::Composite(_) => bboxes[gid].x_min,
        };
        let lsb = ot_round(f64::from(x_min) - pp1) as i16;
        metrics.push((advance, lsb));
    }

    let mut builder = GlyfLocaBuilder::new();
    for glyph in &shapes {
        builder
            .add_glyph(glyph)
            .map_err(|e| SliceError::Write(format!("could not write a glyph: {e}")))?;
    }
    let (glyf, loca, loca_format) = builder.build();

    // Assemble the new font.
    let mut out = FontBuilder::new();
    out.add_table(&glyf)
        .map_err(|e| SliceError::Write(e.to_string()))?;
    out.add_table(&loca)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    finish_static(
        &mut out,
        font,
        &metrics,
        Some(loca_format as i16),
        location,
        plans,
    )
}

/// A static instance of a `CFF2` font.
///
/// The outlines are handled by [`super::cff2`], which resolves the blends in place and
/// drops the variation store; what is left here is the metrics, which CFF2 keeps in
/// `hmtx` and `HVAR` rather than in phantom points. fontTools 4.62.1 does the same and
/// leaves the result as CFF2 rather than downgrading it to CFF 1.0.
fn static_cff2(
    font: &FontRef,
    location: &NormalizedLocation,
    plans: &[super::partial::AxisPlan],
) -> Result<Vec<u8>, SliceError> {
    let instance = super::cff2::instantiate(font, &super::cff2::Request::Pinned(&location.coords))?;

    // A CFF2 glyph has no phantom points, so the advance comes from `hmtx` plus whatever
    // `HVAR` says at this location. The side bearing is left alone: fontTools does not
    // recompute it here either, and the outline's own left edge is not what `hmtx`
    // records for a CFF font.
    let hmtx = font.hmtx()?;
    let hvar = font.hvar().ok();
    let coords: Vec<write_fonts::types::F2Dot14> = location
        .coords
        .iter()
        .map(|c| write_fonts::types::F2Dot14::from_f32(*c as f32))
        .collect();

    let num_glyphs = font.maxp()?.num_glyphs();
    let mut metrics: Vec<(u16, i16)> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let gid = GlyphId::new(gid as u32);
        let advance = f64::from(hmtx.advance(gid).unwrap_or(0));
        let delta = hvar
            .as_ref()
            .and_then(|hvar| hvar.advance_width_delta(gid, &coords).ok())
            .map(|d| d.to_f64())
            .unwrap_or(0.0);
        metrics.push((
            ot_round(advance + delta).max(0) as u16,
            hmtx.side_bearing(gid).unwrap_or(0),
        ));
    }

    let mut out = FontBuilder::new();
    out.add_raw(super::cff2::table::CFF2_TAG, instance.table);

    finish_static(&mut out, font, &metrics, None, location, plans)
}

/// Everything a static instance needs once its outlines and metrics are settled.
///
/// `loca_format` is `None` for a font whose outlines are not in `glyf`, where `head` has
/// nothing to say about a `loca` table that does not exist.
fn finish_static<'a>(
    out: &mut FontBuilder<'a>,
    font: &FontRef<'a>,
    metrics: &[(u16, i16)],
    loca_format: Option<i16>,
    location: &NormalizedLocation,
    plans: &[super::partial::AxisPlan],
) -> Result<Vec<u8>, SliceError> {
    out.add_table(&build_hmtx(metrics))
        .map_err(|e| SliceError::Write(e.to_string()))?;

    let mut head: write_fonts::tables::head::Head = font.head()?.to_owned_table();
    if let Some(format) = loca_format {
        head.index_to_loc_format = format;
    }
    out.add_table(&head)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    // hhea records how many long metrics hmtx holds, and the widest advance among them.
    // `finalize` recomputes the rest of hhea from `glyf`, which a CFF font does not have,
    // so this is the only chance to keep advanceWidthMax honest there.
    let mut hhea: write_fonts::tables::hhea::Hhea = font.hhea()?.to_owned_table();
    hhea.number_of_h_metrics = long_metric_count(metrics) as u16;
    hhea.advance_width_max = advance_width_max(metrics);
    out.add_table(&hhea)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    // Apply MVAR before it is dropped, so the font's metrics land where the location
    // says they should rather than staying at their default values.
    let adjustments = mvar::metric_adjustments(font, location);
    if let Ok(os2) = font.os2() {
        let mut os2: write_fonts::tables::os2::Os2 = os2.to_owned_table();
        mvar::apply_to_os2(&mut os2, &adjustments);
        out.add_table(&os2)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }
    if !adjustments.is_empty() {
        if let Some(hhea_table) = out_hhea_with_mvar(font, &adjustments) {
            let mut hhea = hhea_table;
            hhea.number_of_h_metrics = long_metric_count(metrics) as u16;
            hhea.advance_width_max = advance_width_max(metrics);
            out.add_table(&hhea)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }

        // `post` carries the underline position and thickness, which MVAR varies through
        // the `undo` and `unds` tags. The partial instancer has always baked these in;
        // the static path did not, so a pinned instance kept the default master's
        // underline while its ascender and x-height moved. `owned_post` rather than a
        // plain `to_owned_table` because a version 2.0 `post` with no extra names has to
        // be repaired before write-fonts will accept it.
        if let Some(mut post) = crate::finalize::owned_post(font) {
            mvar::apply_to_post(&mut post, &adjustments);
            out.add_table(&post)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }
    }

    // STAT survives, with the axis values the pinned location can no longer reach
    // removed. See `partial::build_stat`.
    if let Some(stat) = super::partial::build_stat(font, plans)? {
        out.add_table(&stat)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }

    // GDEF's item variation store is what variable kerning and anchors are measured
    // against. With every axis pinned, each of its delta sets becomes a constant -- its
    // value at this location -- which goes into the GPOS value record, anchor or ligature
    // caret that addresses it, and then the store and the addresses go. These tables used
    // to be copied through untouched, which left a store nothing can evaluate: with no
    // fvar a shaper has no coordinates, so the instance kerned like the default master.
    let mut skip = VARIATION_TABLES.to_vec();
    if let Some((gdef, gpos)) =
        super::partial::rebuilt_gdef(font, &Request::Pinned(&location.coords))?
    {
        if holds_nothing(&gdef) {
            // The store was all it carried, and an empty GDEF says nothing. fontTools
            // drops it too.
            skip.push(Tag::new(b"GDEF"));
        } else {
            out.add_table(&gdef)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }
        if let Some(gpos) = gpos {
            out.add_table(&gpos)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }
    }

    // Everything else is copied across verbatim, minus the variation tables.
    copy_remaining_tables(out, font, &skip);

    Ok(std::mem::take(out).build())
}

/// True when a `GDEF` has nothing left in it: no glyph classes, attachment points,
/// carets, mark classes, mark sets or variation store.
fn holds_nothing(gdef: &write_fonts::tables::gdef::Gdef) -> bool {
    gdef.glyph_class_def.is_none()
        && gdef.attach_list.is_none()
        && gdef.lig_caret_list.is_none()
        && gdef.mark_attach_class_def.is_none()
        && gdef.mark_glyph_sets_def.is_none()
        && gdef.item_var_store.is_none()
}

/// The widest advance in `hmtx`, which is what `hhea.advanceWidthMax` reports.
fn advance_width_max(metrics: &[(u16, i16)]) -> write_fonts::types::UfWord {
    write_fonts::types::UfWord::new(
        metrics
            .iter()
            .map(|(advance, _)| *advance)
            .max()
            .unwrap_or(0),
    )
}

/// Copy every table the builder does not already hold, skipping `skip`.
pub fn copy_remaining_tables<'a>(builder: &mut FontBuilder<'a>, font: &FontRef<'a>, skip: &[Tag]) {
    for record in font.table_directory().table_records() {
        let tag = record.tag();
        if builder.contains(tag) || skip.contains(&tag) {
            continue;
        }
        if let Some(data) = font.table_data(tag) {
            builder.add_raw(tag, data.as_bytes());
        }
    }
}

fn out_hhea_with_mvar(
    font: &FontRef,
    adjustments: &HashMap<Tag, f64>,
) -> Option<write_fonts::tables::hhea::Hhea> {
    let mut hhea: write_fonts::tables::hhea::Hhea = font.hhea().ok()?.to_owned_table();
    mvar::apply_to_hhea(&mut hhea, adjustments);
    Some(hhea)
}

/// Pack per-glyph metrics into an `hmtx`, shared with the partial instancer.
pub fn build_hmtx_public(metrics: &[(u16, i16)]) -> Hmtx {
    build_hmtx(metrics)
}

/// How many long metrics `hmtx` needs, shared with the partial instancer.
pub fn long_metric_count_public(metrics: &[(u16, i16)]) -> usize {
    long_metric_count(metrics)
}

/// Pack per-glyph metrics into an `hmtx`, compressing the trailing run of equal advances.
fn build_hmtx(metrics: &[(u16, i16)]) -> Hmtx {
    let long_count = long_metric_count(metrics);
    let h_metrics = metrics[..long_count]
        .iter()
        .map(|&(advance, side_bearing)| LongMetric {
            advance,
            side_bearing,
        })
        .collect();
    let left_side_bearings = metrics[long_count..].iter().map(|&(_, sb)| sb).collect();
    Hmtx {
        h_metrics,
        left_side_bearings,
    }
}

/// How many `longHorMetric` records `hmtx` needs.
///
/// Glyphs at the end that all share the last advance width can be stored as bare side
/// bearings, which is what `numberOfHMetrics` is for. At least one record is required.
fn long_metric_count(metrics: &[(u16, i16)]) -> usize {
    if metrics.is_empty() {
        return 0;
    }
    let last_advance = metrics[metrics.len() - 1].0;
    let mut count = metrics.len();
    while count > 1 && metrics[count - 2].0 == last_advance {
        count -= 1;
    }
    count
}

/// A glyph's bounding box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl Bounds {
    fn union(self, other: Bounds) -> Bounds {
        Bounds {
            x_min: self.x_min.min(other.x_min),
            y_min: self.y_min.min(other.y_min),
            x_max: self.x_max.max(other.x_max),
            y_max: self.y_max.max(other.y_max),
        }
    }
}

/// Bounding boxes for every glyph, resolving composites against their components.
///
/// Composites can nest, so this iterates until nothing changes rather than recursing;
/// that also means a cyclic composite reference (which is malformed, but does occur in
/// the wild) terminates instead of overflowing the stack.
fn compute_bounding_boxes(shapes: &[WGlyph], is_composite: &[bool]) -> Vec<Bounds> {
    let mut bounds: Vec<Bounds> = shapes
        .iter()
        .map(|glyph| match glyph {
            WGlyph::Simple(simple) => Bounds {
                x_min: simple.bbox.x_min,
                y_min: simple.bbox.y_min,
                x_max: simple.bbox.x_max,
                y_max: simple.bbox.y_max,
            },
            _ => Bounds::default(),
        })
        .collect();

    // A composite can only reference glyphs whose bounds are already known after enough
    // passes; the depth of nesting bounds the number of passes needed.
    const MAX_PASSES: usize = 8;
    for _ in 0..MAX_PASSES {
        let mut changed = false;
        for (gid, glyph) in shapes.iter().enumerate() {
            if !is_composite[gid] {
                continue;
            }
            let WGlyph::Composite(composite) = glyph else {
                continue;
            };
            let mut acc: Option<Bounds> = None;
            for component in composite.components() {
                let base = component.glyph.to_u32() as usize;
                if base >= bounds.len() || base == gid {
                    continue;
                }
                let b = bounds[base];
                let (dx, dy) = match component.anchor {
                    write_fonts::tables::glyf::Anchor::Offset { x, y } => {
                        (f64::from(x), f64::from(y))
                    }
                    write_fonts::tables::glyf::Anchor::Point { .. } => (0.0, 0.0),
                };
                let t = component.transform;
                let corners = [
                    (f64::from(b.x_min), f64::from(b.y_min)),
                    (f64::from(b.x_max), f64::from(b.y_min)),
                    (f64::from(b.x_min), f64::from(b.y_max)),
                    (f64::from(b.x_max), f64::from(b.y_max)),
                ];
                let mut cb: Option<Bounds> = None;
                for (x, y) in corners {
                    let tx = t.xx.to_f32() as f64 * x + t.xy.to_f32() as f64 * y + dx;
                    let ty = t.yx.to_f32() as f64 * x + t.yy.to_f32() as f64 * y + dy;
                    let point = Bounds {
                        x_min: ot_round(tx) as i16,
                        y_min: ot_round(ty) as i16,
                        x_max: ot_round(tx) as i16,
                        y_max: ot_round(ty) as i16,
                    };
                    cb = Some(match cb {
                        None => point,
                        Some(existing) => existing.union(point),
                    });
                }
                if let Some(cb) = cb {
                    acc = Some(match acc {
                        None => cb,
                        Some(existing) => existing.union(cb),
                    });
                }
            }
            let new = acc.unwrap_or_default();
            if bounds[gid] != new {
                bounds[gid] = new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    bounds
}

/// Rebuild a composite with a known bounding box.
///
/// `CompositeGlyph` fixes its bbox at construction time and offers no setter, so the
/// glyph is reassembled from its components.
fn rebuild_with_bbox(
    composite: &write_fonts::tables::glyf::CompositeGlyph,
    bounds: Bounds,
) -> write_fonts::tables::glyf::CompositeGlyph {
    let bbox = write_fonts::tables::glyf::Bbox {
        x_min: bounds.x_min,
        y_min: bounds.y_min,
        x_max: bounds.x_max,
        y_max: bounds.y_max,
    };
    let mut components = composite.components().iter().cloned();
    let first = components.next().expect("composite with no components");
    let mut out = write_fonts::tables::glyf::CompositeGlyph::new(first, bbox);
    for component in components {
        out.add_component(component, bbox);
    }
    let instructions = composite.instructions();
    if !instructions.is_empty() {
        out.set_instructions(instructions);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_equal_advances_are_compressed() {
        assert_eq!(long_metric_count(&[(500, 0), (500, 1), (500, 2)]), 1);
        assert_eq!(long_metric_count(&[(400, 0), (500, 1), (500, 2)]), 2);
        assert_eq!(long_metric_count(&[(400, 0), (500, 1), (600, 2)]), 3);
        assert_eq!(long_metric_count(&[(500, 0)]), 1);
        assert_eq!(long_metric_count(&[]), 0);
    }

    #[test]
    fn hmtx_round_trips_through_the_compression() {
        let metrics = vec![(400, -10), (500, 5), (500, 7), (500, 9)];
        let hmtx = build_hmtx(&metrics);
        assert_eq!(hmtx.h_metrics.len(), 2);
        assert_eq!(hmtx.left_side_bearings, vec![7, 9]);
    }
}
