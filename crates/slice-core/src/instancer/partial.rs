//! Partial instancing: narrowing the design space while the font stays variable.
//!
//! This is the case the original Slice is really for. Pinning every axis gives a static
//! font, which [`super::statics`] handles; here some axes survive, either with their
//! whole original extent or restricted to a smaller one, and the variation data has to
//! be rewritten to describe the same shapes over the smaller space.
//!
//! The mathematics is in [`crate::solver`]. What is here is the bookkeeping around it:
//! taking each glyph's tuple variations apart, rebasing every axis of every tuple,
//! recombining what comes back, folding whatever no longer depends on any axis into the
//! outline itself, and rebuilding `fvar`, `avar` and `STAT` to match.
//!
//! # What is carried over, and what is not
//!
//! | table | what happens |
//! |---|---|
//! | `glyf` / `gvar` | rebased; tuples that lost all their axes are baked into the outlines |
//! | `CFF2` | the charstrings' blends and the table's own variation store are re-tented; see [`super::cff2`] |
//! | `hmtx` | recomputed from the phantom points, as always |
//! | `fvar` | pinned axes removed, restricted axes narrowed, named instances filtered |
//! | `avar` | segment maps renormalized onto the new axis extents |
//! | `STAT` | design axis records for pinned axes removed, axis values re-indexed |
//! | `HVAR` | **dropped** for `glyf`, **rebuilt** for `CFF2`. Dropping it loses nothing on a TrueType font: advance widths vary through the phantom points in `gvar`, which are rebased along with everything else, and a renderer falls back to them when `HVAR` is absent. A CFF2 glyph has no phantom points, so `HVAR` is the only place its advance varies and it has to be re-tented instead. |
//! | `VVAR` | **dropped.** Vertical metrics are not otherwise handled here; see the README. |
//! | `MVAR` | applied at the new default and then dropped, so the metrics are right there but no longer vary across whatever range is left. |
//! | `GDEF` / `GPOS` variation stores | **re-tented.** The store's regions are rebased onto the surviving axes, and the residual it leaves at the new default location is written back into the `GPOS` value records, anchors and ligature carets that address it. Without that last step the kerning would be right at the default and wrong everywhere else, which is exactly what pinning an axis away from its default produces. See `gpos_residual`. |

use std::collections::BTreeMap;

use read_fonts::{FontRef, TableProvider};
use write_fonts::tables::avar::{Avar, AxisValueMap, SegmentMaps};
use write_fonts::tables::fvar::InstanceRecord;
use write_fonts::tables::fvar::{AxisInstanceArrays, Fvar, VariationAxisRecord};
use write_fonts::tables::glyf::{GlyfLocaBuilder, Glyph as WGlyph};
use write_fonts::tables::gpos::Gpos;
use write_fonts::tables::gvar::{GlyphDelta, GlyphDeltas, GlyphVariations, Gvar};
use write_fonts::tables::variations::Tent;
use write_fonts::types::{F2Dot14, Fixed, GlyphId, Tag};
use write_fonts::{from_obj::ToOwnedTable, FontBuilder};

use super::glyphs::{build_glyph, ot_round, read_glyph, GlyphPoints};
use super::normalize::{apply_segment_map, normalize_axis, quantize, segment_maps};
use crate::axes::{AxisLimit, AxisSpec};
use crate::solver::{rebase_tent, AxisTriple, Tent as SolverTent};
use crate::SliceError;

/// What is to become of one axis.
#[derive(Clone, Debug)]
pub struct AxisPlan {
    pub spec: AxisSpec,
    pub limit: AxisLimit,
    /// The limit in normalized coordinates, with the pre-normalization distances the
    /// solver needs to renormalize across an off-centre default.
    pub normalized: AxisTriple,
}

impl AxisPlan {
    pub fn is_pinned(&self) -> bool {
        !self.limit.keeps_axis()
    }

    /// True when the axis comes through untouched.
    pub fn is_untouched(&self) -> bool {
        matches!(self.limit, AxisLimit::Full)
    }

    /// The axis extent to write into the output `fvar`, in user space.
    pub fn output_extent(&self) -> (f64, f64, f64) {
        match self.limit {
            AxisLimit::Full => (self.spec.min, self.spec.default, self.spec.max),
            AxisLimit::Range { min, max, .. } => (min, self.spec.default, max),
            // Pinned axes are not written out at all.
            AxisLimit::Pin(v) => (v, v, v),
        }
    }
}

