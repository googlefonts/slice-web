//! Re-tenting an ItemVariationStore that carries its own deltas.
//!
//! [`super::regions`] works out how the regions move; this applies that to a store whose
//! deltas are stored *in* it, which is every store except CFF2's. The only user today is
//! `HVAR` on a CFF2 font, where it is not optional: a CFF2 glyph has no phantom points,
//! so `HVAR` is the only place its advance width varies, and dropping it would leave every
//! weight of a partial instance the same width.
//!
//! The `(outer, inner)` addresses are deliberately preserved. Every `DeltaSetIndexMap`
//! and every `VariationIndex` in the font points at one of them, so renumbering would
//! mean rewriting all of those too; fontTools makes the same choice, and says so in
//! `instantiateItemVariationStore`: "The number of VarData subtables, and the number of
//! items within each, are not modified, in order to keep the existing VariationIndex
//! valid."

use read_fonts::tables::variations::ItemVariationStore;
use write_fonts::tables::variations as wvar;

use super::glyphs::ot_round;
use super::regions::{Region, RegionList, Request};
use crate::SliceError;

/// A store rebuilt over a narrowed design space.
pub struct Rebuilt {
    /// The new store, or `None` when nothing varies over the remaining axes.
    pub store: Option<wvar::ItemVariationStore>,
    /// `default_deltas[outer][inner]`: the part of each delta set that no longer depends
    /// on any axis and so has to be baked into whatever the store was modifying.
    pub default_deltas: Vec<Vec<f64>>,
}

