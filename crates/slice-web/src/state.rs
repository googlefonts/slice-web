//! The application's state, and the rules for keeping it consistent.

use leptos::prelude::*;
use slice_core::axes::{parse_axis_limit, AxisLimit, AxisSpec};
use slice_core::{BitFlags, NameEdits, OutputFormat, SliceFont, SliceJob};

/// A modal error, matching the original's error dialog: a sentence, plus details behind
/// a disclosure.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorMessage {
    pub summary: String,
    pub details: Option<String>,
}

/// Everything the UI reads and writes.
///
/// `Copy`, because Leptos signals are handles rather than values, so the whole struct can
/// be handed to any component without cloning anything real.
#[derive(Clone, Copy)]
pub struct AppState {
    /// The loaded font, if any.
    pub font: RwSignal<Option<SliceFont>>,
    /// The name of the file the user opened, used to suggest an output name.
    pub file_name: RwSignal<String>,
    /// The font's axes, in `fvar` order.
    pub axes: RwSignal<Vec<AxisSpec>>,
    /// The Axis Editor's raw text, one entry per axis. Kept as typed rather than parsed
    /// so an in-progress entry like `200:` is not thrown away mid-keystroke.
    pub axis_text: RwSignal<Vec<String>>,
    /// The Name Editor's contents.
    pub names: RwSignal<NameEdits>,
    /// The Bit Flag Editor's contents.
    pub bits: RwSignal<BitFlags>,
    /// Merge overlapping contours.
    pub remove_overlaps: RwSignal<bool>,
    pub format: RwSignal<OutputFormat>,

