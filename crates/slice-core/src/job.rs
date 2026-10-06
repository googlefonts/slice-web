//! Running a slice: everything that happens when the Slice button is pressed.

use read_fonts::{FontRef, TableProvider};
use write_fonts::tables::head::MacStyle;
use write_fonts::tables::name::{Name, NameRecord};
use write_fonts::tables::os2::SelectionFlags;
use write_fonts::types::NameId;
use write_fonts::{from_obj::ToOwnedTable, FontBuilder};

use crate::axes::{AxisLimit, AxisSpec};
use crate::bits::BitFlags;
use crate::finalize::{finalize, variation_name_ids, Finalize};
use crate::font::{SliceFont, WIN_ENCODING, WIN_LANGUAGE, WIN_PLATFORM};
use crate::instancer::{
    instantiate_feature_variations, instantiate_partial, instantiate_static, normalize_location,
    plan_axes,
};
use crate::names::NameEdits;
use crate::overlaps::{remove_overlaps, OverlapReport};
use crate::SliceError;

/// The container to write the result in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// A bare sfnt: `.ttf` or `.otf` depending on the outlines.
    #[default]
    Sfnt,
    /// WOFF 1.0.
    Woff,
    /// WOFF2.
    Woff2,
}

impl OutputFormat {
    /// The extension a file in this format should carry.
    ///
    /// `is_truetype` decides between `.ttf` and `.otf` for a bare sfnt, which is a
    /// convention rather than a requirement but is what everything expects.
    pub fn extension(&self, is_truetype: bool) -> &'static str {
        match self {
            OutputFormat::Sfnt if is_truetype => "ttf",
            OutputFormat::Sfnt => "otf",
            OutputFormat::Woff => "woff",
            OutputFormat::Woff2 => "woff2",
        }
    }

    /// Guess the format from a file name the user typed.
    pub fn from_filename(name: &str) -> OutputFormat {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".woff2") {
            OutputFormat::Woff2
        } else if lower.ends_with(".woff") {
            OutputFormat::Woff
        } else {
            OutputFormat::Sfnt
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            OutputFormat::Sfnt => "font/ttf",
            OutputFormat::Woff => "font/woff",
            OutputFormat::Woff2 => "font/woff2",
        }
    }
}

/// Everything the three editors and the options add up to.
#[derive(Clone, Debug)]
pub struct SliceJob {
    /// One limit per `fvar` axis, in `fvar` order.
    pub limits: Vec<AxisLimit>,
    pub names: NameEdits,
    pub bits: BitFlags,
    /// Merge overlapping contours after instancing.
    pub remove_overlaps: bool,
    pub format: OutputFormat,
}

impl SliceJob {
    /// A job that changes nothing, ready to be filled in from the editors.
    pub fn new(font: &SliceFont) -> Result<Self, SliceError> {
        let axes = font.axes()?;
        Ok(SliceJob {
            limits: vec![AxisLimit::Full; axes.len()],
            names: font.name_edits(),
            bits: font.bit_flags(),
            remove_overlaps: false,
            format: OutputFormat::Sfnt,
        })
    }
}

/// What a completed slice produced.
#[derive(Clone)]
pub struct SliceOutput {
    /// The finished font file.
    pub bytes: Vec<u8>,
    /// A line per step, for the status bar and the log.
    pub notes: Vec<String>,
    pub overlaps: Option<OverlapReport>,
}

impl std::fmt::Debug for SliceOutput {
    /// Deliberately hand-written: the default would dump the entire font file into any
    /// failing test's output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SliceOutput")
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .field("notes", &self.notes)
            .field("overlaps", &self.overlaps)
            .finish()
    }
}