/// Re-tent `store` onto the axes that survive `request`.
///
/// With every axis pinned nothing survives: the store comes back as `None`, and each
/// delta set's whole value at the pinned location is in `default_deltas`.
pub fn rebuild(store: &ItemVariationStore, request: &Request) -> Result<Rebuilt, SliceError> {
    let list = store
        .variation_region_list()
        .map_err(|e| SliceError::Read(format!("an item variation store is malformed: {e}")))?;
    let all: Vec<Region> = list
        .variation_regions()
        .iter()
        .map(|region| {
            let mut out = Region::new();
            let Ok(region) = region else { return out };
            for (axis, coords) in region.region_axes().iter().enumerate() {
                let peak = coords.peak_coord().to_f64();
                if peak == 0.0 {
                    continue;
                }
                out.insert(
                    axis,
                    (
                        coords.start_coord().to_f64(),
                        peak,
                        coords.end_coord().to_f64(),
                    ),
                );
            }
            out
        })
        .collect();

    let axis_count = request.surviving_axis_count();
    let mut regions = RegionList::default();
    let mut subtables: Vec<Option<wvar::ItemVariationData>> = Vec::new();
    let mut default_deltas: Vec<Vec<f64>> = Vec::new();

    for data in store.item_variation_data().iter().flatten() {
        let Ok(data) = data else {
            // A null offset is a legal empty subtable, and its index still counts.
            subtables.push(None);
            default_deltas.push(Vec::new());
            continue;
        };

        let old: Vec<Region> = data
            .region_indexes()
            .iter()
            .map(|index| {
                all.get(usize::from(index.get()))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        let remap = request.remap(&old);

        let mut rows: Vec<Vec<i32>> = Vec::with_capacity(usize::from(data.item_count()));
        let mut gains = Vec::with_capacity(usize::from(data.item_count()));
        for item in 0..data.item_count() {
            let deltas: Vec<f64> = data.delta_set(item).map(f64::from).collect();
            let (gain, new) = remap.apply(&deltas);
            gains.push(gain);
            rows.push(new.iter().map(|d| ot_round(*d)).collect());
        }

        let indexes: Vec<u16> = remap
            .regions
            .iter()
            .map(|region| regions.intern(region))
            .collect();
        subtables.push(Some(pack(data.item_count(), &indexes, &rows)));
        default_deltas.push(gains);
    }

    if regions.is_empty() {
        return Ok(Rebuilt {
            store: None,
            default_deltas,
        });
    }
    Ok(Rebuilt {
        store: Some(wvar::ItemVariationStore::new(
            regions.into_written(axis_count),
            subtables,
        )),
        default_deltas,
    })
}

/// Pack delta rows into an ItemVariationData.
///
/// The format allows a per-subtable split into "long" and "short" deltas, sized to what
/// the values need. This takes the simple option and makes every delta long, which costs
/// a byte per short delta and cannot encode a value wrongly. The 32-bit form is used only
/// when something does not fit in 16 bits, because `LONG_WORDS` doubles the whole
/// subtable.
fn pack(item_count: u16, region_indexes: &[u16], rows: &[Vec<i32>]) -> wvar::ItemVariationData {
    const LONG_WORDS: u16 = 0x8000;
    let needs_32_bit = rows
        .iter()
        .flatten()
        .any(|d| *d < i32::from(i16::MIN) || *d > i32::from(i16::MAX));

    let mut delta_sets = Vec::new();
    for row in rows {
        for delta in row {
            if needs_32_bit {
                delta_sets.extend_from_slice(&delta.to_be_bytes());
            } else {
                delta_sets.extend_from_slice(&(*delta as i16).to_be_bytes());
            }
        }
    }

    let word_delta_count = region_indexes.len() as u16 | if needs_32_bit { LONG_WORDS } else { 0 };
    wvar::ItemVariationData::new(
        item_count,
        word_delta_count,
        region_indexes.to_vec(),
        delta_sets,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use read_fonts::{FontData, FontRead};
    use write_fonts::dump_table;

    /// Build a one-axis store with one subtable, one region `(0, 1, 1)` and these deltas.
    fn store_bytes(deltas: &[i32]) -> Vec<u8> {
        let rows: Vec<Vec<i32>> = deltas.iter().map(|d| vec![*d]).collect();
        let data = pack(rows.len() as u16, &[0], &rows);
        let list = wvar::VariationRegionList::new(
            1,
            vec![wvar::VariationRegion::new(vec![
                wvar::RegionAxisCoordinates {
                    start_coord: write_fonts::types::F2Dot14::from_f32(0.0),
                    peak_coord: write_fonts::types::F2Dot14::from_f32(1.0),
                    end_coord: write_fonts::types::F2Dot14::from_f32(1.0),
                },
            ])],
        );
        dump_table(&wvar::ItemVariationStore::new(list, vec![Some(data)])).unwrap()
    }

    #[test]
    fn packed_deltas_read_back_at_both_widths() {
        for deltas in [
            vec![0, 1, -1, 100, -100],
            // Beyond 16 bits, which forces the LONG_WORDS form.
            vec![70_000, -70_000, 1],
        ] {
            let bytes = store_bytes(&deltas);
            let store = ItemVariationStore::read(FontData::new(&bytes)).unwrap();
            let data = store.item_variation_data().get(0).unwrap().unwrap();
            let read: Vec<i32> = (0..data.item_count())
                .map(|i| data.delta_set(i).next().unwrap())
                .collect();
            assert_eq!(read, deltas);
        }
    }

    fn plan(
        limit: crate::axes::AxisLimit,
        normalized: crate::solver::AxisTriple,
    ) -> crate::instancer::partial::AxisPlan {
        crate::instancer::partial::AxisPlan {
            spec: crate::axes::AxisSpec {
                tag: "wght".into(),
                min: 400.0,
                default: 400.0,
                max: 900.0,
                name: None,
                hidden: false,
            },
            limit,
            normalized,
        }
    }

    #[test]
    fn narrowing_an_axis_scales_the_deltas_and_keeps_the_store() {
        let bytes = store_bytes(&[100, -50]);
        let store = ItemVariationStore::read(FontData::new(&bytes)).unwrap();
        let plans = vec![plan(
            crate::axes::AxisLimit::range(400.0, 700.0),
            crate::solver::AxisTriple::with_distances(0.0, 0.0, 0.6, 0.0, 500.0),
        )];
        let rebuilt = rebuild(&store, &Request::Restricted(&plans)).unwrap();

        assert_eq!(rebuilt.default_deltas, vec![vec![0.0, 0.0]]);
        let written = dump_table(&rebuilt.store.unwrap()).unwrap();
        let store = ItemVariationStore::read(FontData::new(&written)).unwrap();
        let data = store.item_variation_data().get(0).unwrap().unwrap();
        let read: Vec<i32> = (0..2).map(|i| data.delta_set(i).next().unwrap()).collect();
        assert_eq!(read, vec![60, -30]);
    }

    #[test]
    fn pinning_the_only_axis_leaves_no_store_and_a_default_delta() {
        let bytes = store_bytes(&[100]);
        let store = ItemVariationStore::read(FontData::new(&bytes)).unwrap();
        let plans = vec![plan(
            crate::axes::AxisLimit::Pin(700.0),
            crate::solver::AxisTriple::new(0.6, 0.6, 0.6),
        )];
        let rebuilt = rebuild(&store, &Request::Restricted(&plans)).unwrap();
        assert!(rebuilt.store.is_none());
        assert!((rebuilt.default_deltas[0][0] - 60.0).abs() < 1e-9);
    }

    #[test]
    fn a_static_location_resolves_the_whole_store_into_default_deltas() {
        // The static instancer asks with the normalized location itself, which is the
        // only form that carries what avar 2 did to it. Same answer as pinning by plan.
        let bytes = store_bytes(&[100, -50]);
        let store = ItemVariationStore::read(FontData::new(&bytes)).unwrap();
        let rebuilt = rebuild(&store, &Request::Pinned(&[0.6])).unwrap();
        assert!(rebuilt.store.is_none());
        assert_eq!(rebuilt.default_deltas.len(), 1);
        assert!((rebuilt.default_deltas[0][0] - 60.0).abs() < 1e-9);
        assert!((rebuilt.default_deltas[0][1] + 30.0).abs() < 1e-9);
    }
}