    pub status: RwSignal<String>,
    pub error: RwSignal<Option<ErrorMessage>>,
    pub about_open: RwSignal<bool>,
    /// True while a slice is running, which shows the progress dialog.
    pub busy: RwSignal<bool>,
    /// The notes from the last successful slice.
    pub last_result: RwSignal<Vec<String>>,
    /// Fonts opened before, most recently used first. Lives here rather than in `App` so
    /// that the slice path, which is a free function, can refresh it after storing one.
    pub recent: RwSignal<Vec<crate::recent::RecentFont>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            font: RwSignal::new(None),
            file_name: RwSignal::new(String::new()),
            axes: RwSignal::new(Vec::new()),
            axis_text: RwSignal::new(Vec::new()),
            names: RwSignal::new(NameEdits::new()),
            bits: RwSignal::new(BitFlags::default()),
            remove_overlaps: RwSignal::new(false),
            format: RwSignal::new(OutputFormat::Sfnt),
            status: RwSignal::new("Ready".to_string()),
            error: RwSignal::new(None),
            about_open: RwSignal::new(false),
            busy: RwSignal::new(false),
            last_result: RwSignal::new(Vec::new()),
            recent: RwSignal::new(Vec::new()),
        }
    }

    /// Take on a newly opened font, filling in all three editors.
    pub fn load_font(&self, name: String, bytes: Vec<u8>) {
        let font = match SliceFont::load(bytes) {
            Ok(font) => font,
            Err(e) => {
                self.report(
                    "An error was encountered during the attempt to load your font. \
                     See details below.",
                    Some(e.to_string()),
                );
                return;
            }
        };

        if !font.is_variable() {
            self.report(
                "The file does not appear to be a variable font. See details below.",
                Some(
                    "The font is missing the OpenType fvar table and is not recognized \
                     as a variable font. Please try again with a font that includes the \
                     fvar table."
                        .into(),
                ),
            );
            return;
        }

        let axes = match font.axes() {
            Ok(axes) => axes,
            Err(e) => {
                self.report("The font's axes could not be read.", Some(e.to_string()));
                return;
            }
        };

        let summary = format!(
            "{} {} loaded ({} {})",
            font.family_name().unwrap_or_else(|| "Unnamed".into()),
            font.version().unwrap_or_default(),
            axes.len(),
            if axes.len() == 1 { "axis" } else { "axes" },
        );

        self.axis_text.set(vec![String::new(); axes.len()]);
        self.axes.set(axes);
        self.names.set(font.name_edits());
        self.bits.set(font.bit_flags());
        self.last_result.set(Vec::new());
        self.file_name.set(name);
        self.font.set(Some(font));
        self.status.set(summary);
        self.error.set(None);
    }

    pub fn report(&self, summary: &str, details: Option<String>) {
        self.error.set(Some(ErrorMessage {
            summary: summary.to_string(),
            details,
        }));
        self.status.set("Error".to_string());
    }

    pub fn clear_error(&self) {
        self.error.set(None);
    }

    /// Parse one Axis Editor cell, for live feedback as the user types.
    ///
    /// Returns `Ok(None)` for a cell that is blank, and an error only when what is there
    /// cannot be understood; an entry that is merely incomplete reads as an error too,
    /// which is why this is used for a hint rather than to block anything.
    pub fn axis_entry_problem(&self, index: usize) -> Option<String> {
        let axes = self.axes.get();
        let text = self.axis_text.get();
        let (axis, entry) = (axes.get(index)?, text.get(index)?);
        if entry.trim().is_empty() {
            return None;
        }
        match parse_axis_limit(entry, &axis.tag).and_then(|limit| axis.validate(limit)) {
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        }
    }

    /// Turn the current editor state into a job.
    pub fn build_job(&self) -> Result<SliceJob, slice_core::SliceError> {
        let axes = self.axes.get();
        let text = self.axis_text.get();
        let mut limits = Vec::with_capacity(axes.len());
        for (index, axis) in axes.iter().enumerate() {
            let entry = text.get(index).map(String::as_str).unwrap_or("");
            limits.push(parse_axis_limit(entry, &axis.tag)?);
        }
        Ok(SliceJob {
            limits,
            names: self.changed_names(),
            bits: self.changed_bits(),
            remove_overlaps: self.remove_overlaps.get(),
            format: self.format.get(),
        })
    }

    /// A sensible file name for the result, derived from the input and the settings.
    ///
    /// `Recursive-VF.ttf` sliced at `wght=800` becomes `Recursive-VF-wght800.ttf`, so a
    /// folder full of instances is self-describing.
    pub fn suggested_output_name(&self) -> String {
        let input = self.file_name.get();
        let stem = input
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(&input);
        let stem = if stem.is_empty() { "sliced" } else { stem };

        let axes = self.axes.get();
        let text = self.axis_text.get();
        let mut parts = Vec::new();
        for (index, axis) in axes.iter().enumerate() {
            let entry = text.get(index).map(String::as_str).unwrap_or("");
            match parse_axis_limit(entry, &axis.tag) {
                Ok(AxisLimit::Pin(v)) => parts.push(format!("{}{}", axis.tag, trim_number(v))),
                Ok(AxisLimit::Range { min, max, .. }) => parts.push(format!(
                    "{}{}-{}",
                    axis.tag,
                    trim_number(min),
                    trim_number(max)
                )),
                _ => {}
            }
        }

        let truetype = self
            .font
            .with(|f| f.as_ref().map(|f| f.is_truetype()))
            .unwrap_or(true);
        let extension = self.format.get().extension(truetype);

        if parts.is_empty() {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{}.{extension}", parts.join("-"))
        }
    }

    /// Everything the editors hold, in the form the URL and the recent-fonts store keep.
    pub fn capture_settings(&self) -> crate::settings::Settings {
        let axes = self.axes.get();
        let text = self.axis_text.get();
        crate::settings::Settings {
            axes: axes
                .iter()
                .enumerate()
                .filter_map(|(index, axis)| {
                    let entry = text.get(index)?.trim();
                    // A blank cell is the default, so recording it would only make the
                    // URL longer and no more expressive.
                    (!entry.is_empty()).then(|| (axis.tag.clone(), entry.to_string()))
                })
                .collect(),
            names: self.changed_names(),
            bits: self.changed_bits(),
            remove_overlaps: self.remove_overlaps.get(),
            format: self.format.get(),
        }
    }

    /// Only the name rows the user actually altered.
    ///
    /// The Name Editor is prefilled from the font, so capturing it wholesale would put
    /// five records into every URL and leave a bookmark nobody can read -- and a bookmark
    /// nobody can read is most of the value gone. A row that still matches the font is
    /// restored by loading the font, so it does not need carrying.
    ///
    /// A row the user *cleared* is a change, and is carried as an empty value so that
    /// applying the settings clears it again rather than letting the font's own value
    /// reappear.
    fn changed_names(&self) -> NameEdits {
        let current = self.names.get();
        let Some(original) = self.font.with(|font| font.as_ref().map(|f| f.name_edits())) else {
            return current;
        };
        current.changes_from(&original)
    }

    /// The bit fields, or zero when they still match the font.
    ///
    /// Same reasoning: `fs=192` in every URL is noise when 192 is simply what the font
    /// says. Zero is the "nothing to say" value, and a user who genuinely clears every
    /// exposed bit is recorded by the fields differing from the font's.
    fn changed_bits(&self) -> BitFlags {
        let current = self.bits.get();
        let Some(original) = self.font.with(|font| font.as_ref().map(|f| f.bit_flags())) else {
            return current;
        };
        if current == original {
            BitFlags::default()
        } else {
            current
        }
    }

    /// Fill the editors in from saved settings, against whatever font is loaded now.
    ///
    /// Axes are matched by tag, and one the font does not have is dropped rather than
    /// reported: a bookmark made from one font and opened with another should restore
    /// what still applies instead of refusing the whole thing. Names and flags are
    /// applied as given, since they are the user's text rather than the font's.
    pub fn apply_settings(&self, settings: &crate::settings::Settings) {
        let axes = self.axes.get();
        let mut text = vec![String::new(); axes.len()];
        for (tag, entry) in &settings.axes {
            if let Some(index) = axes.iter().position(|axis| &axis.tag == tag) {
                text[index] = entry.clone();
            }
        }
        self.axis_text.set(text);

        // A link carries only the rows that were changed, so it is laid over the font's
        // own names rather than replacing them. Replacing them blanked every row the
        // link left out -- for a family's Regular, the subfamily "Regular", since that is
        // what the font already says -- and an empty name ID 2 fails Font Book.
        if let Some(own) = self.font.with(|font| font.as_ref().map(|f| f.name_edits())) {
            self.names.set(own.with_changes(&settings.names));
        }
        if settings.bits != slice_core::BitFlags::default() {
            self.bits.set(settings.bits);
        }
        self.remove_overlaps.set(settings.remove_overlaps);
        self.format.set(settings.format);
    }

    /// Reset the Name Editor and Bit Flag Editor to what the font actually contains.
    pub fn revert_editors(&self) {
        self.font.with(|font| {
            if let Some(font) = font {
                self.names.set(font.name_edits());
                self.bits.set(font.bit_flags());
            }
        });
    }
}

/// Format a number for a file name: `800` rather than `800.0`, `0.5` kept as is.
fn trim_number(value: f64) -> String {
    if value == value.trunc() {
        format!("{}", value as i64)
    } else {
        format!("{value}").replace('.', "_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_drop_trailing_zeros_and_keep_fractions_readable() {
        assert_eq!(trim_number(800.0), "800");
        assert_eq!(trim_number(-15.0), "-15");
        assert_eq!(trim_number(0.5), "0_5");
    }
}
