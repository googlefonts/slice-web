//! Re-tenting an ItemVariationStore's regions onto a restricted design space.
//!
//! `gvar`'s tuple variations, CFF2's blends and an `HVAR` delta set are the same idea
//! stored three ways: a set of
//! regions, and a delta per region. [`crate::solver`] already knows how to move one
//! region onto a narrower axis, and [`crate::instancer::partial`] uses it on `gvar`. What
//! is different here is *where the deltas live*. A CFF2 blend keeps them in the
//! charstring, so the store cannot be rewritten on its own: what comes out of this module
//! is not new deltas but the linear map that produces them, which the charstring rewriter
//! then applies to whatever numbers it finds.
//!
//! The map has two parts, and both are needed:
//!
//! * **gain** — the share of an old region's delta that no longer depends on any axis,
//!   because the axis it depended on was pinned. It is added to the base value.
//! * **coefficients** — how much of an old region's delta each surviving new region
//!   carries.
//!
//! Pinning every axis is the same computation with no surviving regions, so the static
//! and partial cases run the same code and differ only in what comes out.

use std::collections::BTreeMap;

use crate::solver::{rebase_tent, support_scalar, Tent};

use super::partial::AxisPlan;

/// A region, as the axes it actually depends on: input axis index to support triple.
///
/// Axes whose peak is zero are absent rather than stored, matching the OpenType rule
/// that such an axis does not participate.
pub type Region = BTreeMap<usize, Tent>;

/// One region expressed over the axes that survive, in output order.
pub type DenseRegion = Vec<Tent>;

/// How the regions of one ItemVariationData map onto the new design space.
#[derive(Clone, Debug, Default)]
pub struct RegionRemap {
    /// The regions the output store needs, over the surviving axes.
    pub regions: Vec<DenseRegion>,
    /// `gain[i]`: how much of old region `i`'s delta now applies unconditionally.
    pub gain: Vec<f64>,
    /// `coeff[i][j]`: how much of old region `i`'s delta lands on new region `j`.
    pub coeff: Vec<Vec<f64>>,
}

impl RegionRemap {
    /// How many regions a blend written against this map needs deltas for.
    pub fn new_region_count(&self) -> usize {
        self.regions.len()
    }

    /// How many deltas per value the *input* charstrings carry.
    pub fn old_region_count(&self) -> usize {
        self.gain.len()
    }

    /// Apply the map to one value's deltas.
    ///
    /// Returns the amount to add to the base value, and the deltas for the new regions.
    pub fn apply(&self, deltas: &[f64]) -> (f64, Vec<f64>) {
        let mut gain = 0.0;
        let mut new = vec![0.0; self.regions.len()];
        for (i, delta) in deltas.iter().enumerate() {
            if *delta == 0.0 {
                continue;
            }
            if let Some(g) = self.gain.get(i) {
                gain += delta * g;
            }
            if let Some(row) = self.coeff.get(i) {
                for (slot, coefficient) in new.iter_mut().zip(row) {
                    *slot += delta * coefficient;
                }
            }
        }
        (gain, new)
    }
}

/// What is to become of the design space.
pub enum Request<'a> {
    /// Every axis pinned, at this normalized location in the *input* axis order.
    Pinned(&'a [f64]),
    /// Some axes survive; the plans are in `fvar` order, one per input axis.
    Restricted(&'a [AxisPlan]),
}

impl Request<'_> {
    /// How one set of regions maps onto the requested design space.
    pub fn remap(&self, regions: &[Region]) -> RegionRemap {
        match self {
            Request::Pinned(location) => pinned_remap(regions, location),
            Request::Restricted(plans) => restricted_remap(regions, plans),
        }
    }

    /// How many axes the output font still has.
    pub fn surviving_axis_count(&self) -> u16 {
        match self {
            Request::Pinned(_) => 0,
            Request::Restricted(plans) => {
                plans.iter().filter(|plan| !plan.is_pinned()).count() as u16
            }
        }
    }
}