/// Work out, for each axis, what the request means in normalized coordinates.
pub fn plan_axes(font: &FontRef, axes: &[AxisSpec], limits: &[AxisLimit]) -> Vec<AxisPlan> {
    let maps = segment_maps(font).map(|(maps, _)| maps);

    axes.iter()
        .zip(limits)
        .enumerate()
        .map(|(index, (spec, limit))| {
            let map = maps.as_ref().and_then(|m| m.get(index));
            let normalize = |value: f64| {
                let normalized = quantize(normalize_axis(value, spec));
                match map {
                    Some(map) => quantize(apply_segment_map(normalized, map)),
                    None => normalized,
                }
            };

            let (min, default, max) = match limit {
                AxisLimit::Full => (spec.min, spec.default, spec.max),
                AxisLimit::Pin(v) => (*v, *v, *v),
                // Level 3 sub-spacing requires the range to contain the default, and
                // `AxisSpec::validate` enforces that before anything gets here. Clamp
                // anyway: the solver asserts on an unsorted triple, and a panic deep in
                // the engine is a poor way to report a caller's mistake.
                AxisLimit::Range { min, max, .. } => (*min, spec.default.clamp(*min, *max), *max),
            };

            AxisPlan {
                spec: spec.clone(),
                limit: *limit,
                normalized: AxisTriple::with_distances(
                    normalize(min),
                    normalize(default),
                    normalize(max),
                    // The pre-normalization widths of the two halves of the *original*
                    // axis. Without these, renormalizing a value that crosses the
                    // default is wrong whenever the default is not centred.
                    spec.default - spec.min,
                    spec.max - spec.default,
                ),
            }
        })
        .collect()
}

/// One tuple variation, in a form that can be taken apart and put back together.
#[derive(Clone, Debug)]
struct TupleVar {
    /// Support per *input* axis index. An axis that is absent does not participate.
    axes: BTreeMap<usize, SolverTent>,
    /// One delta per point, including the four phantom points. Always dense: sparse
    /// tuples have their missing deltas interpolated on the way in, because scaling and
    /// splitting a tuple are both linear and so commute with that interpolation.
    deltas: Vec<(f64, f64)>,
}

impl TupleVar {
    /// A key that identifies the tuple's support, for merging tuples that ended up
    /// covering the same region.
    ///
    /// Quantized to F2Dot14 because that is the precision the tents will be written at;
    /// two tents that differ only below it are the same tent in the file.
    fn support_key(&self) -> Vec<(usize, i16, i16, i16)> {
        self.axes
            .iter()
            .map(|(index, (start, peak, end))| {
                (
                    *index,
                    F2Dot14::from_f32(*start as f32).to_bits(),
                    F2Dot14::from_f32(*peak as f32).to_bits(),
                    F2Dot14::from_f32(*end as f32).to_bits(),
                )
            })
            .collect()
    }

    fn scale(&mut self, scalar: f64) {
        for delta in &mut self.deltas {
            delta.0 *= scalar;
            delta.1 *= scalar;
        }
    }
}

/// Apply one axis's limit to one tuple, which may drop it, keep it, or split it.
fn limit_on_axis(var: &TupleVar, axis: usize, limit: AxisTriple) -> Vec<TupleVar> {
    let Some(&tent) = var.axes.get(&axis) else {
        // The axis does not participate; nothing to do.
        return vec![var.clone()];
    };
    let (lower, peak, upper) = tent;

    if peak == 0.0 {
        // An axis explicitly present but peaking at zero contributes nothing and can be
        // dropped outright (fontTools issue #3453).
        let mut out = var.clone();
        out.axes.remove(&axis);
        return vec![out];
    }

    // A malformed tent -- out of order, or straddling the default -- has no defensible
    // meaning, and the solver asserts on it. Drop the tuple.
    if !(lower <= peak && peak <= upper) || (lower < 0.0 && upper > 0.0) {
        return Vec::new();
    }

    rebase_tent(tent, limit)
        .into_iter()
        .map(|(scalar, new_tent)| {
            let mut out = var.clone();
            match new_tent {
                None => {
                    out.axes.remove(&axis);
                }
                Some(t) => {
                    out.axes.insert(axis, t);
                }
            }
            out.scale(scalar);
            out
        })
        .collect()
}

/// Rebase every tuple onto the new axis extents.
fn limit_tuples(mut tuples: Vec<TupleVar>, plans: &[AxisPlan]) -> Vec<TupleVar> {
    for (index, plan) in plans.iter().enumerate() {
        // An axis nobody asked to change needs no rebasing.
        if plan.is_untouched() {
            continue;
        }
        let mut next = Vec::with_capacity(tuples.len());
        for var in &tuples {
            next.extend(limit_on_axis(var, index, plan.normalized));
        }
        tuples = next;
    }
    tuples
}

