//! Writing a variation store's leftover back into the values it was modifying.
//!
//! When an item variation store is re-tented onto a narrower design space, each delta set
//! splits in two: a part that still varies over the surviving axes, which stays in the
//! store, and a constant — the value at the *new* default location, which
//! `varstore::rebuild` reports as `default_deltas`.
//!
//! For `HVAR` that constant is added to `hmtx` and the matter ends. `GDEF`'s store has no
//! such single home: the numbers it modifies are `GPOS` value records and anchors spread
//! across every lookup in the font, and `GDEF`'s own ligature caret values. Each of them
//! carries a device pointer holding a `(outer, inner)` address into the store, and each
//! needs its own constant added to the number sitting beside that pointer.
//!
//! It is zero whenever every axis other than the restricted one stays at its default,
//! because deltas are measured from the default master. Pinning an axis *away* from its
//! default is what makes it non-zero: the kerning at `wdth=50` is not the kerning at
//! `wdth=100`, and once `wdth` is gone the difference has to live in the value itself.
//!
//! The traversal below mirrors `write-fonts`' own `RemapVarStore`, which walks exactly
//! these structures to renumber indices. It cannot be reused, because it rewrites the
//! pointer and never touches the value beside it, which is the entire job here.

use write_fonts::tables::gdef::{CaretValue, Gdef};
use write_fonts::tables::gpos::{
    AnchorTable, Gpos, MarkArray, PairPos, PairPosFormat1, PairPosFormat2, PositionLookup,
    SinglePos, SinglePosFormat1, SinglePosFormat2, ValueRecord,
};
use write_fonts::tables::layout::DeviceOrVariationIndex;

/// The constant part of each delta set, indexed `[outer][inner]`.
pub struct Residual<'a> {
    deltas: &'a [Vec<f64>],
    /// True when nothing varies any more, so the device pointers have to go as well as
    /// their values being corrected. A pointer into a store that is not there is a
    /// dangling reference, and a shaper following it reads whatever happens to be at that
    /// offset.
    store_is_gone: bool,
}

impl<'a> Residual<'a> {
    pub fn new(deltas: &'a [Vec<f64>], store_is_gone: bool) -> Self {
        Self {
            deltas,
            store_is_gone,
        }
    }

    /// True when there is nothing to write back and nothing to remove.
    pub fn is_noop(&self) -> bool {
        !self.store_is_gone
            && self
                .deltas
                .iter()
                .all(|row| row.iter().all(|d| d.abs() < 0.5))
    }

    fn get(&self, index: &DeviceOrVariationIndex) -> Option<i16> {
        let DeviceOrVariationIndex::VariationIndex(index) = index else {
            // A real `Device` table is hinting-era data with no relationship to the
            // variation store, and is left exactly as it is.
            return None;
        };
        let row = self.deltas.get(usize::from(index.delta_set_outer_index))?;
        let delta = row.get(usize::from(index.delta_set_inner_index))?;
        Some(ot_round(*delta))
    }
}

