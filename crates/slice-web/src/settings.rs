//! Everything the editors hold, in a form that survives a page reload.
//!
//! Two things want this. The address bar, so a slice can be bookmarked and come back with
//! the same axis values, names and flags filled in; and the recent-fonts store, so
//! choosing a font you have used before restores what you last did to it. Both need the
//! same thing written down, so it is written down once here.
//!
//! The font itself is never part of this. A URL cannot carry a 50 MB file, and would not
//! be a URL anyone could paste if it could. What travels is the *request* — which is
//! small, readable, and the part worth keeping.
//!
//! The query format is deliberately legible, because a bookmark that can be read and
//! edited by hand is worth more than a compact one:
//!
//! ```text
//! ?axes=wght=700,CASL=0:1&n1=Recursive%20Sans&n2=Bold&fs=32&mac=1&overlaps=1&format=woff2
//! ```
//!
//! `axes` carries exactly what was typed into the Axis Editor, tag and value, so the
//! syntax in the URL is the syntax in the interface. Unmentioned axes are left blank,
//! which is what an empty cell means anyway.

use slice_core::job::OutputFormat;
use slice_core::names::{NameEdits, NAME_EDITOR_IDS};
use slice_core::BitFlags;

/// The contents of the three editors and the output options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// `(tag, text)` for every axis whose cell is not empty.
    pub axes: Vec<(String, String)>,
    pub names: NameEdits,
    pub bits: BitFlags,
    pub remove_overlaps: bool,
    pub format: OutputFormat,
}

impl Settings {
    /// Serialise to a query string, without the leading `?`.
    pub fn to_query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if !self.axes.is_empty() {
            let joined = self
                .axes
                .iter()
                .map(|(tag, text)| format!("{tag}={text}"))
                .collect::<Vec<_>>()
                .join(",");
            // The separators are left as themselves: `=`, `,` and `:` are all legal in a
            // query value, and a URL that reads `axes=wght=200:700` is the point.
            parts.push(format!("axes={}", encode_keeping(&joined, "=,:")));
        }

        // Only the rows that were changed, and a cleared row as `n16=`: a link applied to
        // a font replaces the rows it carries and leaves the rest as the font has them.
        for (id, value) in self.names.explicit_rows() {
            parts.push(format!("n{id}={}", encode_keeping(value, "")));
        }

        // The raw fields, so bits the editor does not expose survive a round trip too.
        if self.bits.fs_selection != 0 {
            parts.push(format!("fs={}", self.bits.fs_selection));
        }
        if self.bits.mac_style != 0 {
            parts.push(format!("mac={}", self.bits.mac_style));
        }
        if self.remove_overlaps {
            parts.push("overlaps=1".into());
        }
        match self.format {
            OutputFormat::Sfnt => {}
            OutputFormat::Woff => parts.push("format=woff".into()),
            OutputFormat::Woff2 => parts.push("format=woff2".into()),
        }

        parts.join("&")
    }

    /// Parse a query string, with or without the leading `?`.
    ///
    /// Anything unrecognised is ignored rather than refused. A URL is not a file format:
    /// it gets truncated by chat clients, edited by hand and carried between versions,
    /// and the useful behaviour is to restore what can be understood.
    pub fn from_query(query: &str) -> Settings {
        let mut out = Settings::default();
        for pair in query.trim_start_matches('?').split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some(pair) => pair,
                None => continue,
            };
            let value = decode(value);
            match key {
                "axes" => {
                    for entry in value.split(',') {
                        if let Some((tag, text)) = entry.split_once('=') {
                            if !tag.is_empty() {
                                out.axes.push((tag.to_string(), text.to_string()));
                            }
                        }
                    }
                }
                "fs" => out.bits.fs_selection = value.parse().unwrap_or(0),
                "mac" => out.bits.mac_style = value.parse().unwrap_or(0),
                "overlaps" => out.remove_overlaps = value == "1" || value == "true",
                "format" => {
                    out.format = match value.as_str() {
                        "woff" => OutputFormat::Woff,
                        "woff2" => OutputFormat::Woff2,
                        _ => OutputFormat::Sfnt,
                    }
                }
                _ => {
                    if let Some(id) = key.strip_prefix('n').and_then(|n| n.parse::<u16>().ok()) {
                        if NAME_EDITOR_IDS.contains(&id) {
                            out.names.set(id, value);
                        }
                    }
                }
            }
        }
        out
    }

    /// True when there is nothing here worth putting in a URL.
    pub fn is_empty(&self) -> bool {
        *self == Settings::default()
    }
}

/// Read the settings out of the page's own address.
///
/// Returns the default when there is no query, which is the overwhelmingly common case
/// and must cost nothing.
pub fn from_location() -> Settings {
    let Some(window) = web_sys::window() else {
        return Settings::default();
    };
    let query = window.location().search().unwrap_or_default();
    if query.trim_start_matches('?').is_empty() {
        return Settings::default();
    }
    Settings::from_query(&query)
}

/// Write the settings into the address bar, without adding a history entry.
///
/// `replaceState` rather than `pushState`: slicing the same font five times while trying
/// weights should not put five entries in the back button. The address is a description
/// of the current state, not a place the user navigated to.
///
/// Failure is silent by design. This runs at the end of a successful slice, and a browser
/// that refuses the call -- some do, when the page came from a `file://` URL -- must not
/// turn a font the user already has into an error message.
pub fn write_to_location(settings: &Settings) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let query = settings.to_query();
    let path = window.location().pathname().unwrap_or_else(|_| "/".into());
    let url = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
}

