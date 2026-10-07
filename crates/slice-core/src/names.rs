//! The Name Editor: the nine `name` table records Slice lets you rewrite.

use std::collections::BTreeMap;

/// The nameIDs the Name Editor shows, in editor row order.
pub const NAME_EDITOR_IDS: &[u16] = &[1, 2, 3, 4, 6, 16, 17, 21, 22];

/// nameIDs that must always be present in the output.
///
/// The original writes these unconditionally, even when the user blanked the field.
pub const MANDATORY_IDS: &[u16] = &[1, 2, 3, 4, 6];

/// nameIDs that are written when non-empty and *deleted* when the user clears them.
pub const OPTIONAL_IDS: &[u16] = &[16, 17, 21, 22];

/// The row label shown at the left of the Name Editor.
pub fn row_label(name_id: u16) -> &'static str {
    match name_id {
        1 => "01 Family",
        2 => "02 Subfamily",
        3 => "03 Unique",
        4 => "04 Full",
        6 => "06 Postscript",
        16 => "16 Typo Family",
        17 => "17 Typo Subfamily",
        21 => "21 WWS Family",
        22 => "22 WWS Subfamily",
        _ => "",
    }
}

/// A longer explanation, shown as a tooltip. The original offers no such hint; the
/// nameID numbers alone are opaque unless you already know the `name` table.
pub fn row_hint(name_id: u16) -> &'static str {
    match name_id {
        1 => "Font Family name. Limited to four styles per family by legacy systems.",
        2 => "Font Subfamily name. Regular, Italic, Bold or Bold Italic.",
        3 => "Unique font identifier.",
        4 => "Full font name, usually Family plus Subfamily.",
        6 => "PostScript name. No spaces, at most 63 characters.",
        16 => "Typographic Family name, when the family has more than four styles.",
        17 => "Typographic Subfamily name, when the family has more than four styles.",
        21 => "WWS Family name, for families that vary beyond weight/width/slope.",
        22 => "WWS Subfamily name, for families that vary beyond weight/width/slope.",
        _ => "",
    }
}

/// The contents of the Name Editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameEdits {
    values: BTreeMap<u16, String>,
}