/// Combine tuples covering the same region, and separate out the part that no longer
/// depends on any axis.
///
/// The returned deltas apply unconditionally and belong in the outline itself.
fn merge(tuples: Vec<TupleVar>, point_count: usize) -> (Vec<(f64, f64)>, Vec<TupleVar>) {
    let mut merged: BTreeMap<Vec<(usize, i16, i16, i16)>, TupleVar> = BTreeMap::new();
    for var in tuples {
        let key = var.support_key();
        match merged.get_mut(&key) {
            Some(existing) => {
                for (slot, delta) in existing.deltas.iter_mut().zip(&var.deltas) {
                    slot.0 += delta.0;
                    slot.1 += delta.1;
                }
            }
            None => {
                merged.insert(key, var);
            }
        }
    }

    let default = merged
        .remove(&Vec::new())
        .map(|var| var.deltas)
        .unwrap_or_else(|| vec![(0.0, 0.0); point_count]);

    (default, merged.into_values().collect())
}

/// Read a glyph's tuples, with sparse deltas interpolated so every point has one.
fn read_tuples(
    font: &FontRef,
    gid: GlyphId,
    points: &GlyphPoints,
) -> Result<Vec<TupleVar>, SliceError> {
    let Ok(gvar) = font.gvar() else {
        return Ok(Vec::new());
    };
    let Some(data) = gvar.glyph_variation_data(gid).ok().flatten() else {
        return Ok(Vec::new());
    };

    let point_count = points.coords.len();
    let mut out = Vec::new();

    for tuple in data.tuples() {
        let peak: Vec<f64> = tuple
            .peak()
            .values()
            .iter()
            .map(|v| v.get().to_f64())
            .collect();
        let start: Option<Vec<f64>> = tuple
            .intermediate_start()
            .map(|t| t.values().iter().map(|v| v.get().to_f64()).collect());
        let end: Option<Vec<f64>> = tuple
            .intermediate_end()
            .map(|t| t.values().iter().map(|v| v.get().to_f64()).collect());

        let mut axes = BTreeMap::new();
        for (index, &peak_value) in peak.iter().enumerate() {
            if peak_value == 0.0 {
                continue;
            }
            let (lower, upper) = match (&start, &end) {
                (Some(s), Some(e)) => (
                    s.get(index).copied().unwrap_or(0.0),
                    e.get(index).copied().unwrap_or(0.0),
                ),
                _ => {
                    if peak_value > 0.0 {
                        (0.0, peak_value)
                    } else {
                        (peak_value, 0.0)
                    }
                }
            };
            axes.insert(index, (lower, peak_value, upper));
        }

        let mut sparse: Vec<Option<(f64, f64)>> = vec![None; point_count];
        for delta in tuple.deltas() {
            let index = delta.position as usize;
            if index < point_count {
                sparse[index] = Some((f64::from(delta.x_delta), f64::from(delta.y_delta)));
            }
        }
        let deltas =
            super::glyphs::interpolate_missing_public(&sparse, &points.coords, &points.end_pts);

        out.push(TupleVar { axes, deltas });
    }

    Ok(out)
}

/// Build a partially instanced font.
pub fn instantiate_partial(font: &FontRef, plans: &[AxisPlan]) -> Result<Vec<u8>, SliceError> {
    if font.glyf().is_ok() {
        return partial_glyf(font, plans);
    }
    if font.cff2().is_ok() {
        return partial_cff2(font, plans);
    }
    Err(SliceError::Unsupported(
        "This font has CFF 1.0 outlines, which are not variable and which this build \
         cannot rewrite. Only 'glyf' and 'CFF2' outlines can be instanced."
            .into(),
    ))
}

