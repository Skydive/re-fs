//! Typed views of `<uuid>.metadata` and `<uuid>.content`.

use serde::{Deserialize, Deserializer};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ItemType {
    #[serde(rename = "CollectionType")]
    Folder,
    #[serde(rename = "DocumentType")]
    Document,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ParentRef {
    #[default]
    Root,
    Trash,
    Folder(Uuid),
    /// Not a UUID and not one of the special values.
    Invalid(String),
}

impl ParentRef {
    pub fn parse(s: &str) -> Self {
        match s {
            "" => Self::Root,
            "trash" => Self::Trash,
            _ => Uuid::parse_str(s).map_or_else(|_| Self::Invalid(s.to_owned()), Self::Folder),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub visible_name: String,
    #[serde(rename = "type")]
    pub item_type: ItemType,
    #[serde(default, deserialize_with = "parent")]
    pub parent: ParentRef,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, deserialize_with = "millis")]
    pub last_modified: Option<i64>,
    #[serde(default, deserialize_with = "millis")]
    pub created_time: Option<i64>,
    #[serde(default, deserialize_with = "millis")]
    pub last_opened: Option<i64>,
    /// 0-based; -1 or absent when never opened.
    #[serde(default, deserialize_with = "millis")]
    pub last_opened_page: Option<i64>,
}

impl Metadata {
    pub fn from_slice(json: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(json)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Pdf,
    Epub,
    Notebook,
    Other,
}

impl<'de> Deserialize<'de> for FileType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "pdf" => Self::Pdf,
            "epub" => Self::Epub,
            // Older notebooks have an empty fileType.
            "notebook" | "" => Self::Notebook,
            _ => Self::Other,
        })
    }
}

/// One page in display order. `redir` is the original PDF/EPUB page it shows;
/// `None` means a blank page inserted on the tablet (or any notebook page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub id: String,
    pub redir: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    pub file_type: Option<FileType>,
    pub page_count: Option<u32>,
    /// Format v1: page ids in order.
    pub pages: Option<Vec<String>>,
    /// Format v1: original page per position (-1 = inserted).
    pub redirection_page_map: Option<Vec<i64>>,
    /// Format v2.
    #[serde(rename = "cPages")]
    pub c_pages: Option<CPages>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CPages {
    #[serde(default)]
    pub pages: Vec<CPage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CPage {
    pub id: String,
    pub idx: Option<Stamped<String>>,
    pub redir: Option<Stamped<i64>>,
    pub deleted: Option<Stamped<serde_json::Value>>,
}

/// v2 wraps most values as `{"timestamp": "1:2", "value": ...}`.
#[derive(Debug, Clone, Deserialize)]
pub struct Stamped<T> {
    pub value: T,
}

/// What the tree needs from a `.content`: file type, page count, tags. Skips the
/// page lists, which are most of the bytes, without allocating for them.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentKind {
    pub file_type: Option<FileType>,
    #[serde(default, deserialize_with = "millis")]
    pub page_count: Option<i64>,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

/// A document tag. Older files store plain strings, newer ones objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag(pub String);

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            S(String),
            O { name: String },
        }
        Ok(Tag(match Raw::deserialize(d)? {
            Raw::S(s) | Raw::O { name: s } => s,
        }))
    }
}

impl ContentKind {
    pub fn from_slice(json: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(json)
    }
}

impl Content {
    pub fn from_slice(json: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(json)
    }

    /// Pages in display order, for either content format.
    pub fn page_list(&self) -> Vec<Page> {
        if let Some(c) = &self.c_pages {
            let mut live: Vec<&CPage> = c
                .pages
                .iter()
                .filter(|p| !p.deleted.as_ref().is_some_and(|d| is_truthy(&d.value)))
                .collect();
            // idx values are strings that sort in page order ("ba" < "bb"); ties by id
            // so the order is deterministic.
            live.sort_by(|a, b| {
                let ia = a.idx.as_ref().map(|s| s.value.as_str()).unwrap_or("");
                let ib = b.idx.as_ref().map(|s| s.value.as_str()).unwrap_or("");
                ia.cmp(ib).then_with(|| a.id.cmp(&b.id))
            });
            return live
                .into_iter()
                .map(|p| Page {
                    id: p.id.clone(),
                    redir: p.redir.as_ref().and_then(|r| u32::try_from(r.value).ok()),
                })
                .collect();
        }
        let ids = self.pages.as_deref().unwrap_or_default();
        let map = self.redirection_page_map.as_deref();
        ids.iter()
            .enumerate()
            .map(|(i, id)| Page {
                id: id.clone(),
                redir: match map {
                    Some(m) => m.get(i).and_then(|&r| u32::try_from(r).ok()),
                    // No map: a PDF's pages are its own pages in order.
                    None if self.file_type != Some(FileType::Notebook) => u32::try_from(i).ok(),
                    None => None,
                },
            })
            .collect()
    }
}

