//! Files copied onto the tablet (`cp` into a `--rw` mount, or `re-fs push`):
//! plain PDFs and EPUBs, and whole documents with their strokes, from a `.rmdoc`
//! or from a PDF re-fs made (it carries its document's `.rmdoc` as an attachment).

use std::io::{Cursor, Read};
use std::time::{SystemTime, UNIX_EPOCH};

use re_proto::index::parse::{Content, Metadata, ParentRef};
use re_proto::index::plan::{self, DocKind};
use re_proto::index::rmdoc::{self, Import, Part};
use re_proto::index::{naming, FileKind};
use re_proto::wire::{Expect, Plan};
use re_render::attach::RMDOC_NAME;
use uuid::Uuid;

use crate::client::{CallError, Client};

/// Upload chunk size.
const CHUNK: usize = 4 << 20;
/// Refuse archives that unpack to more than this (zip bombs).
const MAX_UNPACKED: u64 = 4 << 30;

/// What a copied-in file can be, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incoming {
    Pdf,
    Epub,
    Rmdoc,
}

impl Incoming {
    pub fn from_name(name: &str) -> Option<Self> {
        match DocKind::from_name(name) {
            Some(DocKind::Pdf) => Some(Incoming::Pdf),
            Some(DocKind::Epub) => Some(Incoming::Epub),
            None => naming::strip_ext(name, ".rmdoc").map(|_| Incoming::Rmdoc),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Not a file the tablet can take, or an archive that doesn't check out (EINVAL).
    Invalid,
    /// The tablet went away (EHOSTDOWN).
    Down,
    /// Anything else (EIO).
    Io,
}

#[derive(Debug)]
pub struct ImportError {
    pub kind: Failure,
    pub message: String,
}

impl ImportError {
    fn invalid(message: impl Into<String>) -> Self {
        ImportError { kind: Failure::Invalid, message: message.into() }
    }
}

impl From<CallError> for ImportError {
    fn from(e: CallError) -> Self {
        let kind = match e {
            CallError::Closed => Failure::Down,
            _ => Failure::Io,
        };
        ImportError { kind, message: e.to_string() }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// An import ready to apply: its files are uploaded, the plan writes them.
pub struct Prepared {
    pub uuid: Uuid,
    pub plan: Plan,
    /// For logs: "PDF", "document from .rmdoc", …
    pub what: &'static str,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Upload one file to the agent's staging area; returns its token.
async fn upload(c: &Client, data: Vec<u8>) -> Result<u64, CallError> {
    let token = crate::rand_u64();
    if data.is_empty() {
        c.put_chunk(token, 0, Vec::new()).await?;
    }
    for (n, chunk) in data.chunks(CHUNK).enumerate() {
        c.put_chunk(token, (n * CHUNK) as u64, chunk.to_vec()).await?;
    }
    Ok(token)
}

/// Check `data` (named `name`), upload what it needs, and plan writing it into
/// `parent`. Whole documents keep their strokes, pages and page order.
pub async fn prepare(c: &Client, parent: &ParentRef, name: &str, data: Vec<u8>) -> Result<Prepared, ImportError> {
    let kind = Incoming::from_name(name).ok_or_else(|| ImportError::invalid(format!("{name}: only .pdf, .epub and .rmdoc can go on the tablet")))?;
    match kind {
        Incoming::Rmdoc => {
            if !data.starts_with(b"PK\x03\x04") {
                return Err(ImportError::invalid(format!("{name} isn't a .rmdoc (zip) file")));
            }
            prepare_rmdoc(c, parent, &rmdoc::import_name(name), &data, "document from .rmdoc").await
        }
        Incoming::Pdf | Incoming::Epub => {
            let doc = if kind == Incoming::Pdf { DocKind::Pdf } else { DocKind::Epub };
            if !doc.sniff(&data[..data.len().min(64)]) {
                return Err(ImportError::invalid(format!("{name} isn't a valid {} file", doc.ext())));
            }
            if kind == Incoming::Pdf {
                if let Some(zip) = re_render::attach::extract(&data, RMDOC_NAME) {
                    return prepare_rmdoc(c, parent, &rmdoc::import_name(name), &zip, "document from its embedded .rmdoc").await;
                }
            }
            let uuid = Uuid::new_v4();
            let size = data.len() as u64;
            let token = upload(c, data).await?;
            let base = naming::strip_ext(name, doc.ext()).unwrap_or(name);
            let ops = plan::new_document(uuid, parent, &naming::visible_name(base), doc, token, size, now_ms());
            let what = if doc == DocKind::Pdf { "PDF" } else { "EPUB" };
            Ok(Prepared { uuid, plan: Plan { expect: vec![Expect { uuid, metadata: None }], ops, restart: true }, what })
        }
    }
}

/// The files of a zip archive, as (name, bytes).
fn unzip(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).map_err(|e| format!("not a readable zip: {e}"))?;
    let mut total = 0u64;
    let mut out = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        total += f.size();
        if total > MAX_UNPACKED {
            return Err("the archive is too large".into());
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf).map_err(|e| format!("{}: {e}", f.name()))?;
        out.push((f.name().to_owned(), buf));
    }
    Ok(out)
}

async fn prepare_rmdoc(c: &Client, parent: &ParentRef, visible: &str, zip: &[u8], what: &'static str) -> Result<Prepared, ImportError> {
    let files = unzip(zip).map_err(ImportError::invalid)?;
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    let layout = rmdoc::layout(&names).map_err(ImportError::invalid)?;
    let mut metadata = None;
    let mut content = None;
    let mut pagedata = None;
    let mut original = None;
    let mut pages = Vec::new();
    for (i, part) in &layout.parts {
        let data = &files[*i].1;
        match part {
            Part::Metadata => metadata = Some(data.as_slice()),
            Part::Content => content = Some(data.as_slice()),
            Part::Pagedata => pagedata = Some(data.as_slice()),
            Part::Original(k) => original = Some((*k, data.clone())),
            Part::Page(n) => pages.push((n.clone(), data.clone())),
        }
    }
    let metadata = metadata.expect("layout checked there's one .metadata");
    Metadata::from_slice(metadata).map_err(|e| ImportError::invalid(format!(".rmdoc has a bad .metadata: {e}")))?;
    if let Some(c) = content {
        Content::from_slice(c).map_err(|e| ImportError::invalid(format!(".rmdoc has a bad .content: {e}")))?;
    }
    // Upload the original and the page files, all at once (the client pipelines).
    let mut tasks = tokio::task::JoinSet::new();
    if let Some((kind, data)) = original {
        let c = c.clone();
        tasks.spawn(async move { (None, Some(kind), upload(&c, data).await) });
    }
    for (name, data) in pages {
        let c = c.clone();
        tasks.spawn(async move { (Some(name), None, upload(&c, data).await) });
    }
    let mut staged_original: Option<(FileKind, u64)> = None;
    let mut staged_pages = Vec::new();
    while let Some(r) = tasks.join_next().await {
        let (page, kind, token) = r.map_err(|e| ImportError { kind: Failure::Io, message: e.to_string() })?;
        let token = token?;
        match (page, kind) {
            (Some(n), _) => staged_pages.push((n, token)),
            (None, Some(k)) => staged_original = Some((k, token)),
            (None, None) => unreachable!(),
        }
    }
    staged_pages.sort();
    let uuid = Uuid::new_v4();
    let d = Import { metadata, content, pagedata, original: staged_original, pages: staged_pages };
    let ops = rmdoc::import_rmdoc(uuid, parent, visible, &d, now_ms()).map_err(|e| ImportError::invalid(e.to_string()))?;
    Ok(Prepared { uuid, plan: Plan { expect: vec![Expect { uuid, metadata: None }], ops, restart: true }, what })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_by_name() {
        assert_eq!(Incoming::from_name("a.PDF"), Some(Incoming::Pdf));
        assert_eq!(Incoming::from_name("a.epub"), Some(Incoming::Epub));
        assert_eq!(Incoming::from_name("a.rmdoc"), Some(Incoming::Rmdoc));
        assert_eq!(Incoming::from_name("a.zip"), None);
    }

    #[test]
    fn unzips_stored_and_deflated() {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zw.start_file("a", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        zw.write_all(b"stored").unwrap();
        zw.start_file("b", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
        zw.write_all(&b"deflated ".repeat(100)).unwrap();
        let z = zw.finish().unwrap().into_inner();
        let files = unzip(&z).unwrap();
        assert_eq!(files[0], ("a".to_string(), b"stored".to_vec()));
        assert_eq!(files[1].1, b"deflated ".repeat(100));
        assert!(unzip(b"PK\x03\x04garbage").is_err());
    }
}