/// Percent-encode, leaving the characters in `keep` alone.
///
/// Written out rather than pulled in, because the only alternative in this dependency
/// tree is `js_sys::encode_uri_component`, which escapes the separators that make the
/// axis list readable and cannot be told not to.
fn encode_keeping(text: &str, keep: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || "-_.~".contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            // A `+` means a space only in form encoding, which is what a browser produces
            // when it submits one. Nothing here produces `+` for anything else.
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        let mut names = NameEdits::new();
        names.set(1, "Recursive Sans");
        names.set(2, "Bold");
        Settings {
            axes: vec![("wght".into(), "700".into()), ("CASL".into(), "0:1".into())],
            names,
            bits: BitFlags {
                fs_selection: 32,
                mac_style: 1,
            },
            remove_overlaps: true,
            format: OutputFormat::Woff2,
        }
    }

    #[test]
    fn a_settings_round_trip_is_lossless() {
        let original = sample();
        let parsed = Settings::from_query(&original.to_query());
        assert_eq!(parsed, original);
    }

    #[test]
    fn the_regular_of_a_family_keeps_its_subfamily_through_its_own_link() {
        // What a user hit: the Regular's names, typed in, sliced, bookmarked by the app
        // and opened again. Its subfamily is never in the link -- the font already says
        // "Regular" -- and reopening the link used to blank it, so the slice came out
        // with an empty name ID 2 and Font Book refused the font.
        let mut font = NameEdits::new();
        font.set(1, "Google Sans Flex")
            .set(2, "Regular")
            .set(3, "4.005;GOOG;GoogleSansFlex-Regular")
            .set(4, "Google Sans Flex Regular")
            .set(6, "GoogleSansFlex-Regular")
            .set(16, "Google Sans Flex");
        let mut typed = font.clone();
        typed
            .set(1, "Google Sans Flex Condensed")
            .set(3, "4.005;GOOG;GoogleSansFlex-Condensed-Regular")
            .set(4, "Google Sans Flex Condensed Regular")
            .set(6, "GoogleSansFlex-Condensed-Regular")
            .set(16, "");

        let link = Settings {
            names: typed.changes_from(&font),
            ..Settings::default()
        }
        .to_query();
        assert!(
            !link.contains("n2="),
            "an unchanged row has no business in a link: {link}"
        );
        assert!(
            link.contains("n16=&") || link.ends_with("n16="),
            "a cleared row is said: {link}"
        );

        let reopened = font.with_changes(&Settings::from_query(&link).names);
        assert_eq!(reopened.get(2), Some("Regular"));
        assert_eq!(
            reopened.get(16),
            None,
            "and the cleared row is cleared again"
        );
        assert_eq!(reopened, typed);
    }

    #[test]
    fn the_axis_list_stays_readable_in_the_url() {
        // The whole reason for hand-rolling the encoder: a bookmark someone can read.
        let query = sample().to_query();
        assert!(query.contains("axes=wght=700,CASL=0:1"), "{query}");
    }

    #[test]
    fn a_space_in_a_name_survives() {
        let parsed = Settings::from_query(&sample().to_query());
        assert_eq!(parsed.names.get(1), Some("Recursive Sans"));
    }

    #[test]
    fn awkward_characters_in_a_name_survive() {
        let mut names = NameEdits::new();
        // Ampersand and equals would end the field; the em dash is multi-byte.
        names.set(4, "A & B = C — ü");
        let settings = Settings {
            names,
            ..Settings::default()
        };
        let parsed = Settings::from_query(&settings.to_query());
        assert_eq!(parsed.names.get(4), Some("A & B = C — ü"));
    }

    #[test]
    fn an_empty_settings_makes_an_empty_query() {
        assert_eq!(Settings::default().to_query(), "");
        assert!(Settings::default().is_empty());
    }

    #[test]
    fn nonsense_is_ignored_rather_than_refused() {
        // A URL gets truncated and hand-edited; restoring what parses beats refusing.
        let parsed = Settings::from_query("?axes=wght=400&nonsense&n99=x&fs=notanumber&n1=Keep");
        assert_eq!(parsed.axes, vec![("wght".to_string(), "400".to_string())]);
        assert_eq!(parsed.names.get(1), Some("Keep"));
        assert_eq!(parsed.bits.fs_selection, 0);
    }

    #[test]
    fn a_name_id_the_editor_does_not_show_is_dropped() {
        // n99 is not an editable row; accepting it would write a record the interface
        // cannot display and the user cannot undo.
        let parsed = Settings::from_query("n99=ghost&n1=real");
        assert_eq!(parsed.names.get(1), Some("real"));
        assert_eq!(parsed.names.get(99), None);
    }

    #[test]
    fn unexposed_bits_survive_the_round_trip() {
        // fsSelection bit 7 (WWS) is not one of the six the editor shows, and clobbering
        // it is the original Slice's E4 defect. The URL carries the whole field.
        let settings = Settings {
            bits: BitFlags {
                fs_selection: 1 << 7,
                mac_style: 0,
            },
            ..Settings::default()
        };
        assert_eq!(
            Settings::from_query(&settings.to_query()).bits.fs_selection,
            1 << 7
        );
    }
}
