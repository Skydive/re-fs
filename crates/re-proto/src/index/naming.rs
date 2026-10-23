//! Turning a tablet `visibleName` into a filename, and back.

/// Shown in place of `/`, which can't appear in a filename. U+2215 DIVISION SLASH.
pub const SLASH_STANDIN: char = '∕';
/// Linux NAME_MAX, in bytes.
pub const NAME_MAX: usize = 255;

/// The filename for an item: `/` replaced, NUL dropped, `.`/`..`/empty made safe,
/// `ext` (e.g. ".pdf", or "" for folders) appended unless the name already ends with
/// it (imported files often keep "x.pdf" as their name), and the whole thing kept
/// within NAME_MAX bytes without splitting a character or losing the extension.
pub fn file_name(visible_name: &str, ext: &str) -> String {
    let mut base: String = visible_name
        .chars()
        .filter(|&c| c != '\0')
        .map(|c| if c == '/' { SLASH_STANDIN } else { c })
        .collect();
    if base.is_empty() {
        base = "Untitled".into();
    } else if ext.is_empty() && (base == "." || base == "..") {
        // A folder literally named "." or ".." would be unreachable.
        base = base.replace('.', "․"); // U+2024 ONE DOT LEADER
    }
    if let Some(stripped) = strip_ext(&base, ext) {
        base.truncate(stripped.len());
    }
    truncate_base(&mut base, NAME_MAX - ext.len());
    base + ext
}

/// `name` without `ext`, if it ends with it (ASCII case-insensitive).
pub fn strip_ext<'a>(name: &'a str, ext: &str) -> Option<&'a str> {
    let cut = name.len().checked_sub(ext.len())?;
    (!ext.is_empty() && name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(ext)).then(|| &name[..cut])
}

/// The visible name for a filename the user typed: the inverse of `file_name`,
/// minus the extension the caller already stripped.
pub fn visible_name(file_base: &str) -> String {
    file_base.replace(SLASH_STANDIN, "/")
}

/// The `n`th name for a clash (n ≥ 2): "Notes (2).pdf", or "Books (2)" for folders.
pub fn numbered(name: &str, ext: &str, n: u32) -> String {
    let base = strip_ext(name, ext).unwrap_or(name);
    let suffix = format!(" ({n})");
    let mut base = base.to_owned();
    truncate_base(&mut base, NAME_MAX - ext.len() - suffix.len());
    base + &suffix + ext
}

fn truncate_base(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slashes_and_extensions() {
        assert_eq!(file_name("01/02/24", ".pdf"), "01∕02∕24.pdf");
        assert_eq!(file_name("A/B", ""), "A∕B");
        assert_eq!(visible_name("01∕02∕24"), "01/02/24");
    }

    #[test]
    fn keeps_spaces_colons_and_unicode() {
        assert_eq!(file_name("03 Physics ", ""), "03 Physics ");
        assert_eq!(file_name("Chapter 1: Intro & Notes", ""), "Chapter 1: Intro & Notes");
        assert_eq!(file_name("«Vie»\u{a0}– l’été…", ".epub"), "«Vie»\u{a0}– l’été….epub");
    }

    #[test]
    fn degenerate_names() {
        assert_eq!(file_name("", ".pdf"), "Untitled.pdf");
        assert_eq!(file_name("..", ""), "․․");
        assert_eq!(file_name("..", ".pdf"), "...pdf");
        assert_eq!(file_name("a\0b", ""), "ab");
    }

    #[test]
    fn long_names_fit_and_keep_extension() {
        let long = "é".repeat(200); // 400 bytes
        let n = file_name(&long, ".pdf");
        assert!(n.len() <= NAME_MAX);
        assert!(n.ends_with(".pdf"));
        let m = numbered(&n, ".pdf", 12);
        assert!(m.len() <= NAME_MAX);
        assert!(m.ends_with(" (12).pdf"));
    }

    #[test]
    fn existing_extension_not_doubled() {
        assert_eq!(file_name("EPI (2015).pdf", ".pdf"), "EPI (2015).pdf");
        assert_eq!(file_name("Scan.PDF", ".pdf"), "Scan.pdf");
        assert_eq!(file_name("Book.epub", ".pdf"), "Book.epub.pdf");
        assert_eq!(file_name(".pdf", ".pdf"), ".pdf");
        assert_eq!(numbered("Scan.PDF", ".pdf", 2), "Scan (2).pdf");
        assert_eq!(strip_ext("é", ".pdf"), None);
    }

    #[test]
    fn numbering() {
        assert_eq!(numbered("Notes.pdf", ".pdf", 2), "Notes (2).pdf");
        assert_eq!(numbered("Books", "", 3), "Books (3)");
    }
}