/// A partial instance of a `glyf` font: rebase every `gvar` tuple onto the new extents.
fn partial_glyf(font: &FontRef, plans: &[AxisPlan]) -> Result<Vec<u8>, SliceError> {
    let num_glyphs = font.maxp()?.num_glyphs();

    // Which input axes survive, and where they land in the output.
    let kept: Vec<usize> = (0..plans.len())
        .filter(|i| !plans[*i].is_pinned())
        .collect();
    let output_index: BTreeMap<usize, usize> = kept
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, new))
        .collect();

    let mut shapes: Vec<WGlyph> = Vec::with_capacity(num_glyphs as usize);
    let mut metrics: Vec<(u16, i16)> = Vec::with_capacity(num_glyphs as usize);
    let mut variations: Vec<GlyphVariations> = Vec::new();
    let mut any_variations = false;

    for gid in 0..num_glyphs {
        let gid = GlyphId::new(gid as u32);
        let mut points = read_glyph(font, gid)?;
        let point_count = points.coords.len();

        let tuples = read_tuples(font, gid, &points)?;
        let limited = limit_tuples(tuples, plans);
        let (default_deltas, remaining) = merge(limited, point_count);

        // Whatever no longer depends on an axis moves into the outline.
        for (point, delta) in points.coords.iter_mut().zip(&default_deltas) {
            point.0 += delta.0;
            point.1 += delta.1;
        }

        let phantoms = points.phantoms();
        let (pp1, pp2) = (phantoms[0].0, phantoms[1].0);

        let glyph = build_glyph(&points);
        let x_min = match &glyph {
            WGlyph::Simple(simple) => simple.bbox.x_min,
            _ => 0,
        };
        let advance = ot_round(pp2 - pp1).max(0) as u16;
        let lsb = ot_round(f64::from(x_min) - pp1) as i16;
        metrics.push((advance, lsb));
        shapes.push(glyph);

        // Whatever is left stays in gvar, expressed over the surviving axes.
        let deltas: Vec<GlyphDeltas> = remaining
            .iter()
            .filter_map(|var| {
                let mut tents = vec![Tent::new(F2Dot14::ZERO, None); kept.len()];
                for (index, (start, peak, end)) in &var.axes {
                    let Some(&slot) = output_index.get(index) else {
                        // A pinned axis should have been removed by the limiting; if one
                        // survived, the tuple cannot be expressed and is dropped rather
                        // than written against the wrong axis.
                        return None;
                    };
                    tents[slot] = Tent::new(
                        F2Dot14::from_f32(*peak as f32),
                        Some((
                            F2Dot14::from_f32(*start as f32),
                            F2Dot14::from_f32(*end as f32),
                        )),
                    );
                }

                let rounded: Vec<GlyphDelta> = var
                    .deltas
                    .iter()
                    .map(|(x, y)| GlyphDelta::required(ot_round(*x) as i16, ot_round(*y) as i16))
                    .collect();
                if rounded.iter().all(|d| d.x == 0 && d.y == 0) {
                    // A tuple that moves nothing is not worth writing.
                    return None;
                }
                Some(GlyphDeltas::new(tents, rounded))
            })
            .collect();

        // Every glyph needs an entry, including the ones with nothing to say: the
        // offsets array in gvar is positional, so a missing entry does not mean "this
        // glyph does not vary", it shifts every later glyph's data onto the wrong glyph.
        any_variations |= !deltas.is_empty();
        variations.push(GlyphVariations::new(gid, deltas));
    }

    // A composite's bounds depend on the glyphs it references, so they can only be filled
    // in once every glyph has been instanced. Without this the composites ship with a
    // (0, 0, 0, 0) box and `head`'s font-wide box, being the union of the per-glyph ones,
    // comes out too small: on the `composites` fixture restricted to wght=400:700, head
    // claimed xMax 550 for outlines reaching 620.
    super::glyphs::fill_composite_bboxes(&mut shapes);

    // Assemble.
    let mut builder = GlyfLocaBuilder::new();
    for glyph in &shapes {
        builder
            .add_glyph(glyph)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }
    let (glyf, loca, loca_format) = builder.build();

    let mut out = FontBuilder::new();
    out.add_table(&glyf)
        .map_err(|e| SliceError::Write(e.to_string()))?;
    out.add_table(&loca)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    if any_variations {
        let gvar = Gvar::new(variations, kept.len() as u16)
            .map_err(|e| SliceError::Write(e.to_string()))?;
        out.add_table(&gvar)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }

    finish_partial(&mut out, font, &metrics, Some(loca_format as i16), plans)
}

