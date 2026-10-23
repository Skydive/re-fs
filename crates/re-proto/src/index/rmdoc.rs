//! Importing a whole document from a `.rmdoc` (reMarkable's archive of one
//! document's files), as copied in directly or embedded in an annotated PDF.
//! Pure: validates the archive's file list and turns it into a `Plan`'s ops under
//! a fresh UUID; the client unpacks and uploads, the agent only moves bytes.

use uuid::Uuid;

use super::entry::{is_page_file, FileKind};
use super::naming;
use super::parse::ParentRef;
use super::plan::edit_metadata;
use crate::wire::Op;

/// What one file in the archive is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Metadata,
    Content,
    Pagedata,
    /// The imported PDF or EPUB.
    Original(FileKind),
    /// A file in the page folder (`<page-id>.rm`, `<page-id>-metadata.json`).
    Page(String),
}

/// The archive's files, classified: (index into the caller's list, part).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The document's UUID inside the archive (replaced on import).
    pub uuid: Uuid,
    pub parts: Vec<(usize, Part)>,
}

/// Validate a `.rmdoc`'s file names. Every file must belong to one document;
/// tablet-made caches (thumbnails, `.local`, text conversion, old highlights,
/// `.epubindex`) are skipped; anything else is refused.
pub fn layout(names: &[&str]) -> Result<Layout, String> {
    let mut uuid: Option<Uuid> = None;
    let mut parts = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if name.ends_with('/') {
            continue; // directory entries
        }
        let (u, rest) = name.split_at_checked(36).ok_or_else(|| format!("unexpected file {name:?}"))?;
        let u = Uuid::parse_str(u).map_err(|_| format!("unexpected file {name:?}"))?;
        if *uuid.get_or_insert(u) != u {
            return Err(format!("{name:?} belongs to another document"));
        }
        let part = match rest {
            ".metadata" => Part::Metadata,
            ".content" => Part::Content,
            ".pagedata" => Part::Pagedata,
            ".pdf" => Part::Original(FileKind::Pdf),
            ".epub" => Part::Original(FileKind::Epub),
            ".local" | ".epubindex" => continue,
            r if [".thumbnails/", ".textconversion/", ".highlights/"].iter().any(|p| r.starts_with(p)) => continue,
            r => match r.strip_prefix('/') {
                Some(page) if is_page_file(page) => Part::Page(page.to_owned()),
                _ => return Err(format!("unexpected file {name:?}")),
            },
        };
        parts.push((i, part));
    }
    let uuid = uuid.ok_or("the archive is empty")?;
    match parts.iter().filter(|(_, p)| *p == Part::Metadata).count() {
        1 => {}
        0 => return Err("the archive has no .metadata".into()),
        _ => return Err("the archive has several .metadata files".into()),
    }
    if parts.iter().filter(|(_, p)| matches!(p, Part::Original(_))).count() > 1 {
        return Err("the archive has more than one original file".into());
    }
    Ok(Layout { uuid, parts })
}

/// The visible name for a document imported from file `file_name`: the
/// extension (".rmdoc", ".pdf"), and the ".annotated" re-fs adds, removed.
pub fn import_name(file_name: &str) -> String {
    let mut base = file_name;
    for ext in [".rmdoc", ".pdf", ".annotated"] {
        base = naming::strip_ext(base, ext).unwrap_or(base);
    }
    naming::visible_name(if base.is_empty() { file_name } else { base })
}

/// The data to write, uploaded already where it's a file (`token`s from PutChunk).
pub struct Import<'a> {
    pub metadata: &'a [u8],
    pub content: Option<&'a [u8]>,
    pub pagedata: Option<&'a [u8]>,
    pub original: Option<(FileKind, u64)>,
    /// (page-folder file name, staged token).
    pub pages: Vec<(String, u64)>,
}