fn is_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_i64() != Some(0),
        serde_json::Value::Null => false,
        _ => true,
    }
}

fn parent<'de, D: Deserializer<'de>>(d: D) -> Result<ParentRef, D::Error> {
    Ok(ParentRef::parse(&Option::<String>::deserialize(d)?.unwrap_or_default()))
}

/// Timestamps are milliseconds, stored as a string ("1701093565557") or sometimes a number.
fn millis<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        S(String),
        N(i64),
        F(f64),
    }
    Ok(match Option::<Raw>::deserialize(d)? {
        Some(Raw::S(s)) => s.trim().parse().ok(),
        Some(Raw::N(n)) => Some(n),
        Some(Raw::F(f)) => Some(f as i64),
        None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_folder_and_document() {
        let m = Metadata::from_slice(
            br#"{"createdTime":"1701093565557","lastModified":"1701100163735","parent":"22222222-2222-4222-8222-222222222222","pinned":false,"type":"DocumentType","visibleName":"01/02/24"}"#,
        )
        .unwrap();
        assert_eq!(m.item_type, ItemType::Document);
        assert_eq!(m.visible_name, "01/02/24");
        assert_eq!(m.last_modified, Some(1701100163735));
        assert_eq!(m.created_time, Some(1701093565557));
        assert!(matches!(m.parent, ParentRef::Folder(_)));
        assert!(!m.deleted);

        let f = Metadata::from_slice(br#"{"type":"CollectionType","visibleName":"Books","parent":"","lastModified":1}"#).unwrap();
        assert_eq!(f.item_type, ItemType::Folder);
        assert_eq!(f.parent, ParentRef::Root);
        assert_eq!(f.created_time, None);
    }

    #[test]
    fn parent_values() {
        assert_eq!(ParentRef::parse(""), ParentRef::Root);
        assert_eq!(ParentRef::parse("trash"), ParentRef::Trash);
        assert_eq!(ParentRef::parse("nope"), ParentRef::Invalid("nope".into()));
        let m = Metadata::from_slice(br#"{"type":"DocumentType","visibleName":"x"}"#).unwrap();
        assert_eq!(m.parent, ParentRef::Root);
    }

    #[test]
    fn content_v1_pages() {
        let c = Content::from_slice(
            br#"{"fileType":"pdf","pageCount":3,"pages":["a","b","c"],"redirectionPageMap":[0,-1,1]}"#,
        )
        .unwrap();
        assert_eq!(c.file_type, Some(FileType::Pdf));
        let p = c.page_list();
        assert_eq!(p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(p.iter().map(|p| p.redir).collect::<Vec<_>>(), [Some(0), None, Some(1)]);
    }

    #[test]
    fn content_v2_orders_by_idx_and_skips_deleted() {
        let c = Content::from_slice(
            br#"{"fileType":"pdf","formatVersion":2,"cPages":{"pages":[
                {"id":"second","idx":{"timestamp":"1:2","value":"bb"},"redir":{"timestamp":"1:1","value":1}},
                {"id":"gone","idx":{"timestamp":"1:2","value":"bc"},"deleted":{"timestamp":"1:3","value":1}},
                {"id":"first","idx":{"timestamp":"1:2","value":"ba"},"redir":{"timestamp":"1:1","value":0}},
                {"id":"inserted","idx":{"timestamp":"1:2","value":"bab"}}
            ]}}"#,
        )
        .unwrap();
        let p = c.page_list();
        assert_eq!(p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["first", "inserted", "second"]);
        assert_eq!(p.iter().map(|p| p.redir).collect::<Vec<_>>(), [Some(0), None, Some(1)]);
    }

    #[test]
    fn content_kind_tags_and_counts() {
        let c = ContentKind::from_slice(br#"{"fileType":"pdf","pageCount":12,"tags":[{"name":"Work","timestamp":1},"old"],"pages":["a"]}"#).unwrap();
        assert_eq!(c.page_count, Some(12));
        assert_eq!(c.tags, vec![Tag("Work".into()), Tag("old".into())]);
        let m = Metadata::from_slice(br#"{"type":"DocumentType","visibleName":"x","lastOpened":"1700","lastOpenedPage":4}"#).unwrap();
        assert_eq!((m.last_opened, m.last_opened_page), (Some(1700), Some(4)));
    }

    #[test]
    fn empty_file_type_is_notebook() {
        let c = Content::from_slice(br#"{"fileType":""}"#).unwrap();
        assert_eq!(c.file_type, Some(FileType::Notebook));
        let c = Content::from_slice(br#"{}"#).unwrap();
        assert_eq!(c.file_type, None);
    }
}
