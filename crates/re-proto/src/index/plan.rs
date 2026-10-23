//! Filesystem actions → tablet file writes. Pure functions over the raw JSON, so
//! every mapping is unit-tested; the agent only applies the resulting `Op`s.
//!
//! | action                 | tablet change                                          |
//! |------------------------|--------------------------------------------------------|
//! | mkdir                  | new `.metadata` (CollectionType) + `.content`          |
//! | copy a PDF/EPUB in     | staged file → `.pdf`/`.epub`, new `.content` + `.metadata` |
//! | rename / move          | `.metadata`: `visibleName`, `parent`                   |
//! | rm / rmdir             | `.metadata`: `parent = "trash"` (recoverable)          |

use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::entry::FileKind;
use super::parse::ParentRef;
use crate::wire::Op;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Epub,
}

impl DocKind {
    pub fn file(self) -> FileKind {
        match self {
            DocKind::Pdf => FileKind::Pdf,
            DocKind::Epub => FileKind::Epub,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            DocKind::Pdf => ".pdf",
            DocKind::Epub => ".epub",
        }
    }

    /// From a filename's extension (case-insensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".pdf") {
            Some(DocKind::Pdf)
        } else if lower.ends_with(".epub") {
            Some(DocKind::Epub)
        } else {
            None
        }
    }

    /// Whether `head` (the first bytes of a file) looks like this kind of file.
    pub fn sniff(self, head: &[u8]) -> bool {
        match self {
            DocKind::Pdf => head.starts_with(b"%PDF-"),
            // A zip whose first entry is the stored "mimetype" file.
            DocKind::Epub => head.starts_with(b"PK\x03\x04") && head.get(30..58) == Some(b"mimetypeapplication/epub+zip"),
        }
    }
}

pub fn parent_str(p: &ParentRef) -> String {
    match p {
        ParentRef::Root => String::new(),
        ParentRef::Trash => "trash".into(),
        ParentRef::Folder(u) => u.to_string(),
        ParentRef::Invalid(s) => s.clone(),
    }
}

fn pretty(v: &Value) -> Vec<u8> {
    // xochitl writes 4-space indented JSON; serde's default pretty printer uses 2,
    // which xochitl reads just as well.
    serde_json::to_vec_pretty(v).expect("serialising JSON")
}

/// A new folder.
pub fn new_folder(uuid: Uuid, parent: &ParentRef, name: &str, now_ms: i64) -> Vec<Op> {
    let meta = json!({
        "createdTime": now_ms.to_string(),
        "deleted": false,
        "lastModified": now_ms.to_string(),
        "metadatamodified": true,
        "modified": true,
        "parent": parent_str(parent),
        "pinned": false,
        "synced": false,
        "type": "CollectionType",
        "version": 0,
        "visibleName": name,
    });
    vec![
        Op::PutJson { uuid, file: FileKind::Content, json: pretty(&json!({ "tags": [] })) },
        Op::PutJson { uuid, file: FileKind::Metadata, json: pretty(&meta) },
    ]
}

/// A new document from an upload staged under `token`. xochitl fills in the rest of
/// `.content` (page list, page count) when it next starts.
pub fn new_document(uuid: Uuid, parent: &ParentRef, name: &str, kind: DocKind, token: u64, size: u64, now_ms: i64) -> Vec<Op> {
    let content = json!({
        "coverPageNumber": 0,
        "extraMetadata": {},
        "fileType": match kind { DocKind::Pdf => "pdf", DocKind::Epub => "epub" },
        "pageCount": 0,
        "sizeInBytes": size.to_string(),
        "tags": [],
        "textScale": 1,
    });
    let meta = json!({
        "createdTime": now_ms.to_string(),
        "lastModified": now_ms.to_string(),
        "lastOpened": "0",
        "lastOpenedPage": 0,
        "new": true,
        "parent": parent_str(parent),
        "pinned": false,
        "source": "",
        "type": "DocumentType",
        "visibleName": name,
    });
    vec![
        Op::PutStaged { uuid, file: kind.file(), token },
        Op::PutJson { uuid, file: FileKind::Content, json: pretty(&content) },
        Op::PutJson { uuid, file: FileKind::Metadata, json: pretty(&meta) },
    ]
}

