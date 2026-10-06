//! Instancing a `CFF2` table.
//!
//! CFF2 keeps its variation data in two places at once, and that is the whole shape of
//! the problem. The ItemVariationStore in the Top DICT holds the *regions* — which part
//! of the design space each contribution covers — while the deltas themselves sit inline
//! in the charstrings, as operands of the `blend` operator. Neither half means anything
//! without the other, so instancing has to rewrite both together.
//!
//! | request | store | charstrings |
//! |---|---|---|
//! | every axis pinned | dropped | every `blend` resolved into the value it produces |
//! | an axis narrowed | re-tented | every `blend` re-expressed over the new regions |
//!
//! Both are the same computation: [`regions`] turns the old regions into a linear map,
//! and [`charstring`] applies that map to whatever deltas it finds. Pinning is the case
//! where no regions survive, so the map has only its unconditional part and the `blend`
//! operators disappear on their own.
//!
//! This matches what fontTools 4.62.1 does. In particular a fully pinned CFF2 font stays
//! **CFF2**: fontTools' `instantiateCFF2` resolves the blends and drops the store but
//! does not convert to CFF 1.0 unless asked, and converting would mean writing a second,
//! different outline format for no benefit the corpus can name.

pub mod charstring;
pub mod num;
pub mod table;

use read_fonts::tables::variations::ItemVariationStore;
use read_fonts::{FontData, FontRead, FontRef};
use write_fonts::tables::variations as wvar;

use super::regions::{self, Region, RegionRemap};
use crate::SliceError;

pub use super::regions::Request;

/// A rebuilt `CFF2` table.
pub struct Cff2Instance {
    pub table: Vec<u8>,
    /// True when the result still has an ItemVariationStore, and so still varies.
    pub varies: bool,
}

/// The Private DICT operator that names the ItemVariationData its blends index.
const PRIVATE_VSINDEX: u16 = 22;
/// The Private DICT operator holding the local subroutine offset, which the writer
/// recomputes rather than copying.
const PRIVATE_SUBRS: u16 = 19;
/// The DICT `blend` operator, which is numbered differently from the charstring one.
const DICT_BLEND: u16 = 23;

fn malformed(what: &str) -> SliceError {
    SliceError::Read(format!("the CFF2 table is malformed: {what}"))
}

/// Rewrite a font's `CFF2` table for the requested design space.
pub fn instantiate(font: &FontRef, request: &Request) -> Result<Cff2Instance, SliceError> {
    let source = table::read(font)?;

    // One remap per ItemVariationData, because `vsindex` selects between them and each
    // one has its own region list.
    let per_data = read_regions(source.var_store)?;
    let remaps: Vec<RegionRemap> = per_data
        .iter()
        .map(|regions| request.remap(regions))
        .collect();

    let var_store = build_var_store(&remaps, request)?;

    // Which font DICT each glyph belongs to. A font with no FDSelect has one.
    let fd_select = match source.fd_select {
        None => None,
        Some(bytes) => Some(
            read_fonts::ps::cff::fd_select::FdSelect::read(FontData::new(bytes))
                .map_err(|e| SliceError::Read(format!("the CFF2 FDSelect is malformed: {e}")))?,
        ),
    };

    let global_subrs = source.global_subrs.clone();
    let mut charstrings = Vec::with_capacity(source.charstrings.len());
    for (gid, program) in source.charstrings.iter().enumerate() {
        let fd = fd_select
            .as_ref()
            .and_then(|select| select.font_index(write_fonts::types::GlyphId::new(gid as u32)))
            .unwrap_or(0) as usize;
        let font_dict = source.font_dicts.get(fd).ok_or_else(|| {
            SliceError::Read(format!(
                "FDSelect sends glyph {gid} to font DICT {fd}, which does not exist"
            ))
        })?;
        let context = charstring::Context {
            global_subrs: &global_subrs,
            local_subrs: &font_dict.local_subrs,
            remaps: &remaps,
            initial_vsindex: font_dict.vsindex(),
        };
        charstrings.push(charstring::rewrite(program, &context)?);
    }

    let mut font_dicts = Vec::with_capacity(source.font_dicts.len());
    for font_dict in &source.font_dicts {
        font_dicts.push(table::FontDictBuilder {
            other_entries: font_dict
                .other_entries
                .iter()
                .map(|entry| entry.raw.clone())
                .collect(),
            private: rewrite_private_dict(&font_dict.private, &remaps, var_store.is_some())?,
            // Every subroutine was inlined into the charstrings that called it, so there
            // is nothing left for these to hold. See `charstring` for why.
            local_subrs: Vec::new(),
        });
    }

    let builder = table::Cff2Builder {
        top_dict_extra: table::top_dict_extra(&source.top_dict),
        // Inlining consumed the global subroutines along with the local ones.
        global_subrs: Vec::new(),
        charstrings,
        var_store: var_store.clone(),
        fd_select: source.fd_select.map(<[u8]>::to_vec),
        font_dicts,
    };

    Ok(Cff2Instance {
        table: builder.build()?,
        varies: var_store.is_some(),
    })
}