/// A partial instance of a `CFF2` font.
///
/// The blends and the CFF2 variation store are re-tented by [`super::cff2`]. What is left
/// here is `HVAR`, which for CFF2 is not optional the way it is for `glyf`: there are no
/// phantom points, so `HVAR` is the only place a glyph's advance varies, and dropping it
/// would leave every weight in the surviving range the same width.
fn partial_cff2(font: &FontRef, plans: &[AxisPlan]) -> Result<Vec<u8>, SliceError> {
    let instance = super::cff2::instantiate(font, &super::cff2::Request::Restricted(plans))?;

    let hmtx = font.hmtx()?;
    let num_glyphs = font.maxp()?.num_glyphs();
    let mut metrics: Vec<(u16, i16)> = Vec::with_capacity(num_glyphs as usize);

    let rebuilt_hvar = match font.hvar() {
        Err(_) => None,
        Ok(hvar) => {
            let store = hvar
                .item_variation_store()
                .map_err(|e| SliceError::Read(format!("the HVAR table is malformed: {e}")))?;
            let rebuilt = super::varstore::rebuild(&store, plans)?;
            // Whatever no longer varies is baked into `hmtx`, exactly as fontTools does
            // for a CFF2 font (`_instantiateVHVAR`, "CFF2 fonts need hmtx/vmtx updated
            // here").
            let mapping = hvar.advance_width_mapping().transpose().map_err(|e| {
                SliceError::Read(format!("the HVAR advance mapping is malformed: {e}"))
            })?;
            for gid in 0..num_glyphs {
                let index = match &mapping {
                    Some(map) => map.get(u32::from(gid)).map_err(|e| {
                        SliceError::Read(format!("the HVAR advance mapping is malformed: {e}"))
                    })?,
                    // With no mapping the glyph ID is the delta set index, in subtable 0.
                    None => read_fonts::tables::variations::DeltaSetIndex {
                        outer: 0,
                        inner: gid,
                    },
                };
                let delta = rebuilt
                    .default_deltas
                    .get(usize::from(index.outer))
                    .and_then(|row| row.get(usize::from(index.inner)))
                    .copied()
                    .unwrap_or(0.0);
                let gid = GlyphId::new(u32::from(gid));
                let advance = f64::from(hmtx.advance(gid).unwrap_or(0)) + delta;
                metrics.push((
                    ot_round(advance).max(0) as u16,
                    hmtx.side_bearing(gid).unwrap_or(0),
                ));
            }
            Some((hvar, rebuilt))
        }
    };
    if metrics.is_empty() {
        for gid in 0..num_glyphs {
            let gid = GlyphId::new(u32::from(gid));
            metrics.push((
                hmtx.advance(gid).unwrap_or(0),
                hmtx.side_bearing(gid).unwrap_or(0),
            ));
        }
    }

    let mut out = FontBuilder::new();
    out.add_raw(super::cff2::table::CFF2_TAG, instance.table);

    if let Some((hvar, rebuilt)) = rebuilt_hvar {
        if let Some(store) = rebuilt.store {
            // The delta-set index maps are copied across untouched: `varstore::rebuild`
            // keeps every (outer, inner) address, so everything pointing into the store
            // still points at the same row.
            let mut owned: write_fonts::tables::hvar::Hvar = hvar.to_owned_table();
            owned.item_variation_store = store.into();
            out.add_table(&owned)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }
    }

    finish_partial(&mut out, font, &metrics, None, plans)
}

/// Re-tent `GDEF`'s item variation store, and write its residual back into `GPOS`.
///
/// Returns the rebuilt `GDEF` and `GPOS`, or `None` when the font has no store, in which
/// case both tables are copied through untouched like any others.
///
/// Re-tenting splits every delta set into a part that still varies over the surviving
/// axes and a constant — the value at the *new* default location. For `HVAR` the constant
/// is baked into `hmtx`. `GDEF`'s has no single home: it belongs to the `GPOS` value
/// records, anchors and ligature carets that address the store, one constant each. See
/// `gpos_residual`.
///
/// The constant is zero whenever every other axis stays at its default, because deltas are
/// measured from the default master. Pinning an axis *away* from its default is what makes
/// it non-zero, and this used to refuse that case.
fn rebuilt_gdef(
    font: &FontRef,
    plans: &[AxisPlan],
) -> Result<Option<(write_fonts::tables::gdef::Gdef, Option<Gpos>)>, SliceError> {
    let Ok(gdef) = font.gdef() else {
        return Ok(None);
    };
    let Some(store) = gdef.item_var_store() else {
        return Ok(None);
    };
    let store = store.map_err(|e| SliceError::Read(format!("GDEF is malformed: {e}")))?;

    let rebuilt = super::varstore::rebuild(&store, plans)?;
    let mut owned: write_fonts::tables::gdef::Gdef = gdef.to_owned_table();

    let residual =
        super::gpos_residual::Residual::new(&rebuilt.default_deltas, rebuilt.store.is_none());

    // Only touch GPOS when there is something to say. Rewriting it otherwise would
    // re-serialise every lookup in the font for no change, and re-serialising is where
    // subtle differences creep in.
    let gpos = if residual.is_noop() {
        None
    } else {
        match font.gpos() {
            Ok(gpos) => {
                let mut owned_gpos: Gpos = gpos.to_owned_table();
                super::gpos_residual::apply(&mut owned_gpos, &mut owned, &residual);
                Some(owned_gpos)
            }
            Err(_) => {
                // A store with no GPOS to modify: the carets in GDEF are still corrected
                // by the call below, which is the only other thing that can address it.
                let mut empty = Gpos::default();
                super::gpos_residual::apply(&mut empty, &mut owned, &residual);
                None
            }
        }
    };

    owned.item_var_store = rebuilt.store.into();
    Ok(Some((owned, gpos)))
}