impl SliceJob {
    /// Check the job against the font, the way the original validates before opening the
    /// save dialog.
    ///
    /// Returns the validated limits paired with their axes.
    pub fn validate<'a>(
        &self,
        axes: &'a [AxisSpec],
    ) -> Result<Vec<(&'a AxisSpec, AxisLimit)>, SliceError> {
        if self.limits.len() != axes.len() {
            return Err(SliceError::Unsupported(format!(
                "the job describes {} axes but the font has {}",
                self.limits.len(),
                axes.len()
            )));
        }

        let mut validated = Vec::with_capacity(axes.len());
        for (axis, limit) in axes.iter().zip(&self.limits) {
            validated.push((axis, axis.validate(*limit)?));
        }

        // The original refuses to run when no axis cell was filled in, on the grounds
        // that the output would just be a copy of the input. Removing overlaps is a real
        // change, so it lifts that objection.
        //
        // The question is asked of the limits as typed, not as validated. A range can
        // clamp to the axis's own extent -- `100:900` on a 300..1000 axis -- and judging
        // the clamped result would refuse that while accepting the equivalent `300:900`,
        // a distinction the user has no way to predict. A no-op slice is also not a
        // no-op internally: the instancer still runs its optimizations over the font.
        let changes_design_space = self.limits.iter().any(|l| l.is_restriction());
        if !changes_design_space && !self.remove_overlaps {
            return Err(SliceError::NothingToDo);
        }

        Ok(validated)
    }

    /// True when every axis is pinned, so the result is a static font.
    pub fn is_fully_pinned(&self) -> bool {
        !self.limits.is_empty() && self.limits.iter().all(|l| !l.keeps_axis())
    }

    /// Run the job.
    pub fn run(&self, font: &SliceFont) -> Result<SliceOutput, SliceError> {
        let axes = font.axes()?;
        let validated = self.validate(&axes)?;
        let mut notes = Vec::new();

        let font_ref = font.font_ref()?;

        // Which name records the input's fvar and STAT referred to. Anything in here
        // that the output no longer refers to is pruned at the end; collecting it now
        // is the only chance, since fvar is about to be rewritten or dropped.
        let names_before = variation_name_ids(&font_ref);

        // The location the output sits at, in user space, for OS/2's weight and width
        // classes and post's italic angle. A pinned axis contributes its pin; anything
        // still variable contributes the default it will open at.
        let final_location: Vec<(String, f64)> = validated
            .iter()
            .map(|(axis, limit)| {
                let value = match limit {
                    AxisLimit::Pin(v) => *v,
                    AxisLimit::Range { min, max, .. } => axis.default.clamp(*min, *max),
                    AxisLimit::Full => axis.default,
                };
                (axis.tag.clone(), value)
            })
            .collect();

        // Instancing.
        let mut bytes = if self.is_fully_pinned() {
            let user: Vec<f64> = validated
                .iter()
                .map(|(axis, limit)| match limit {
                    AxisLimit::Pin(v) => *v,
                    _ => axis.default,
                })
                .collect();
            let location = normalize_location(&font_ref, &axes, &user);
            let limits: Vec<AxisLimit> = validated.iter().map(|(_, l)| *l).collect();
            let plans = plan_axes(&font_ref, &axes, &limits);
            notes.push(format!(
                "Pinned {}",
                validated
                    .iter()
                    .map(|(axis, limit)| format!("{}={}", axis.tag, limit))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            instantiate_static(&font_ref, &location, &plans)?
        } else if validated.iter().any(|(_, l)| l.is_restriction()) {
            let limits: Vec<AxisLimit> = validated.iter().map(|(_, l)| *l).collect();
            let plans = plan_axes(&font_ref, &axes, &limits);
            notes.push(format!(
                "Restricted design space to {}",
                plans
                    .iter()
                    .map(|plan| {
                        let (min, _, max) = plan.output_extent();
                        if plan.is_pinned() {
                            format!("{}={}", plan.spec.tag, min)
                        } else {
                            format!("{}={}:{}", plan.spec.tag, min, max)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            instantiate_partial(&font_ref, &plans)?
        } else {
            // Nothing to do to the design space; the user only asked for overlap
            // removal or editor changes.
            font.data().to_vec()
        };

        // Feature variations describe substitutions by axis position, so they have to
        // be resolved against the new design space before anything downstream can rely
        // on the font's shaping behaviour.
        {
            let plans = plan_axes(
                &font_ref,
                &axes,
                &validated.iter().map(|(_, l)| *l).collect::<Vec<_>>(),
            );
            let resolved = instantiate_feature_variations(&bytes, &plans)?;
            if resolved.len() != bytes.len() {
                notes.push("Resolved feature variations".to_string());
            }
            bytes = resolved;
        }

        // Overlap removal, which needs the outlines to have stopped moving.
        let mut overlap_report = None;
        if self.remove_overlaps {
            let (simplified, report) = remove_overlaps(&bytes)?;
            notes.push(format!("Overlaps: {}", report.summary()));
            overlap_report = Some(report);
            bytes = simplified;
        }

        // Editor changes, applied last so they survive everything else.
        bytes = apply_names(&bytes, &self.names)?;
        bytes = apply_bits(&bytes, self.bits)?;
        notes.push("Applied name records and bit flags".to_string());

        // Everything that depends on the final outlines: maxp, the head bounding box,
        // hhea's extremes, OS/2's average width and weight/width class, post's italic
        // angle, and the name records that only existed to name axes now gone.
        bytes = finalize(
            &bytes,
            &Finalize {
                location: final_location,
                variation_name_ids_before: names_before,
            },
        )?;
        notes.push("Recalculated metrics and pruned unused name records".to_string());

        // Container.
        match self.format {
            OutputFormat::Sfnt => {}
            OutputFormat::Woff => {
                bytes = crate::font::woff::encode_woff(&bytes)?;
                notes.push("Wrapped as WOFF".to_string());
            }
            OutputFormat::Woff2 => {
                bytes = crate::font::woff2::encode_woff2(&bytes)?;
                notes.push("Wrapped as WOFF2".to_string());
            }
        }

        Ok(SliceOutput {
            bytes,
            notes,
            overlaps: overlap_report,
        })
    }
}

/// Rewrite the `name` table according to the Name Editor.
pub fn apply_names(font_bytes: &[u8], edits: &NameEdits) -> Result<Vec<u8>, SliceError> {
    let font = FontRef::new(font_bytes).map_err(|e| SliceError::Read(e.to_string()))?;
    let (writes, deletes) = edits.plan();

    let mut records: Vec<NameRecord> = match font.name() {
        Ok(name) => {
            let data = name.string_data();
            name.name_record()
                .iter()
                .filter_map(|record| {
                    let text = record.string(data).ok()?.chars().collect::<String>();
                    Some(NameRecord::new(
                        record.platform_id(),
                        record.encoding_id(),
                        record.language_id(),
                        record.name_id(),
                        text.into(),
                    ))
                })
                .collect()
        }
        Err(_) => Vec::new(),
    };

    let is_target = |record: &NameRecord, id: u16| {
        record.name_id == NameId::new(id)
            && record.platform_id == WIN_PLATFORM
            && record.encoding_id == WIN_ENCODING
            && record.language_id == WIN_LANGUAGE
    };

    for id in deletes {
        records.retain(|record| !is_target(record, id));
    }
    for (id, text) in writes {
        records.retain(|record| !is_target(record, id));
        records.push(NameRecord::new(
            WIN_PLATFORM,
            WIN_ENCODING,
            WIN_LANGUAGE,
            NameId::new(id),
            text.into(),
        ));
    }

    // The name table requires its records sorted by platform, encoding, language, then
    // name ID.
    records.sort_by_key(|r| {
        (
            r.platform_id,
            r.encoding_id,
            r.language_id,
            r.name_id.to_u16(),
        )
    });

    let mut builder = FontBuilder::new();
    builder
        .add_table(&Name::new(records))
        .map_err(|e| SliceError::Write(e.to_string()))?;
    crate::instancer::statics::copy_remaining_tables(&mut builder, &font, &[]);
    Ok(builder.build())
}

/// Write the Bit Flag Editor's state into `OS/2` and `head`.
pub fn apply_bits(font_bytes: &[u8], bits: BitFlags) -> Result<Vec<u8>, SliceError> {
    let font = FontRef::new(font_bytes).map_err(|e| SliceError::Read(e.to_string()))?;
    let mut builder = FontBuilder::new();

    if let Ok(os2) = font.os2() {
        let mut os2: write_fonts::tables::os2::Os2 = os2.to_owned_table();
        os2.fs_selection = SelectionFlags::from_bits_truncate(bits.fs_selection);
        builder
            .add_table(&os2)
            .map_err(|e| SliceError::Write(e.to_string()))?;
    }

    let mut head: write_fonts::tables::head::Head = font.head()?.to_owned_table();
    head.mac_style = MacStyle::from_bits_truncate(bits.mac_style);
    builder
        .add_table(&head)
        .map_err(|e| SliceError::Write(e.to_string()))?;

    crate::instancer::statics::copy_remaining_tables(&mut builder, &font, &[]);
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> SliceFont {
        SliceFont::load(crate::testdata::recursive_vf().to_vec()).unwrap()
    }

    /// A job that pins every axis to its default.
    fn pin_all(font: &SliceFont) -> SliceJob {
        let axes = font.axes().unwrap();
        SliceJob {
            limits: axes.iter().map(|a| AxisLimit::Pin(a.default)).collect(),
            names: font.name_edits(),
            bits: font.bit_flags(),
            remove_overlaps: false,
            format: OutputFormat::Sfnt,
        }
    }

    #[test]
    fn asking_for_the_original_design_space_is_refused() {
        let font = font();
        let job = SliceJob::new(&font).unwrap();
        let err = job.run(&font).unwrap_err();
        assert!(matches!(err, SliceError::NothingToDo), "got {err:?}");
    }

    #[test]
    fn overlap_removal_alone_is_a_real_request() {
        // The original refuses any job that does not narrow the design space. Removing
        // overlaps changes the font, so it should be allowed on its own -- except that
        // it needs a static font, which this one is not.
        let font = font();
        let mut job = SliceJob::new(&font).unwrap();
        job.remove_overlaps = true;
        let err = job.run(&font).unwrap_err();
        assert!(
            !matches!(err, SliceError::NothingToDo),
            "should get past the nothing-to-do check, got {err:?}"
        );
    }

    #[test]
    fn an_out_of_range_axis_value_is_reported_against_that_axis() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(5000.0);
        let err = job.run(&font).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("wght"), "got {message}");
    }

    #[test]
    fn restricting_one_axis_keeps_the_font_variable() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::range(300.0, 700.0);
        let output = job.run(&font).unwrap();

        let result = SliceFont::load(output.bytes).unwrap();
        assert!(result.is_variable(), "wght should survive");
        let axes = result.axes().unwrap();
        assert_eq!(axes.len(), 1, "the four pinned axes should be gone");
        assert_eq!(axes[0].tag, "wght");
        assert_eq!(
            (axes[0].min, axes[0].default, axes[0].max),
            (300.0, 300.0, 700.0)
        );
    }

    #[test]
    fn keeping_an_axis_whole_leaves_its_extent_alone() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Full;
        let output = job.run(&font).unwrap();

        let result = SliceFont::load(output.bytes).unwrap();
        let axes = result.axes().unwrap();
        assert_eq!(axes.len(), 1);
        assert_eq!(
            (axes[0].min, axes[0].default, axes[0].max),
            (300.0, 300.0, 1000.0)
        );
    }

    #[test]
    fn pinning_every_axis_produces_a_static_font() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);
        let output = job.run(&font).unwrap();

        let result = SliceFont::load(output.bytes).unwrap();
        assert!(!result.is_variable());
        assert_eq!(result.glyph_count(), font.glyph_count());
    }

    #[test]
    fn name_edits_reach_the_output() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);
        job.names.set(1, "Sliced Family");
        job.names.set(2, "Bold");
        job.names.set(16, "Typographic Family");

        let output = job.run(&font).unwrap();
        let result = SliceFont::load(output.bytes).unwrap();
        let edits = result.name_edits();
        assert_eq!(edits.get(1), Some("Sliced Family"));
        assert_eq!(edits.get(2), Some("Bold"));
        assert_eq!(edits.get(16), Some("Typographic Family"));
    }

    #[test]
    fn clearing_an_optional_name_removes_the_record() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);
        job.names.set(16, "Typographic Family");
        let with_record = SliceFont::load(job.run(&font).unwrap().bytes).unwrap();
        assert!(with_record.name_edits().get(16).is_some());

        job.names.set(16, "");
        let without = SliceFont::load(job.run(&font).unwrap().bytes).unwrap();
        assert!(
            without.name_edits().get(16).is_none(),
            "a cleared optional record should be deleted, not left behind"
        );
    }

    #[test]
    fn bit_flags_reach_the_output() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);
        job.bits.set_fs_selection_bit(5, true); // BOLD
        job.bits.set_fs_selection_bit(6, false); // not REGULAR
        job.bits.set_mac_style_bit(0, true); // BOLD

        let output = job.run(&font).unwrap();
        let result = SliceFont::load(output.bytes).unwrap();
        let bits = result.bit_flags();
        assert!(bits.fs_selection_bit(5));
        assert!(!bits.fs_selection_bit(6));
        assert!(bits.mac_style_bit(0));
        assert!(bits.warnings().is_empty(), "{:?}", bits.warnings());
    }

    #[test]
    fn woff_output_is_a_woff_file_that_reads_back() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);
        job.format = OutputFormat::Woff;

        let output = job.run(&font).unwrap();
        assert_eq!(&output.bytes[..4], b"wOFF");

        // And it round trips back to a usable font.
        let reopened = SliceFont::load(output.bytes).unwrap();
        assert_eq!(reopened.glyph_count(), font.glyph_count());
    }

    #[test]
    fn woff2_output_is_a_woff2_file_that_reads_back() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);
        job.format = OutputFormat::Woff2;

        let output = job.run(&font).unwrap();
        assert_eq!(&output.bytes[..4], b"wOF2");

        let reopened = SliceFont::load(output.bytes).unwrap();
        assert_eq!(reopened.glyph_count(), font.glyph_count());
    }

    #[test]
    fn woff2_output_is_smaller_than_woff_which_is_smaller_than_sfnt() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(700.0);

        let mut size_of = |format| {
            job.format = format;
            job.run(&font).unwrap().bytes.len()
        };
        let sfnt = size_of(OutputFormat::Sfnt);
        let woff = size_of(OutputFormat::Woff);
        let woff2 = size_of(OutputFormat::Woff2);
        assert!(
            woff2 < woff && woff < sfnt,
            "expected woff2 < woff < sfnt, got {woff2} < {woff} < {sfnt}"
        );
    }

    #[test]
    fn the_output_extension_follows_the_format() {
        assert_eq!(OutputFormat::Sfnt.extension(true), "ttf");
        assert_eq!(OutputFormat::Sfnt.extension(false), "otf");
        assert_eq!(OutputFormat::Woff.extension(true), "woff");
        assert_eq!(OutputFormat::Woff2.extension(true), "woff2");
        assert_eq!(OutputFormat::from_filename("Test.WOFF"), OutputFormat::Woff);
        assert_eq!(OutputFormat::from_filename("Test.ttf"), OutputFormat::Sfnt);
        // `.woff2` must not be mistaken for `.woff`, in either case.
        assert_eq!(
            OutputFormat::from_filename("Test.woff2"),
            OutputFormat::Woff2
        );
        assert_eq!(
            OutputFormat::from_filename("Test.WOFF2"),
            OutputFormat::Woff2
        );
        assert_eq!(OutputFormat::Woff2.mime_type(), "font/woff2");
    }

    #[test]
    fn pinning_then_removing_overlaps_runs_end_to_end() {
        let font = font();
        let mut job = pin_all(&font);
        job.limits[2] = AxisLimit::Pin(1000.0);
        job.remove_overlaps = true;

        let output = job.run(&font).unwrap();
        let report = output.overlaps.expect("a report should be produced");
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let result = SliceFont::load(output.bytes).unwrap();
        assert!(!result.is_variable());
        assert_eq!(result.glyph_count(), font.glyph_count());
    }
}