/// Ops writing the document under `new` in `parent` as `name`. `.content` and the
/// page files are kept byte for byte (page ids tie them together); `.metadata`
/// gets the new parent and name, and is marked as changed for sync.
pub fn import_rmdoc(new: Uuid, parent: &ParentRef, name: &str, d: &Import, now_ms: i64) -> serde_json::Result<Vec<Op>> {
    let mut ops = Vec::new();
    if let Some((file, token)) = d.original {
        ops.push(Op::PutStaged { uuid: new, file, token });
    }
    for (page, token) in &d.pages {
        ops.push(Op::PutStagedPage { uuid: new, name: page.clone(), token: *token });
    }
    if let Some(c) = d.content {
        ops.push(Op::PutJson { uuid: new, file: FileKind::Content, json: c.to_vec() });
    }
    if let Some(p) = d.pagedata {
        ops.push(Op::PutJson { uuid: new, file: FileKind::Pagedata, json: p.to_vec() });
    }
    let edited = edit_metadata(d.metadata, Some(parent), Some(name), now_ms)?;
    let mut meta: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&edited)?;
    if meta.contains_key("deleted") {
        meta.insert("deleted".into(), false.into());
    }
    ops.push(Op::PutJson { uuid: new, file: FileKind::Metadata, json: serde_json::to_vec_pretty(&meta)? });
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::parse::Metadata;

    const U: &str = "33333333-3333-4333-8333-333333333333";
    const P: &str = "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f";

    fn names(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn classifies_a_document() {
        let files = vec![
            format!("{U}.metadata"),
            format!("{U}.content"),
            format!("{U}.pdf"),
            format!("{U}.pagedata"),
            format!("{U}/"),
            format!("{U}/{P}.rm"),
            format!("{U}/{P}-metadata.json"),
            format!("{U}.thumbnails/{P}.png"),
            format!("{U}.local"),
        ];
        let l = layout(&names(&files)).unwrap();
        assert_eq!(l.uuid.to_string(), U);
        let parts: Vec<Part> = l.parts.into_iter().map(|(_, p)| p).collect();
        assert_eq!(
            parts,
            [
                Part::Metadata,
                Part::Content,
                Part::Original(FileKind::Pdf),
                Part::Pagedata,
                Part::Page(format!("{P}.rm")),
                Part::Page(format!("{P}-metadata.json")),
            ]
        );
    }

    #[test]
    fn refuses_bad_archives() {
        let other = "44444444-4444-4444-8444-444444444444";
        for bad in [
            vec![format!("{U}.metadata"), format!("{U}/../../etc/passwd")],
            vec![format!("{U}.metadata"), format!("{U}.exe")],
            vec![format!("{U}.metadata"), format!("{other}.content")],
            vec![format!("{U}.content")],
            vec![format!("{U}.metadata"), format!("{U}.pdf"), format!("{U}.epub")],
            vec!["notes.txt".to_string()],
            vec![],
        ] {
            assert!(layout(&names(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn import_names() {
        assert_eq!(import_name("Paper.annotated.pdf"), "Paper");
        assert_eq!(import_name("Notes.pdf"), "Notes");
        assert_eq!(import_name("Book.rmdoc"), "Book");
        assert_eq!(import_name("01∕02∕24.annotated.pdf"), "01/02/24");
        assert_eq!(import_name(".pdf"), ".pdf");
    }

    #[test]
    fn import_ops_under_a_new_uuid() {
        let new = Uuid::from_bytes([9; 16]);
        let folder = Uuid::from_bytes([2; 16]);
        let meta = br#"{"type":"DocumentType","visibleName":"old","parent":"trash","deleted":true,"lastModified":"1"}"#;
        let d = Import {
            metadata: meta,
            content: Some(b"{\"fileType\":\"pdf\"}"),
            pagedata: Some(b"Blank\n"),
            original: Some((FileKind::Pdf, 7)),
            pages: vec![(format!("{P}.rm"), 8)],
        };
        let ops = import_rmdoc(new, &ParentRef::Folder(folder), "Paper", &d, 99).unwrap();
        assert_eq!(ops[0], Op::PutStaged { uuid: new, file: FileKind::Pdf, token: 7 });
        assert_eq!(ops[1], Op::PutStagedPage { uuid: new, name: format!("{P}.rm"), token: 8 });
        assert_eq!(ops[2], Op::PutJson { uuid: new, file: FileKind::Content, json: b"{\"fileType\":\"pdf\"}".to_vec() });
        assert_eq!(ops[3], Op::PutJson { uuid: new, file: FileKind::Pagedata, json: b"Blank\n".to_vec() });
        let Op::PutJson { file: FileKind::Metadata, json, .. } = &ops[4] else { panic!("no metadata op") };
        let m = Metadata::from_slice(json).unwrap();
        assert_eq!((m.visible_name.as_str(), m.parent, m.deleted, m.last_modified), ("Paper", ParentRef::Folder(folder), false, Some(99)));
    }
}
