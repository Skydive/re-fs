use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The files of one item that re-fs cares about. Paths on the tablet are only ever
/// built from a parsed UUID plus one of these, never from a client-supplied string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileKind {
    Metadata,
    Content,
    Pdf,
    Epub,
    /// Template name per page.
    Pagedata,
}

impl FileKind {
    pub const ALL: [FileKind; 5] = [Self::Metadata, Self::Content, Self::Pdf, Self::Epub, Self::Pagedata];

    /// The suffix after the UUID on the tablet, e.g. `.pdf`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Metadata => ".metadata",
            Self::Content => ".content",
            Self::Pdf => ".pdf",
            Self::Epub => ".epub",
            Self::Pagedata => ".pagedata",
        }
    }

    pub fn from_suffix(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.suffix() == s)
    }
}

/// Size and modification time (ms since the epoch) of one file on the tablet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: i64,
}

/// A file in a document's page folder (`<uuid>/`): `<page-id>.rm` strokes or
/// `<page-id>-metadata.json` (layer names).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageFile {
    pub name: String,
    pub size: u64,
    pub mtime_ms: i64,
}

/// Whether `name` may be a file in a page folder. Anything else is never listed
/// or served, so a request can't name a path outside the folder.
pub fn is_page_file(name: &str) -> bool {
    let id_ok = |id: &str| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    name.strip_suffix(".rm").is_some_and(id_ok) || name.strip_suffix("-metadata.json").is_some_and(id_ok)
}

/// One item as the agent sends it: the raw JSON bytes, unparsed, plus file stats.
/// Keeping the agent ignorant of the JSON means format changes only touch the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub uuid: Uuid,
    pub metadata: Vec<u8>,
    pub content: Option<Vec<u8>>,
    pub files: Vec<FileStat>,
    /// The page folder's files, sorted by name.
    pub pages: Vec<PageFile>,
}

impl Entry {
    pub fn stat(&self, kind: FileKind) -> Option<&FileStat> {
        self.files.iter().find(|f| f.kind == kind)
    }

    pub fn page(&self, name: &str) -> Option<&PageFile> {
        self.pages.iter().find(|p| p.name == name)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn page_file_names() {
        use super::is_page_file;
        assert!(is_page_file("0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f.rm"));
        assert!(is_page_file("0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f-metadata.json"));
        assert!(!is_page_file("../../etc/passwd"));
        assert!(!is_page_file("0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f.rm/../x"));
        assert!(!is_page_file("x.rm"));
    }
}