/// The regions each ItemVariationData refers to, in the order its blends index them.
fn read_regions(store: Option<&[u8]>) -> Result<Vec<Vec<Region>>, SliceError> {
    let Some(bytes) = store else {
        return Ok(Vec::new());
    };
    let store = ItemVariationStore::read(FontData::new(bytes))
        .map_err(|e| SliceError::Read(format!("the CFF2 variation store is malformed: {e}")))?;
    let list = store
        .variation_region_list()
        .map_err(|e| SliceError::Read(format!("the CFF2 variation store is malformed: {e}")))?;

    let all: Vec<Region> = list
        .variation_regions()
        .iter()
        .map(|region| {
            let mut out = Region::new();
            let Ok(region) = region else { return out };
            for (axis, coords) in region.region_axes().iter().enumerate() {
                let peak = coords.peak_coord().to_f64();
                // A peak of zero means the axis does not participate, which is not the
                // same as a tent of (0, 0, 0) and must not be stored as one.
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

    let mut per_data = Vec::new();
    for data in store.item_variation_data().iter().flatten() {
        let Ok(data) = data else {
            per_data.push(Vec::new());
            continue;
        };
        per_data.push(
            data.region_indexes()
                .iter()
                .map(|index| {
                    all.get(usize::from(index.get()))
                        .cloned()
                        .unwrap_or_default()
                })
                .collect(),
        );
    }
    Ok(per_data)
}

/// Build the output ItemVariationStore from the remaps, or none when nothing varies.
///
/// The store's own delta arrays stay empty: in CFF2 the deltas live in the charstrings,
/// and the store exists only to name the regions and to group them into the subtables
/// that `vsindex` selects between.
fn build_var_store(
    remaps: &[RegionRemap],
    request: &Request,
) -> Result<Option<Vec<u8>>, SliceError> {
    if matches!(request, Request::Pinned(_)) {
        return Ok(None);
    }
    if remaps.iter().all(|remap| remap.new_region_count() == 0) {
        return Ok(None);
    }

    let axis_count = request.surviving_axis_count();
    let mut list = regions::RegionList::default();
    let data: Vec<Option<wvar::ItemVariationData>> = remaps
        .iter()
        .map(|remap| {
            let indexes = remap
                .regions
                .iter()
                .map(|region| list.intern(region))
                .collect();
            // itemCount and wordDeltaCount are both zero: a CFF2 store holds no deltas
            // of its own, only the regions its charstrings' blends are stated against.
            Some(wvar::ItemVariationData::new(0, 0, indexes, Vec::new()))
        })
        .collect();

    let store = wvar::ItemVariationStore::new(list.into_written(axis_count), data);
    let bytes = write_fonts::dump_table(&store)
        .map_err(|e| SliceError::Write(format!("could not write the CFF2 variation store: {e}")))?;
    Ok(Some(bytes))
}

/// Resolve or re-tent the blends in one Private DICT.
///
/// A Private DICT carries the alignment zones and stem widths that keep a face readable
/// at text sizes, and in a variable font those move with the design space too. Dropping
/// them would be a silent quality loss; leaving an unresolved `blend` in a font whose
/// store has just been deleted would be a malformed one.
///
/// The awkwardness is that a DICT `blend` leaves its results on the operand stack for a
/// *later* operator to consume, so an entry's values can be split across several parsed
/// entries. `carried` is that stack.
fn rewrite_private_dict(
    entries: &[num::DictEntry],
    remaps: &[RegionRemap],
    keeps_store: bool,
) -> Result<Vec<u8>, SliceError> {
    let mut out = Vec::new();
    let mut vsindex = 0usize;
    let mut carried: Vec<BlendedValue> = Vec::new();

    for entry in entries {
        match entry.operator {
            // The writer recomputes this; see `Cff2Builder::build`.
            PRIVATE_SUBRS => continue,
            PRIVATE_VSINDEX => {
                vsindex = entry.operands.first().copied().unwrap_or(0.0).max(0.0) as usize;
                // With no store left there is nothing to index, and the operator would
                // point at a table that is no longer in the font.
                if keeps_store {
                    out.extend_from_slice(&entry.raw);
                }
            }
            DICT_BLEND => {
                let mut stack: Vec<BlendedValue> = std::mem::take(&mut carried);
                stack.extend(entry.operands.iter().map(|v| BlendedValue::plain(*v)));
                let count = stack
                    .pop()
                    .ok_or_else(|| malformed("a Private DICT blend has no operand count"))?
                    .base;
                let mut blended = resolve_dict_blend(&mut stack, count, remaps, vsindex)?;
                // Whatever sat below the blend's operands stays below its results.
                stack.append(&mut blended);
                carried = stack;
            }
            _ => {
                let mut values = std::mem::take(&mut carried);
                values.extend(entry.operands.iter().map(|v| BlendedValue::plain(*v)));
                write_dict_values(&values, &mut out);
                write_dict_operator(entry.operator, &mut out);
            }
        }
    }
    if !carried.is_empty() {
        return Err(malformed(
            "a Private DICT ended with a blend and no operator",
        ));
    }
    Ok(out)
}

/// One Private DICT value, with whatever deltas still apply to it.
struct BlendedValue {
    base: f64,
    deltas: Vec<f64>,
}

impl BlendedValue {
    fn plain(base: f64) -> Self {
        BlendedValue {
            base,
            deltas: Vec::new(),
        }
    }
}

fn resolve_dict_blend(
    stack: &mut Vec<BlendedValue>,
    count: f64,
    remaps: &[RegionRemap],
    vsindex: usize,
) -> Result<Vec<BlendedValue>, SliceError> {
    if !(1.0..=65535.0).contains(&count) || count.fract() != 0.0 {
        return Err(malformed(&format!(
            "{count} is not a Private DICT blend operand count"
        )));
    }
    let count = count as usize;
    let remap = remaps.get(vsindex).ok_or_else(|| {
        malformed(&format!(
            "a Private DICT blends against variation data {vsindex}, which does not exist"
        ))
    })?;
    let old_regions = remap.old_region_count();
    let needed = count * (old_regions + 1);
    if stack.len() < needed {
        return Err(malformed("a Private DICT blend has too few operands"));
    }
    let operands: Vec<f64> = stack
        .split_off(stack.len() - needed)
        .into_iter()
        .map(|value| {
            if value.deltas.is_empty() {
                Ok(value.base)
            } else {
                Err(malformed("Private DICT blends cannot nest"))
            }
        })
        .collect::<Result<_, _>>()?;
    let (bases, deltas) = operands.split_at(count);

    Ok(bases
        .iter()
        .enumerate()
        .map(|(index, base)| {
            let row = &deltas[index * old_regions..(index + 1) * old_regions];
            let (gain, new_deltas) = remap.apply(row);
            BlendedValue {
                base: base + gain.round_ties_even(),
                deltas: new_deltas
                    .iter()
                    .map(|d| f64::from(crate::instancer::glyphs::ot_round(*d)))
                    .collect(),
            }
        })
        .collect())
}

/// Write a run of Private DICT values, grouping the ones that still vary into a `blend`.
fn write_dict_values(values: &[BlendedValue], out: &mut Vec<u8>) {
    let varying = |value: &BlendedValue| value.deltas.iter().any(|d| *d != 0.0);
    let mut index = 0;
    while index < values.len() {
        if !varying(&values[index]) {
            num::write_dict_number(values[index].base, out);
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && varying(&values[index]) {
            index += 1;
        }
        let run = &values[start..index];
        for value in run {
            num::write_dict_number(value.base, out);
        }
        for value in run {
            for delta in &value.deltas {
                num::write_dict_number(*delta, out);
            }
        }
        num::write_dict_integer(run.len() as i32, out);
        out.push(DICT_BLEND as u8);
    }
}

fn write_dict_operator(operator: u16, out: &mut Vec<u8>) {
    if operator >= 1200 {
        out.push(12);
        out.push((operator - 1200) as u8);
    } else {
        out.push(operator as u8);
    }
}