/// Everything a partial instance needs once its outlines and metrics are settled.
fn finish_partial<'a>(
    out: &mut FontBuilder<'a>,
    font: &FontRef<'a>,
    metrics: &[(u16, i16)],
    loca_format: Option<i16>,
    plans: &[AxisPlan],
) -> Result<Vec<u8>, SliceError> {
    out.add_table(&super::statics::build_hmtx_public(metrics))
        .map_err(|e| SliceError::Write(e.to_string()))?;

    let mut head: write_fonts::tables::head::Head = font.head()?.to_owned_table();
    if let Some(format) = loca_format {
        head.index_to_loc_format = format;
    }
    out.add_table(&head)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    let mut hhea: write_fonts::tables::hhea::Hhea = font.hhea()?.to_owned_table();
    hhea.number_of_h_metrics = super::statics::long_metric_count_public(metrics) as u16;
    hhea.advance_width_max = write_fonts::types::UfWord::new(
        metrics
            .iter()
            .map(|(advance, _)| *advance)
            .max()
            .unwrap_or(0),
    );
    out.add_table(&hhea)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    out.add_table(&build_fvar(font, plans)?)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    if let Some(avar) = build_avar(font, plans)? {
        out.add_table(&avar)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }

    if let Some(stat) = build_stat(font, plans)? {
        out.add_table(&stat)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }

    // GDEF's item variation store is what variable kerning and variable anchors are
    // measured against, and GPOS reaches into it by (outer, inner) address. Re-tenting it
    // onto the narrowed axes is the same operation HVAR needs, so it is the same code;
    // `varstore::rebuild` preserves every address, which is what keeps the delta-set
    // index maps and the GPOS references pointing at the row they always pointed at.
    if let Some((gdef, gpos)) = rebuilt_gdef(font, plans)? {
        out.add_table(&gdef)
            .map_err(|e| SliceError::Write(e.to_string()))?;
        // Only present when the residual was non-zero; otherwise GPOS is copied through
        // by `copy_remaining_tables` exactly as it arrived.
        if let Some(gpos) = gpos {
            out.add_table(&gpos)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }
    }

    // MVAR is applied at the new default and then dropped; see the module docs.
    let location = super::normalize::NormalizedLocation {
        coords: plans
            .iter()
            .map(|plan| {
                if plan.is_pinned() {
                    plan.normalized.default
                } else {
                    0.0
                }
            })
            .collect(),
        tags: plans
            .iter()
            .map(|plan| Tag::new_checked(plan.spec.tag.as_bytes()).unwrap_or(Tag::new(b"    ")))
            .collect(),
    };
    let adjustments = super::mvar::metric_adjustments(font, &location);
    if let Ok(os2) = font.os2() {
        let mut os2: write_fonts::tables::os2::Os2 = os2.to_owned_table();
        super::mvar::apply_to_os2(&mut os2, &adjustments);
        out.add_table(&os2)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }
    // Only when MVAR actually has something to say about it; see `finalize::owned_post`
    // for why converting this table is not free of hazards.
    if !adjustments.is_empty() {
        if let Some(mut post) = crate::finalize::owned_post(font) {
            super::mvar::apply_to_post(&mut post, &adjustments);
            out.add_table(&post)
                .map_err(|e| SliceError::Write(e.to_string()))?;
        }
    }

    // Tables whose contents describe the old axis space and are rebuilt or dropped here.
    const REPLACED: &[Tag] = &[
        Tag::new(b"fvar"),
        Tag::new(b"avar"),
        Tag::new(b"gvar"),
        Tag::new(b"cvar"),
        Tag::new(b"STAT"),
        // Dropped: for glyf outlines the advance variation lives in the gvar phantom
        // points, which have been rebased along with everything else. A CFF2 font has
        // no phantom points, so `partial_cff2` rebuilds HVAR and adds it above, which
        // makes the builder already hold it by the time this runs.
        Tag::new(b"HVAR"),
        Tag::new(b"VVAR"),
        // Applied above, then dropped.
        Tag::new(b"MVAR"),
    ];
    super::statics::copy_remaining_tables(out, font, REPLACED);

    Ok(std::mem::take(out).build())
}

/// Rebuild `fvar` over the surviving axes.
fn build_fvar(font: &FontRef, plans: &[AxisPlan]) -> Result<Fvar, SliceError> {
    let source = font.fvar()?;

    let mut axes = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        if plan.is_pinned() {
            continue;
        }
        let record = source
            .axes()?
            .get(index)
            .ok_or_else(|| SliceError::Read("fvar axis disappeared while instancing".into()))?;
        let (min, default, max) = plan.output_extent();
        axes.push(VariationAxisRecord {
            axis_tag: record.axis_tag(),
            min_value: Fixed::from_f64(min),
            default_value: Fixed::from_f64(default),
            max_value: Fixed::from_f64(max),
            flags: record.flags(),
            axis_name_id: record.axis_name_id(),
        });
    }

    // Named instances survive only if they sit at the pinned location and inside the
    // new ranges; otherwise they would name a place the font can no longer reach.
    let mut instances = Vec::new();
    for instance in source.instances()?.iter() {
        let Ok(instance) = instance else { continue };
        let coords = instance.coordinates;

        let mut keep = true;
        let mut kept_coords = Vec::new();
        for (index, plan) in plans.iter().enumerate() {
            let value = coords
                .get(index)
                .map(|v| v.get().to_f64())
                .unwrap_or(plan.spec.default);
            match plan.limit {
                AxisLimit::Pin(pinned) => {
                    if value != pinned {
                        keep = false;
                        break;
                    }
                }
                AxisLimit::Range { min, max, .. } => {
                    if value < min || value > max {
                        keep = false;
                        break;
                    }
                    kept_coords.push(Fixed::from_f64(value));
                }
                AxisLimit::Full => kept_coords.push(Fixed::from_f64(value)),
            }
        }
        if keep {
            instances.push(InstanceRecord {
                subfamily_name_id: instance.subfamily_name_id,
                flags: instance.flags,
                coordinates: kept_coords,
                post_script_name_id: instance.post_script_name_id,
            });
        }
    }

    Ok(Fvar::new(AxisInstanceArrays::new(axes, instances)))
}

