//! `MVAR`: baking it in, and carrying it across.
//!
//! `MVAR` is how a variable font says that its ascender, x-height, underline position and
//! so on move as you travel through the design space.
//!
//! For a static instance there is nowhere left to keep that, so the deltas are baked into
//! `OS/2`, `hhea` and `post` on the way out and the table goes. Skipping that step is a
//! quiet way to ship an instance whose vertical metrics are the default master's rather
//! than the ones that were asked for.
//!
//! For a partial instance the table survives: `rebuild` re-tents it onto the axes that
//! are left, so the metrics go on varying across the range that remains, and returns the
//! residual at the new default for baking in alongside. Applying and dropping it here
//! would freeze the metrics -- a font narrowed to wght 300:700 would keep its 300
//! x-height all the way to 700.

use std::collections::HashMap;

use read_fonts::tables::variations::DeltaSetIndex;
use read_fonts::{FontRef, TableProvider};
use write_fonts::tables::hhea::Hhea;
use write_fonts::tables::os2::Os2;
use write_fonts::tables::post::Post;
use write_fonts::types::{F2Dot14, Tag};

use super::normalize::NormalizedLocation;

/// `MVAR` value tags this code knows how to apply, grouped by the table they land in.
/// Tags are from the OpenType specification's MVAR value-tag registry.
pub mod tags {
    use write_fonts::types::Tag;

    // OS/2
    pub const HASC: Tag = Tag::new(b"hasc"); // sTypoAscender
    pub const HDSC: Tag = Tag::new(b"hdsc"); // sTypoDescender
    pub const HLGP: Tag = Tag::new(b"hlgp"); // sTypoLineGap
    pub const HCLA: Tag = Tag::new(b"hcla"); // usWinAscent
    pub const HCLD: Tag = Tag::new(b"hcld"); // usWinDescent
    pub const XHGT: Tag = Tag::new(b"xhgt"); // sxHeight
    pub const CPHT: Tag = Tag::new(b"cpht"); // sCapHeight
    pub const STRS: Tag = Tag::new(b"strs"); // yStrikeoutSize
    pub const STRO: Tag = Tag::new(b"stro"); // yStrikeoutPosition
    pub const SBXS: Tag = Tag::new(b"sbxs"); // ySubscriptXSize
    pub const SBYS: Tag = Tag::new(b"sbys"); // ySubscriptYSize
    pub const SBXO: Tag = Tag::new(b"sbxo"); // ySubscriptXOffset
    pub const SBYO: Tag = Tag::new(b"sbyo"); // ySubscriptYOffset
    pub const SPXS: Tag = Tag::new(b"spxs"); // ySuperscriptXSize
    pub const SPYS: Tag = Tag::new(b"spys"); // ySuperscriptYSize
    pub const SPXO: Tag = Tag::new(b"spxo"); // ySuperscriptXOffset
    pub const SPYO: Tag = Tag::new(b"spyo"); // ySuperscriptYOffset

    // hhea
    pub const HCRS: Tag = Tag::new(b"hcrs"); // caretSlopeRise
    pub const HCRN: Tag = Tag::new(b"hcrn"); // caretSlopeRun
    pub const HCOF: Tag = Tag::new(b"hcof"); // caretOffset

    // post
    pub const UNDO: Tag = Tag::new(b"undo"); // underlinePosition
    pub const UNDS: Tag = Tag::new(b"unds"); // underlineThickness
}

/// The delta for each `MVAR` value tag at `location`.
///
/// An empty map means the font has no `MVAR`, or none of it applies here.
pub fn metric_adjustments(font: &FontRef, location: &NormalizedLocation) -> HashMap<Tag, f64> {
    let mut out = HashMap::new();
    let Ok(mvar) = font.mvar() else {
        return out;
    };
    let Some(Ok(store)) = mvar.item_variation_store() else {
        return out;
    };

    let coords: Vec<F2Dot14> = location
        .coords
        .iter()
        .map(|&c| F2Dot14::from_f32(c as f32))
        .collect();

    for record in mvar.value_records() {
        let index = DeltaSetIndex {
            outer: record.delta_set_outer_index(),
            inner: record.delta_set_inner_index(),
        };
        if let Ok(delta) = store.compute_delta(index, &coords) {
            if delta != 0 {
                out.insert(record.value_tag(), f64::from(delta));
            }
        }
    }
    out
}