impl NameEdits {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name_id: u16, value: impl Into<String>) -> &mut Self {
        self.values.insert(name_id, value.into());
        self
    }

    /// The editor cell contents, or `None` when the row is blank.
    pub fn get(&self, name_id: u16) -> Option<&str> {
        self.values
            .get(&name_id)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// The editor cell contents, treating a missing row as blank.
    pub fn get_or_empty(&self, name_id: u16) -> &str {
        self.values.get(&name_id).map(String::as_str).unwrap_or("")
    }

    /// Rows in editor order, as `(nameID, text)`.
    pub fn rows(&self) -> impl Iterator<Item = (u16, &str)> {
        NAME_EDITOR_IDS
            .iter()
            .map(move |&id| (id, self.get_or_empty(id)))
    }

    /// The rows held explicitly, in editor order -- an explicitly emptied row included,
    /// a row never set left out. This is what a link carries.
    pub fn explicit_rows(&self) -> impl Iterator<Item = (u16, &str)> {
        NAME_EDITOR_IDS
            .iter()
            .filter_map(move |&id| self.values.get(&id).map(|text| (id, text.as_str())))
    }

    /// The rows of these edits that differ from `original`, for a link to carry.
    ///
    /// A row the user cleared is kept as an explicit empty string, so the link can say
    /// "clear this" rather than say nothing about it: clearing an optional row is how a
    /// record gets deleted.
    pub fn changes_from(&self, original: &NameEdits) -> NameEdits {
        let mut out = NameEdits::new();
        for &id in NAME_EDITOR_IDS {
            let now = self.get_or_empty(id);
            if now != original.get_or_empty(id) {
                out.set(id, now);
            }
        }
        out
    }

    /// These rows, with every row `changes` holds explicitly replaced -- an explicit empty
    /// string included. A row `changes` does not mention keeps its value here.
    ///
    /// This is how a link is applied to the font it is opened with. A link carries only
    /// the rows that were changed, so a row it leaves out means "as the font has it", not
    /// "blank". Applying a link used to replace all nine rows with the link's, which
    /// blanked every row the user had left alone. For a family's Regular that row is the
    /// subfamily, "Regular" -- the one name its link never carries, because the font
    /// already says it -- and a font with an empty name ID 2 is what Font Book rejects
    /// as "'name' table structure".
    pub fn with_changes(&self, changes: &NameEdits) -> NameEdits {
        let mut out = self.clone();
        for (&id, text) in &changes.values {
            out.values.insert(id, text.clone());
        }
        out
    }

    /// The records to write, and the records to delete, when applying these edits.
    ///
    /// Mandatory IDs are always written. Optional IDs are written when the user typed
    /// something and removed when they cleared the field, which is how the original
    /// distinguishes "leave it alone" from "take it out".
    pub fn plan(&self) -> (Vec<(u16, String)>, Vec<u16>) {
        let mut writes = Vec::new();
        let mut deletes = Vec::new();

        for &id in MANDATORY_IDS {
            writes.push((id, self.get_or_empty(id).to_string()));
        }
        for &id in OPTIONAL_IDS {
            match self.get(id) {
                Some(text) => writes.push((id, text.to_string())),
                None => deletes.push(id),
            }
        }
        (writes, deletes)
    }

    /// True when no row has any text in it.
    pub fn is_empty(&self) -> bool {
        NAME_EDITOR_IDS.iter().all(|&id| self.get(id).is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_in_editor_order() {
        let edits = NameEdits::new();
        let ids: Vec<_> = edits.rows().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 6, 16, 17, 21, 22]);
    }

    #[test]
    fn blank_optional_rows_become_deletions() {
        let mut edits = NameEdits::new();
        edits.set(1, "Test Family").set(16, "Typo Family");
        let (writes, deletes) = edits.plan();

        // Every mandatory ID is written, even the ones left blank.
        for &id in MANDATORY_IDS {
            assert!(
                writes.iter().any(|(w, _)| *w == id),
                "missing write for {id}"
            );
        }
        assert!(writes.contains(&(16, "Typo Family".to_string())));
        // 17, 21 and 22 were never filled in, so they come out.
        assert_eq!(deletes, vec![17, 21, 22]);
    }

    #[test]
    fn whitespace_only_is_still_text() {
        // The original treats only the empty string as "delete this record", so a space
        // is a real (if odd) value. Keep that behaviour rather than trimming silently.
        let mut edits = NameEdits::new();
        edits.set(16, " ");
        let (writes, deletes) = edits.plan();
        assert!(writes.contains(&(16, " ".to_string())));
        assert!(!deletes.contains(&16));
    }

    /// Google Sans Flex's own names, as the Name Editor loads them.
    fn google_sans_flex() -> NameEdits {
        let mut font = NameEdits::new();
        font.set(1, "Google Sans Flex")
            .set(2, "Regular")
            .set(3, "4.005;GOOG;GoogleSansFlex-Regular")
            .set(4, "Google Sans Flex Regular")
            .set(6, "GoogleSansFlex-Regular");
        font
    }

    /// The names a user types for the Regular of a condensed family: everything changes
    /// except the subfamily, which is already "Regular".
    fn condensed_regular() -> NameEdits {
        let mut edits = google_sans_flex();
        edits
            .set(1, "Google Sans Flex Condensed")
            .set(3, "4.005;GOOG;GoogleSansFlex-Condensed-Regular")
            .set(4, "Google Sans Flex Condensed Regular")
            .set(6, "GoogleSansFlex-Condensed-Regular");
        edits
    }

    #[test]
    fn a_row_the_link_leaves_out_keeps_the_fonts_value() {
        // The bug a user hit: the link for the Regular carries no subfamily, because it
        // matches the font, and reopening the link used to blank it -- an empty name
        // ID 2, which Font Book reports as "'name' table structure".
        let font = google_sans_flex();
        let link = condensed_regular().changes_from(&font);
        assert_eq!(link.get(2), None, "an unchanged row is not carried");

        let restored = font.with_changes(&link);
        assert_eq!(restored.get(2), Some("Regular"));
        assert_eq!(restored, condensed_regular());
    }

    #[test]
    fn a_cleared_row_is_carried_and_cleared_again() {
        // Clearing an optional row deletes the record, so a link has to be able to say
        // "clear this", not merely say nothing about the row.
        let mut font = google_sans_flex();
        font.set(16, "Google Sans Flex");
        let mut edits = font.clone();
        edits.set(16, "");

        let link = edits.changes_from(&font);
        assert_eq!(link.explicit_rows().collect::<Vec<_>>(), vec![(16, "")]);

        let restored = font.with_changes(&link);
        assert_eq!(restored.get(16), None);
        assert!(
            restored.plan().1.contains(&16),
            "the record is still deleted"
        );
    }

    #[test]
    fn nothing_changed_means_nothing_to_carry() {
        let font = google_sans_flex();
        let link = font.changes_from(&font);
        assert_eq!(link.explicit_rows().count(), 0);
        assert_eq!(font.with_changes(&link), font);
    }
}