/// Rebuild `avar` over the surviving axes.
///
/// A segment map is a function on normalized coordinates, and the normalized coordinates
/// have just changed meaning, so each retained mapping has to be expressed against the
/// new extents: its input renormalized through the axis's new range, its output through
/// the range that same map produces.
fn build_avar(font: &FontRef, plans: &[AxisPlan]) -> Result<Option<Avar>, SliceError> {
    let Some((maps, has_v2)) = segment_maps(font) else {
        return Ok(None);
    };
    if has_v2 {
        return Err(SliceError::Unsupported(
            "This font has an avar version 2 table, whose extra variation store this \
             build cannot rewrite. Pin every axis instead, which produces a static \
             instance."
                .into(),
        ));
    }

    let mut out = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        if plan.is_pinned() {
            continue;
        }
        let Some(map) = maps.get(index) else {
            out.push(SegmentMaps::new(identity_segment_map()));
            continue;
        };
        if plan.is_untouched() || map.len() < 3 {
            out.push(SegmentMaps::new(
                map.iter()
                    .map(|(from, to)| AxisValueMap {
                        from_coordinate: F2Dot14::from_f32(*from as f32),
                        to_coordinate: F2Dot14::from_f32(*to as f32),
                    })
                    .collect(),
            ));
            continue;
        }

        // The axis range *before* avar, which is what the map's inputs are stated in.
        let plain = AxisTriple::with_distances(
            quantize(normalize_axis(
                match plan.limit {
                    AxisLimit::Range { min, .. } => min,
                    _ => plan.spec.min,
                },
                &plan.spec,
            )),
            quantize(normalize_axis(plan.spec.default, &plan.spec)),
            quantize(normalize_axis(
                match plan.limit {
                    AxisLimit::Range { max, .. } => max,
                    _ => plan.spec.max,
                },
                &plan.spec,
            )),
            plan.spec.default - plan.spec.min,
            plan.spec.max - plan.spec.default,
        );

        // And the range the map turns that into, which is what its outputs are in.
        let mapped = AxisTriple::with_distances(
            quantize(apply_segment_map(plain.min, map)),
            quantize(apply_segment_map(plain.default, map)),
            quantize(apply_segment_map(plain.max, map)),
            plain.distance_negative,
            plain.distance_positive,
        );

        let mut pairs: BTreeMap<i16, i16> = BTreeMap::new();
        for (from, to) in map {
            if *from < plain.min || *from > plain.max {
                continue;
            }
            let new_from = quantize(plain.renormalize_value(*from));
            let new_to = quantize(mapped.renormalize_value(*to));
            pairs.insert(
                F2Dot14::from_f32(new_from as f32).to_bits(),
                F2Dot14::from_f32(new_to as f32).to_bits(),
            );
        }
        // The three fixed points every segment map must contain.
        for (from, to) in [(-1.0f64, -1.0f64), (0.0, 0.0), (1.0, 1.0)] {
            pairs.insert(
                F2Dot14::from_f32(from as f32).to_bits(),
                F2Dot14::from_f32(to as f32).to_bits(),
            );
        }

        out.push(SegmentMaps::new(
            pairs
                .into_iter()
                .map(|(from, to)| AxisValueMap {
                    from_coordinate: F2Dot14::from_bits(from),
                    to_coordinate: F2Dot14::from_bits(to),
                })
                .collect(),
        ));
    }

    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(Avar::new(out)))
}

fn identity_segment_map() -> Vec<AxisValueMap> {
    [(-1.0f32, -1.0f32), (0.0, 0.0), (1.0, 1.0)]
        .into_iter()
        .map(|(from, to)| AxisValueMap {
            from_coordinate: F2Dot14::from_f32(from),
            to_coordinate: F2Dot14::from_f32(to),
        })
        .collect()
}