/// The remap for a location where every axis is pinned.
///
/// Nothing survives, so every region collapses to its scalar at that location and the
/// whole contribution becomes gain.
pub fn pinned_remap(regions: &[Region], location: &[f64]) -> RegionRemap {
    let gain = regions
        .iter()
        .map(|region| region_scalar(region, location))
        .collect::<Vec<_>>();
    RegionRemap {
        regions: Vec::new(),
        coeff: vec![Vec::new(); gain.len()],
        gain,
    }
}

/// The scalar one region contributes at `location`, in the original normalized space.
pub fn region_scalar(region: &Region, location: &[f64]) -> f64 {
    let mut scalar = 1.0;
    for (axis, tent) in region {
        let value = location.get(*axis).copied().unwrap_or(0.0);
        scalar *= support_scalar(value, *tent);
        if scalar == 0.0 {
            break;
        }
    }
    scalar
}

/// The remap for a design space that is narrowed rather than collapsed.
///
/// `plans` is in `fvar` order, one entry per input axis. Axes that stay produce columns
/// in the new regions, in the same order they will appear in the output `fvar`.
pub fn restricted_remap(regions: &[Region], plans: &[AxisPlan]) -> RegionRemap {
    let surviving: Vec<usize> = (0..plans.len())
        .filter(|i| !plans[*i].is_pinned())
        .collect();
    let column: BTreeMap<usize, usize> = surviving
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, new))
        .collect();

    let mut out = RegionRemap {
        regions: Vec::new(),
        gain: vec![0.0; regions.len()],
        coeff: vec![Vec::new(); regions.len()],
    };
    // New regions are identified by their F2Dot14 form, which is the precision they will
    // be written at: two regions that differ only below it are the same region on disk,
    // and leaving them separate would write a store with duplicate columns.
    let mut seen: BTreeMap<Vec<(i16, i16, i16)>, usize> = BTreeMap::new();

    for (index, region) in regions.iter().enumerate() {
        for (scalar, limited) in limit_region(region, plans) {
            if scalar == 0.0 {
                continue;
            }
            if limited.is_empty() {
                out.gain[index] += scalar;
                continue;
            }
            // A region that still depends on a pinned axis cannot be written against the
            // output's axes. The limiting above removes pinned axes, so reaching this
            // means the font's regions disagree with its own `fvar`.
            if limited.keys().any(|axis| !column.contains_key(axis)) {
                continue;
            }
            let mut dense: DenseRegion = vec![(0.0, 0.0, 0.0); surviving.len()];
            for (axis, tent) in &limited {
                dense[column[axis]] = *tent;
            }
            let key = quantized_key(&dense);
            let slot = *seen.entry(key).or_insert_with(|| {
                out.regions.push(dense);
                out.regions.len() - 1
            });
            let row = &mut out.coeff[index];
            if row.len() <= slot {
                row.resize(slot + 1, 0.0);
            }
            row[slot] += scalar;
        }
    }

    // Every row has to be as wide as the region list, because `apply` walks them in
    // parallel and a short row would silently drop the last regions' contributions.
    let width = out.regions.len();
    for row in &mut out.coeff {
        row.resize(width, 0.0);
    }
    out
}

fn quantized_key(region: &DenseRegion) -> Vec<(i16, i16, i16)> {
    use write_fonts::types::F2Dot14;
    let bits = |v: f64| F2Dot14::from_f32(v as f32).to_bits();
    region
        .iter()
        .map(|(start, peak, end)| (bits(*start), bits(*peak), bits(*end)))
        .collect()
}

/// The region list an output ItemVariationStore needs, built by interning regions.
///
/// Each ItemVariationData is re-tented on its own and comes back with its own list of
/// regions, but the store has one shared list that the subtables index into. Interning
/// is what merges them, and it has to compare at F2Dot14 precision because that is what
/// the file stores.
#[derive(Default)]
pub struct RegionList {
    regions: Vec<DenseRegion>,
    seen: BTreeMap<Vec<(i16, i16, i16)>, u16>,
}

