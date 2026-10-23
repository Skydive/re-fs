//! reMarkable stroke files to something you can read: a page model parsed from
//! `.rm` files (v5 and v6), PDF output for notebooks and annotated PDFs, and
//! highlights as Markdown. Pure: bytes in, bytes out.

pub mod attach;
pub mod highlights;
pub mod model;
pub mod pdf;
mod reader;
pub mod v5;
pub mod v6;

pub use model::{Color, Highlight, Line, Page, Point, Tool};

/// Page size in tablet pixels (226 dpi).
pub const PAGE_W: f32 = 1404.0;
pub const PAGE_H: f32 = 1872.0;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a reMarkable .lines file")]
    NotLines,
    #[error("unsupported .lines version {0}")]
    Version(u32),
    #[error("truncated or malformed .rm data at byte {0}")]
    Malformed(usize),
    #[error("PDF: {0}")]
    Pdf(#[from] lopdf::Error),
}

/// Parse a `.rm` page of either version.
pub fn parse(bytes: &[u8]) -> Result<Page, Error> {
    const PREFIX: &[u8] = b"reMarkable .lines file, version=";
    if bytes.len() < 43 || !bytes.starts_with(PREFIX) {
        return Err(Error::NotLines);
    }
    match bytes[PREFIX.len()] {
        b'6' => v6::parse(bytes),
        b'5' => v5::parse(bytes),
        b'3' => v5::parse(bytes), // v3 is v5 without the per-line padding word
        d => Err(Error::Version(u32::from(d.wrapping_sub(b'0')))),
    }
}