/// An item's `.metadata` with a new parent and/or name. Every other field is kept;
/// the bookkeeping fields xochitl and sync use are updated.
pub fn edit_metadata(raw: &[u8], parent: Option<&ParentRef>, name: Option<&str>, now_ms: i64) -> serde_json::Result<Vec<u8>> {
    let mut v: Map<String, Value> = serde_json::from_slice(raw)?;
    if let Some(p) = parent {
        v.insert("parent".into(), Value::String(parent_str(p)));
    }
    if let Some(n) = name {
        v.insert("visibleName".into(), Value::String(n.into()));
    }
    v.insert("lastModified".into(), Value::String(now_ms.to_string()));
    v.insert("metadatamodified".into(), Value::Bool(true));
    if v.contains_key("synced") {
        v.insert("synced".into(), Value::Bool(false));
    }
    Ok(pretty(&Value::Object(v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::parse::{Content, FileType, ItemType, Metadata};

    fn json_of(op: &Op) -> &[u8] {
        match op {
            Op::PutJson { json, .. } => json,
            Op::PutStaged { .. } | Op::PutStagedPage { .. } => panic!("not json"),
        }
    }

    #[test]
    fn folder_metadata_parses_back() {
        let u = Uuid::from_bytes([1; 16]);
        let parent = Uuid::from_bytes([2; 16]);
        let ops = new_folder(u, &ParentRef::Folder(parent), "A/B", 1234);
        let m = Metadata::from_slice(json_of(&ops[1])).unwrap();
        assert_eq!((m.item_type, m.visible_name.as_str(), m.parent, m.last_modified), (ItemType::Folder, "A/B", ParentRef::Folder(parent), Some(1234)));
        assert!(matches!(&ops[0], Op::PutJson { file: FileKind::Content, .. }));
    }

    #[test]
    fn document_ops_in_safe_order() {
        let u = Uuid::from_bytes([1; 16]);
        let ops = new_document(u, &ParentRef::Root, "Paper", DocKind::Epub, 77, 10, 5);
        // Data first, metadata last: the item only appears once everything is there.
        assert_eq!(ops[0], Op::PutStaged { uuid: u, file: FileKind::Epub, token: 77 });
        let c = Content::from_slice(json_of(&ops[1])).unwrap();
        assert_eq!(c.file_type, Some(FileType::Epub));
        let m = Metadata::from_slice(json_of(&ops[2])).unwrap();
        assert_eq!((m.item_type, m.parent), (ItemType::Document, ParentRef::Root));
    }

    #[test]
    fn edit_keeps_other_fields() {
        let raw = br#"{"createdTime":"1","lastOpenedPage":80,"parent":"","synced":true,"type":"DocumentType","visibleName":"old","custom":{"x":1}}"#;
        let out = edit_metadata(raw, Some(&ParentRef::Trash), Some("new"), 99).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["parent"], "trash");
        assert_eq!(v["visibleName"], "new");
        assert_eq!(v["lastModified"], "99");
        assert_eq!(v["synced"], false);
        assert_eq!(v["metadatamodified"], true);
        assert_eq!(v["lastOpenedPage"], 80);
        assert_eq!(v["custom"]["x"], 1);
        let only_name = edit_metadata(raw, None, Some("n"), 1).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&only_name).unwrap()["parent"], "");
    }

    #[test]
    fn kinds_and_sniffing() {
        assert_eq!(DocKind::from_name("a.PDF"), Some(DocKind::Pdf));
        assert_eq!(DocKind::from_name("a.epub"), Some(DocKind::Epub));
        assert_eq!(DocKind::from_name("a.txt"), None);
        assert!(DocKind::Pdf.sniff(b"%PDF-1.7\n"));
        assert!(!DocKind::Pdf.sniff(b"hello"));
        let mut epub = b"PK\x03\x04".to_vec();
        epub.resize(30, 0);
        epub.extend_from_slice(b"mimetypeapplication/epub+zip");
        assert!(DocKind::Epub.sniff(&epub));
        assert!(!DocKind::Epub.sniff(b"PK\x03\x04 a plain zip"));
    }
}