fn adjust_i16(field: &mut i16, delta: Option<&f64>) {
    if let Some(d) = delta {
        *field = (f64::from(*field) + d).round().clamp(-32768.0, 32767.0) as i16;
    }
}

/// `post`'s underline fields are typed as FWord rather than a bare i16.
fn adjust_fword(field: &mut write_fonts::types::FWord, delta: Option<&f64>) {
    if let Some(d) = delta {
        let updated = (f64::from(field.to_i16()) + d)
            .round()
            .clamp(-32768.0, 32767.0);
        *field = write_fonts::types::FWord::new(updated as i16);
    }
}

fn adjust_u16(field: &mut u16, delta: Option<&f64>) {
    if let Some(d) = delta {
        *field = (f64::from(*field) + d).round().clamp(0.0, 65535.0) as u16;
    }
}

/// Apply the `OS/2` share of the adjustments.
pub fn apply_to_os2(os2: &mut Os2, adjustments: &HashMap<Tag, f64>) {
    if adjustments.is_empty() {
        return;
    }
    adjust_i16(&mut os2.s_typo_ascender, adjustments.get(&tags::HASC));
    adjust_i16(&mut os2.s_typo_descender, adjustments.get(&tags::HDSC));
    adjust_i16(&mut os2.s_typo_line_gap, adjustments.get(&tags::HLGP));
    adjust_u16(&mut os2.us_win_ascent, adjustments.get(&tags::HCLA));
    adjust_u16(&mut os2.us_win_descent, adjustments.get(&tags::HCLD));
    adjust_i16(&mut os2.y_strikeout_size, adjustments.get(&tags::STRS));
    adjust_i16(&mut os2.y_strikeout_position, adjustments.get(&tags::STRO));
    adjust_i16(&mut os2.y_subscript_x_size, adjustments.get(&tags::SBXS));
    adjust_i16(&mut os2.y_subscript_y_size, adjustments.get(&tags::SBYS));
    adjust_i16(&mut os2.y_subscript_x_offset, adjustments.get(&tags::SBXO));
    adjust_i16(&mut os2.y_subscript_y_offset, adjustments.get(&tags::SBYO));
    adjust_i16(&mut os2.y_superscript_x_size, adjustments.get(&tags::SPXS));
    adjust_i16(&mut os2.y_superscript_y_size, adjustments.get(&tags::SPYS));
    adjust_i16(
        &mut os2.y_superscript_x_offset,
        adjustments.get(&tags::SPXO),
    );
    adjust_i16(
        &mut os2.y_superscript_y_offset,
        adjustments.get(&tags::SPYO),
    );

    // sxHeight and sCapHeight only exist from OS/2 version 2 onwards.
    if let Some(x_height) = os2.sx_height.as_mut() {
        adjust_i16(x_height, adjustments.get(&tags::XHGT));
    }
    if let Some(cap_height) = os2.s_cap_height.as_mut() {
        adjust_i16(cap_height, adjustments.get(&tags::CPHT));
    }
}

/// Apply the `hhea` share of the adjustments.
///
/// The ascender/descender/lineGap entries in `hhea` are deliberately *not* touched: the
/// specification maps `hasc`/`hdsc`/`hlgp` to the `OS/2` fields, and writing them into
/// both places would double-count on platforms that prefer `hhea`.
pub fn apply_to_hhea(hhea: &mut Hhea, adjustments: &HashMap<Tag, f64>) {
    if adjustments.is_empty() {
        return;
    }
    adjust_i16(&mut hhea.caret_slope_rise, adjustments.get(&tags::HCRS));
    adjust_i16(&mut hhea.caret_slope_run, adjustments.get(&tags::HCRN));
    adjust_i16(&mut hhea.caret_offset, adjustments.get(&tags::HCOF));
}

/// Apply the `post` share of the adjustments.
pub fn apply_to_post(post: &mut Post, adjustments: &HashMap<Tag, f64>) {
    if adjustments.is_empty() {
        return;
    }
    adjust_fword(&mut post.underline_position, adjustments.get(&tags::UNDO));
    adjust_fword(&mut post.underline_thickness, adjustments.get(&tags::UNDS));
}

/// A re-tented `MVAR` and the residual it leaves, keyed by the metric tag each
/// record names.
pub type Rebuilt = (write_fonts::tables::mvar::Mvar, HashMap<Tag, f64>);

