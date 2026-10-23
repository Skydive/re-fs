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
}

impl FileKind {
    pub const ALL: [FileKind; 4] = [Self::Metadata, Self::Content, Self::Pdf, Self::Epub];

    /// The suffix after the UUID on the tablet, e.g. `.pdf`.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Metadata => ".metadata",
            Self::Content => ".content",
            Self::Pdf => ".pdf",
            Self::Epub => ".epub",
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

/// One item as the agent sends it: the raw JSON bytes, unparsed, plus file stats.
/// Keeping the agent ignorant of the JSON means format changes only touch the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub uuid: Uuid,
    pub metadata: Vec<u8>,
    pub content: Option<Vec<u8>>,
    pub files: Vec<FileStat>,
}

impl Entry {
    pub fn stat(&self, kind: FileKind) -> Option<&FileStat> {
        self.files.iter().find(|f| f.kind == kind)
    }
}