/// Filter `STAT`'s axis values to those the output can still reach.
///
/// fontTools keeps every design axis record, including the pinned ones, and drops only
/// the axis *values* that fall outside the new limits. That is deliberate and worth
/// copying: a static instance's `STAT` is how it declares "this is Casual 1, Weight
/// 1000", and stripping the pinned axes would throw exactly that away. An earlier
/// version here removed them and had to renumber every axis value's index, which was
/// both more code and wrong.
pub fn build_stat(
    font: &FontRef,
    plans: &[AxisPlan],
) -> Result<Option<write_fonts::tables::stat::Stat>, SliceError> {
    use write_fonts::tables::stat as wstat;

    let Ok(source) = font.stat() else {
        return Ok(None);
    };
    let Ok(design_axes) = source.design_axes() else {
        return Ok(None);
    };

    let records: Vec<wstat::AxisRecord> = design_axes
        .iter()
        .map(|axis| wstat::AxisRecord {
            axis_tag: axis.axis_tag(),
            axis_name_id: axis.axis_name_id(),
            axis_ordering: axis.axis_ordering(),
        })
        .collect();
    if records.is_empty() {
        return Ok(None);
    }

    // The extent each axis ends up with, by tag, for testing axis values against.
    let extent = |tag: Tag| -> Option<(f64, f64)> {
        plans.iter().find_map(|plan| {
            if Tag::new_checked(plan.spec.tag.as_bytes()).ok()? == tag {
                let (min, _, max) = plan.output_extent();
                Some((min, max))
            } else {
                None
            }
        })
    };
    // An `AxisValue` names its axis by index into the design-axis array. An index past
    // the end of that array does not describe anything, so the record is dropped rather
    // than copied through. Real fonts do carry this: `OpenSansCondensed-Italic[wght].ttf`
    // in google/fonts' axisregistry test data has a two-axis STAT with a record naming
    // axis 2, and fontTools 4.62.1 raises `IndexError: list index out of range` on it.
    // Surviving where the reference crashes is only worth something if the corruption is
    // removed rather than passed on.
    let outside = |axis_index: u16, value: f64| -> bool {
        let Some(axis) = design_axes.get(axis_index as usize) else {
            return true;
        };
        match extent(axis.axis_tag()) {
            Some((min, max)) => value < min || value > max,
            // An axis STAT describes that fvar never had is left alone.
            None => false,
        }
    };

    let mut values: Vec<wstat::AxisValue> = Vec::new();
    if let Some(Ok(subtables)) = source.offset_to_axis_values() {
        for subtable in subtables.axis_values().iter().flatten() {
            if let Some(value) = keep_axis_value(&subtable, &outside) {
                values.push(value);
            }
        }
    }

    let fallback = source
        .elided_fallback_name_id()
        // A missing elidedFallbackNameID means STAT 1.0, which predates the field;
        // nameID 2 (Subfamily) is the conventional stand-in.
        .unwrap_or(write_fonts::types::NameId::new(2));
    Ok(Some(wstat::Stat::new(records, values, fallback)))
}

/// Copy one STAT axis value across, unless the location it names is out of reach.
fn keep_axis_value(
    value: &read_fonts::tables::stat::AxisValue,
    outside: &dyn Fn(u16, f64) -> bool,
) -> Option<write_fonts::tables::stat::AxisValue> {
    use read_fonts::tables::stat::AxisValue as R;
    use write_fonts::tables::stat as w;

    match value {
        R::Format1(v) => (!outside(v.axis_index(), v.value().to_f64())).then(|| {
            w::AxisValue::format_1(v.axis_index(), v.flags(), v.value_name_id(), v.value())
        }),
        R::Format2(v) => (!outside(v.axis_index(), v.nominal_value().to_f64())).then(|| {
            w::AxisValue::format_2(
                v.axis_index(),
                v.flags(),
                v.value_name_id(),
                v.nominal_value(),
                v.range_min_value(),
                v.range_max_value(),
            )
        }),
        R::Format3(v) => (!outside(v.axis_index(), v.value().to_f64())).then(|| {
            w::AxisValue::format_3(
                v.axis_index(),
                v.flags(),
                v.value_name_id(),
                v.value(),
                v.linked_value(),
            )
        }),
        R::Format4(v) => {
            // A format 4 record names one point across several axes at once. If any of
            // them is out of reach -- or names an axis the design-axis array does not
            // have -- the record no longer describes a location the font has, so the
            // whole record goes rather than being quietly reduced.
            let records: Vec<w::AxisValueRecord> = v
                .axis_values()
                .iter()
                .map(|rec| w::AxisValueRecord {
                    axis_index: rec.axis_index(),
                    value: rec.value(),
                })
                .collect();
            let any_outside = v
                .axis_values()
                .iter()
                .any(|rec| outside(rec.axis_index(), rec.value().to_f64()));
            (!any_outside && !records.is_empty())
                .then(|| w::AxisValue::format_4(v.flags(), v.value_name_id(), records))
        }
    }
}