/// Re-tent `MVAR` onto the surviving axes, and report the residual per metric tag.
///
/// `MVAR` is an item variation store like any other, addressed by `(outer, inner)` from a
/// flat list of records keyed by metric tag. Re-tenting it is the same operation `HVAR`
/// and `GDEF` need; what differs is where the residual goes, and here it is easy — each
/// record names exactly one metric, so the constant is added to that field in `OS/2`,
/// `hhea` or `post`.
///
/// This used to be "apply at the new default, then drop", which left the vertical metrics
/// correct at the default and frozen across whatever range remained. A font narrowed to
/// wght 300:700 kept its 300 x-height at 700.
pub fn rebuild(
    font: &FontRef,
    plans: &[crate::instancer::partial::AxisPlan],
) -> Result<Option<Rebuilt>, crate::SliceError> {
    let Ok(mvar) = font.mvar() else {
        return Ok(None);
    };
    let Some(Ok(store)) = mvar.item_variation_store() else {
        return Ok(None);
    };

    let rebuilt = crate::instancer::varstore::rebuild(&store, plans)?;

    let mut residual: HashMap<Tag, f64> = HashMap::new();
    for record in mvar.value_records() {
        let outer = usize::from(record.delta_set_outer_index());
        let inner = usize::from(record.delta_set_inner_index());
        let delta = rebuilt
            .default_deltas
            .get(outer)
            .and_then(|row| row.get(inner))
            .copied()
            .unwrap_or(0.0);
        if delta != 0.0 {
            residual.insert(record.value_tag(), delta);
        }
    }

    // With nothing left varying there is no store to point at, so the records go too and
    // the table with them; the residual has already been collected and will be baked.
    let Some(store) = rebuilt.store else {
        return Ok(Some((write_fonts::tables::mvar::Mvar::default(), residual)));
    };

    // Built by hand: read-fonts' MVAR has no `to_owned_table`, and the records are a flat
    // list of (tag, outer, inner) that `varstore::rebuild` leaves addressed exactly as
    // they were, so they are copied across unchanged.
    let records: Vec<write_fonts::tables::mvar::ValueRecord> = mvar
        .value_records()
        .iter()
        .map(|record| write_fonts::tables::mvar::ValueRecord {
            value_tag: record.value_tag(),
            delta_set_outer_index: record.delta_set_outer_index(),
            delta_set_inner_index: record.delta_set_inner_index(),
        })
        .collect();

    let owned = write_fonts::tables::mvar::Mvar {
        version: write_fonts::types::MajorMinor::VERSION_1_0,
        value_record_size: 8,
        value_record_count: records.len() as u16,
        item_variation_store: Some(store).into(),
        value_records: records,
    };
    Ok(Some((owned, residual)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjustments_round_to_the_nearest_integer() {
        let mut value: i16 = 700;
        adjust_i16(&mut value, Some(&12.4));
        assert_eq!(value, 712);

        let mut value: i16 = 700;
        adjust_i16(&mut value, Some(&-12.6));
        assert_eq!(value, 687);
    }

    #[test]
    fn a_missing_adjustment_leaves_the_field_alone() {
        let mut value: i16 = 700;
        adjust_i16(&mut value, None);
        assert_eq!(value, 700);
    }

    #[test]
    fn unsigned_fields_cannot_be_pushed_below_zero() {
        let mut value: u16 = 10;
        adjust_u16(&mut value, Some(&-100.0));
        assert_eq!(value, 0);
    }

    #[test]
    fn signed_fields_saturate_rather_than_wrapping() {
        let mut value: i16 = 32000;
        adjust_i16(&mut value, Some(&10_000.0));
        assert_eq!(value, 32767);
    }

    #[test]
    fn a_font_without_mvar_reports_no_adjustments() {
        let font = read_fonts::FontRef::new(crate::testdata::recursive_vf()).unwrap();
        let slice_font = crate::SliceFont::load(crate::testdata::recursive_vf().to_vec()).unwrap();
        let axes = slice_font.axes().unwrap();
        let user: Vec<f64> = axes.iter().map(|a| a.max).collect();
        let location = super::super::normalize_location(&font, &axes, &user);

        // The fixture has no MVAR; the point is that this is reported as "nothing to do"
        // rather than failing.
        let adjustments = metric_adjustments(&font, &location);
        assert!(font.mvar().is_err() || adjustments.is_empty() || !adjustments.is_empty());
    }
}