/// Round half away from zero, which is what OpenType specifies for delta application.
fn ot_round(value: f64) -> i16 {
    (value + 0.5)
        .floor()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Add `residual` into every value in `gpos` that points at the store, and into
/// `gdef`'s ligature carets.
pub fn apply(gpos: &mut Gpos, gdef: &mut Gdef, residual: &Residual) {
    for lookup in &mut gpos.lookup_list.lookups {
        lookup_of(lookup, residual);
    }
    if let Some(carets) = gdef.lig_caret_list.as_mut() {
        for entry in carets.lig_glyphs.iter_mut() {
            for caret in entry.caret_values.iter_mut() {
                let CaretValue::Format3(format3) = &mut **caret else {
                    continue;
                };
                if let Some(delta) = residual.get(&format3.device) {
                    format3.coordinate += delta;
                }
                if residual.store_is_gone {
                    // Format 3 *is* "a coordinate plus a device"; with the store gone
                    // there is no device to point at, so the caret becomes the plain
                    // coordinate it now amounts to rather than keeping a dangling one.
                    **caret = CaretValue::format_1(format3.coordinate);
                }
            }
        }
    }
}

fn lookup_of(lookup: &mut PositionLookup, residual: &Residual) {
    match lookup {
        PositionLookup::Single(inner) => {
            for subtable in inner.subtables.iter_mut() {
                match &mut **subtable {
                    SinglePos::Format1(SinglePosFormat1 { value_record, .. }) => {
                        value_of(value_record, residual)
                    }
                    SinglePos::Format2(SinglePosFormat2 { value_records, .. }) => {
                        for record in value_records {
                            value_of(record, residual);
                        }
                    }
                }
            }
        }
        PositionLookup::Pair(inner) => {
            for subtable in inner.subtables.iter_mut() {
                match &mut **subtable {
                    PairPos::Format1(PairPosFormat1 { pair_sets, .. }) => {
                        for set in pair_sets {
                            for record in &mut set.pair_value_records {
                                value_of(&mut record.value_record1, residual);
                                value_of(&mut record.value_record2, residual);
                            }
                        }
                    }
                    PairPos::Format2(PairPosFormat2 { class1_records, .. }) => {
                        for class1 in class1_records {
                            for class2 in &mut class1.class2_records {
                                value_of(&mut class2.value_record1, residual);
                                value_of(&mut class2.value_record2, residual);
                            }
                        }
                    }
                }
            }
        }
        PositionLookup::Cursive(inner) => {
            for subtable in inner.subtables.iter_mut() {
                for entry in &mut subtable.entry_exit_record {
                    for anchor in [entry.entry_anchor.as_mut(), entry.exit_anchor.as_mut()]
                        .into_iter()
                        .flatten()
                    {
                        anchor_of(anchor, residual);
                    }
                }
            }
        }
        PositionLookup::MarkToBase(inner) => {
            for subtable in inner.subtables.iter_mut() {
                mark_array_of(&mut subtable.mark_array, residual);
                {
                    let bases = &mut *subtable.base_array;
                    for record in bases.base_records.iter_mut() {
                        for slot in record.base_anchors.iter_mut() {
                            if let Some(anchor) = slot.as_mut() {
                                anchor_of(anchor, residual);
                            }
                        }
                    }
                }
            }
        }
        PositionLookup::MarkToLig(inner) => {
            for subtable in inner.subtables.iter_mut() {
                mark_array_of(&mut subtable.mark_array, residual);
                {
                    let ligatures = &mut *subtable.ligature_array;
                    for attach in ligatures.ligature_attaches.iter_mut() {
                        for component in attach.component_records.iter_mut() {
                            for slot in component.ligature_anchors.iter_mut() {
                                if let Some(anchor) = slot.as_mut() {
                                    anchor_of(anchor, residual);
                                }
                            }
                        }
                    }
                }
            }
        }
        PositionLookup::MarkToMark(inner) => {
            for subtable in inner.subtables.iter_mut() {
                mark_array_of(&mut subtable.mark1_array, residual);
                {
                    let marks = &mut *subtable.mark2_array;
                    for record in marks.mark2_records.iter_mut() {
                        for slot in record.mark2_anchors.iter_mut() {
                            if let Some(anchor) = slot.as_mut() {
                                anchor_of(anchor, residual);
                            }
                        }
                    }
                }
            }
        }
        // Contextual and chained-contextual lookups position by *invoking* other lookups
        // rather than by carrying values of their own, so the values they reach are
        // corrected when those lookups are visited. An extension lookup wraps one of the
        // above; the inner one is in the same lookup list and is reached on its own turn.
        PositionLookup::Contextual(_)
        | PositionLookup::ChainContextual(_)
        | PositionLookup::Extension(_) => {}
    }
}

fn mark_array_of(array: &mut MarkArray, residual: &Residual) {
    for record in array.mark_records.iter_mut() {
        anchor_of(&mut record.mark_anchor, residual);
    }
}

fn anchor_of(anchor: &mut AnchorTable, residual: &Residual) {
    let AnchorTable::Format3(anchor) = anchor else {
        // Formats 1 and 2 carry no device tables, so there is nothing pointing at the
        // store and nothing to correct.
        return;
    };
    if let Some(device) = anchor.x_device.as_ref() {
        if let Some(delta) = residual.get(device) {
            anchor.x_coordinate += delta;
        }
    }
    if let Some(device) = anchor.y_device.as_ref() {
        if let Some(delta) = residual.get(device) {
            anchor.y_coordinate += delta;
        }
    }
    if residual.store_is_gone {
        anchor.x_device = Default::default();
        anchor.y_device = Default::default();
    }
}

fn value_of(record: &mut ValueRecord, residual: &Residual) {
    // Each device belongs to exactly one of the four numbers, and correcting the wrong
    // one would move a glyph sideways instead of changing its advance.
    for (device, value) in [
        (&record.x_placement_device, &mut record.x_placement),
        (&record.y_placement_device, &mut record.y_placement),
        (&record.x_advance_device, &mut record.x_advance),
        (&record.y_advance_device, &mut record.y_advance),
    ] {
        let Some(device) = device.as_ref() else {
            continue;
        };
        let Some(delta) = residual.get(device) else {
            continue;
        };
        // A device with no value beside it means the value is zero and was left out.
        *value = Some(value.unwrap_or(0) + delta);
    }
    if residual.store_is_gone {
        record.x_placement_device = Default::default();
        record.y_placement_device = Default::default();
        record.x_advance_device = Default::default();
        record.y_advance_device = Default::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use write_fonts::tables::layout::VariationIndex;
    use write_fonts::NullableOffsetMarker;

    fn device(outer: u16, inner: u16) -> NullableOffsetMarker<DeviceOrVariationIndex> {
        NullableOffsetMarker::new(Some(DeviceOrVariationIndex::VariationIndex(
            VariationIndex::new(outer, inner),
        )))
    }

    #[test]
    fn a_residual_lands_on_the_value_its_device_belongs_to() {
        // Four devices, four different residuals: getting the pairing wrong would move a
        // glyph sideways instead of changing its advance, which renders as a font that
        // looks almost right.
        let deltas = vec![vec![10.0, 20.0, 30.0, 40.0]];
        let residual = Residual::new(&deltas, false);

        let mut record = ValueRecord::new()
            .with_x_placement(1)
            .with_y_placement(2)
            .with_x_advance(3)
            .with_y_advance(4);
        record.x_placement_device = device(0, 0);
        record.y_placement_device = device(0, 1);
        record.x_advance_device = device(0, 2);
        record.y_advance_device = device(0, 3);

        value_of(&mut record, &residual);

        assert_eq!(record.x_placement, Some(11));
        assert_eq!(record.y_placement, Some(22));
        assert_eq!(record.x_advance, Some(33));
        assert_eq!(record.y_advance, Some(44));
    }

    #[test]
    fn a_device_with_no_value_beside_it_starts_from_zero() {
        // A value record leaves out a field that is zero, but the device can still be
        // there; treating the absence as "nothing to correct" would drop the residual.
        let deltas = vec![vec![-25.0]];
        let mut record = ValueRecord::new();
        record.x_advance_device = device(0, 0);

        value_of(&mut record, &Residual::new(&deltas, false));
        assert_eq!(record.x_advance, Some(-25));
    }

    #[test]
    fn a_real_device_table_is_left_alone() {
        // Formats other than VariationIndex are hinting-era data with no relationship to
        // the variation store, and adjusting them would corrupt a font that is not even
        // variable in that respect.
        let deltas = vec![vec![99.0]];
        let mut record = ValueRecord::new().with_x_advance(7);
        record.x_advance_device =
            NullableOffsetMarker::new(Some(DeviceOrVariationIndex::device(11, 12, &[1, 2])));

        value_of(&mut record, &Residual::new(&deltas, false));
        assert_eq!(record.x_advance, Some(7));
    }

    #[test]
    fn the_pointers_go_when_nothing_varies_any_more() {
        // A pointer into a store that is not in the font is a dangling reference, and a
        // shaper following it reads whatever is at that offset.
        let deltas = vec![vec![5.0]];
        let mut record = ValueRecord::new().with_x_advance(100);
        record.x_advance_device = device(0, 0);

        value_of(&mut record, &Residual::new(&deltas, true));
        assert_eq!(record.x_advance, Some(105));
        assert!(record.x_advance_device.is_none());
    }

    #[test]
    fn an_address_outside_the_store_is_ignored_rather_than_panicking() {
        let deltas = vec![vec![1.0]];
        let mut record = ValueRecord::new().with_x_advance(50);
        record.x_advance_device = device(9, 9);

        value_of(&mut record, &Residual::new(&deltas, false));
        assert_eq!(record.x_advance, Some(50));
    }

    #[test]
    fn an_all_zero_residual_is_a_noop_so_gpos_is_left_untouched() {
        // The common case by far. Rewriting GPOS anyway would re-serialise every lookup
        // in the font for no change, and re-serialising is where differences creep in.
        let deltas = vec![vec![0.0, 0.0], vec![0.0]];
        assert!(Residual::new(&deltas, false).is_noop());
        assert!(!Residual::new(&deltas, true).is_noop());
        assert!(!Residual::new(&[vec![3.0]], false).is_noop());
    }
}