impl RegionList {
    /// The index of `region` in the list, adding it if it is not already there.
    pub fn intern(&mut self, region: &DenseRegion) -> u16 {
        let next = self.regions.len() as u16;
        *self.seen.entry(quantized_key(region)).or_insert_with(|| {
            self.regions.push(region.clone());
            next
        })
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// The list in the form write-fonts wants.
    pub fn into_written(
        self,
        axis_count: u16,
    ) -> write_fonts::tables::variations::VariationRegionList {
        use write_fonts::tables::variations as wvar;
        use write_fonts::types::F2Dot14;
        let regions = self
            .regions
            .into_iter()
            .map(|region| {
                wvar::VariationRegion::new(
                    region
                        .into_iter()
                        .map(|(start, peak, end)| wvar::RegionAxisCoordinates {
                            start_coord: F2Dot14::from_f32(start as f32),
                            peak_coord: F2Dot14::from_f32(peak as f32),
                            end_coord: F2Dot14::from_f32(end as f32),
                        })
                        .collect(),
                )
            })
            .collect();
        wvar::VariationRegionList::new(axis_count, regions)
    }
}

/// Apply every axis's limit to one region, which may drop it, keep it, or split it.
///
/// This mirrors `partial::limit_tuples`, which does the same thing to a `gvar` tuple's
/// deltas. The difference is that there is nothing to scale here: the scalar is carried
/// out to the caller, which multiplies whatever deltas the charstring turns out to hold.
fn limit_region(region: &Region, plans: &[AxisPlan]) -> Vec<(f64, Region)> {
    let mut current = vec![(1.0, region.clone())];

    for (axis, plan) in plans.iter().enumerate() {
        if plan.is_untouched() {
            continue;
        }
        let mut next = Vec::with_capacity(current.len());
        for (scalar, region) in &current {
            let Some(&tent) = region.get(&axis) else {
                next.push((*scalar, region.clone()));
                continue;
            };
            let (lower, peak, upper) = tent;

            // An axis present but peaking at zero contributes nothing (fontTools issue
            // #3453), and a tent that is out of order or straddles the default has no
            // defensible meaning at all -- the solver asserts on both.
            if peak == 0.0 {
                let mut without = region.clone();
                without.remove(&axis);
                next.push((*scalar, without));
                continue;
            }
            if !(lower <= peak && peak <= upper) || (lower < 0.0 && upper > 0.0) {
                continue;
            }

            for (factor, new_tent) in rebase_tent(tent, plan.normalized) {
                let mut updated = region.clone();
                match new_tent {
                    None => {
                        updated.remove(&axis);
                    }
                    Some(t) => {
                        updated.insert(axis, t);
                    }
                }
                next.push((scalar * factor, updated));
            }
        }
        current = next;
    }

    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axes::{AxisLimit, AxisSpec};
    use crate::solver::AxisTriple;

    fn plan(limit: AxisLimit, normalized: AxisTriple) -> AxisPlan {
        AxisPlan {
            spec: AxisSpec {
                tag: "wght".into(),
                min: 400.0,
                default: 400.0,
                max: 900.0,
                name: Some("Weight".into()),
                hidden: false,
            },
            limit,
            normalized,
        }
    }

    fn one_axis(tent: Tent) -> Region {
        [(0usize, tent)].into_iter().collect()
    }

    #[test]
    fn pinning_turns_a_region_into_its_scalar() {
        let regions = vec![one_axis((0.0, 1.0, 1.0))];
        let remap = pinned_remap(&regions, &[0.6]);
        assert_eq!(remap.new_region_count(), 0);
        assert!((remap.gain[0] - 0.6).abs() < 1e-12);

        // A delta of 130 at a scalar of 0.6 lands 78 units on the base value, which is
        // what fontTools writes for this fixture at wght=700.
        let (gain, deltas) = remap.apply(&[130.0]);
        assert!((gain - 78.0).abs() < 1e-9);
        assert!(deltas.is_empty());
    }

    #[test]
    fn a_region_outside_the_pinned_location_contributes_nothing() {
        let regions = vec![one_axis((0.5, 1.0, 1.0))];
        let remap = pinned_remap(&regions, &[0.25]);
        assert_eq!(remap.gain[0], 0.0);
        assert_eq!(remap.apply(&[500.0]).0, 0.0);
    }

    #[test]
    fn narrowing_an_axis_scales_the_delta_and_keeps_the_region() {
        // wght 400/400/900 narrowed to 400:700 puts the new maximum at 0.6 normalized.
        // The single region (0, 1, 1) keeps its shape and its delta shrinks by 0.6,
        // which is exactly what fontTools 4.62.1 emits for the `cff2-vf` fixture.
        let regions = vec![one_axis((0.0, 1.0, 1.0))];
        let plans = vec![plan(
            AxisLimit::range(400.0, 700.0),
            AxisTriple::with_distances(0.0, 0.0, 0.6, 0.0, 500.0),
        )];
        let remap = restricted_remap(&regions, &plans);

        assert_eq!(remap.new_region_count(), 1);
        assert_eq!(remap.regions[0].len(), 1);
        assert_eq!(remap.gain[0], 0.0);
        let (gain, deltas) = remap.apply(&[130.0]);
        assert_eq!(gain, 0.0);
        assert!((deltas[0] - 78.0).abs() < 1e-9, "{deltas:?}");
    }

    #[test]
    fn an_untouched_axis_passes_its_region_through_unchanged() {
        let regions = vec![one_axis((0.0, 1.0, 1.0))];
        let plans = vec![plan(AxisLimit::Full, AxisTriple::new(-1.0, 0.0, 1.0))];
        let remap = restricted_remap(&regions, &plans);
        assert_eq!(remap.regions, vec![vec![(0.0, 1.0, 1.0)]]);
        assert_eq!(remap.apply(&[100.0]), (0.0, vec![100.0]));
    }

    #[test]
    fn regions_that_land_on_the_same_tent_share_one_column() {
        // Two input regions that differ only below F2Dot14's precision are one region in
        // the file, so they must not be written twice.
        let regions = vec![one_axis((0.0, 1.0, 1.0)), one_axis((0.0, 1.0 - 1e-7, 1.0))];
        let plans = vec![plan(AxisLimit::Full, AxisTriple::new(-1.0, 0.0, 1.0))];
        let remap = restricted_remap(&regions, &plans);
        assert_eq!(remap.new_region_count(), 1);
        assert_eq!(remap.apply(&[10.0, 20.0]), (0.0, vec![30.0]));
    }

    #[test]
    fn a_two_axis_region_loses_only_the_pinned_half() {
        // wdth pinned at its maximum, wght kept whole: the region's wdth tent evaluates
        // to 1 and disappears, leaving a region on wght alone.
        let region: Region = [(0usize, (0.0, 1.0, 1.0)), (1usize, (0.0, 1.0, 1.0))]
            .into_iter()
            .collect();
        let plans = vec![
            plan(AxisLimit::Full, AxisTriple::new(-1.0, 0.0, 1.0)),
            plan(AxisLimit::Pin(900.0), AxisTriple::new(1.0, 1.0, 1.0)),
        ];
        let remap = restricted_remap(&[region], &plans);
        assert_eq!(remap.new_region_count(), 1);
        assert_eq!(remap.regions[0], vec![(0.0, 1.0, 1.0)]);
        assert_eq!(remap.apply(&[64.0]), (0.0, vec![64.0]));
    }

    #[test]
    fn a_malformed_region_is_dropped_rather_than_reaching_the_solver() {
        // A tent straddling the default is not a tent; the solver asserts on one.
        let regions = vec![one_axis((-1.0, 0.5, 1.0))];
        let plans = vec![plan(
            AxisLimit::range(400.0, 700.0),
            AxisTriple::with_distances(0.0, 0.0, 0.6, 0.0, 500.0),
        )];
        let remap = restricted_remap(&regions, &plans);
        assert_eq!(remap.new_region_count(), 0);
        assert_eq!(remap.gain[0], 0.0);
    }
}
