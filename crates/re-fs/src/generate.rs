//! Files re-fs makes on demand: annotated PDFs, rendered notebooks, .rmdoc
//! archives and highlights. Made on first open, kept in the cache under a
//! fingerprint of everything they're made from, so they're remade only when the
//! document changes on the tablet.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use re_proto::index::parse::Content;
use re_proto::index::tree::{Node, NodeKind, Variant};
use re_proto::index::{Entry, FileKind};
use re_proto::wire::MAX_READ;
use re_render::pdf::{self, PageSpec};
use uuid::Uuid;

use crate::cache::{Cache, Key, ReadError};
use crate::client::Client;
use crate::session::Shared;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GenKey {
    pub uuid: Uuid,
    pub variant: Variant,
    pub version: u64,
}

impl GenKey {
    pub fn of(n: &Node) -> Option<GenKey> {
        n.variant.generated().then(|| Some(GenKey { uuid: n.uuid?, variant: n.variant, version: n.version })).flatten()
    }

    fn file_name(&self) -> String {
        let v = match self.variant {
            Variant::Annotated => "annotated.pdf",
            Variant::Rendered => "rendered.pdf",
            Variant::Rmdoc => "rmdoc",
            Variant::Highlights => "highlights.md",
            Variant::Original | Variant::Dir => "x",
        };
        format!("{}.{v}@{:016x}", self.uuid, self.version)
    }
}

/// One generation at a time per file; others wait for it.
#[derive(Default)]
pub struct Generator {
    locks: Mutex<HashMap<GenKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl Cache {
    fn gen_path(&self, key: &GenKey) -> PathBuf {
        self.dir().join("generated").join(key.file_name())
    }

    /// The made file, if it's in the cache.
    pub fn generated(&self, key: &GenKey) -> Option<File> {
        File::open(self.gen_path(key)).ok()
    }

    pub fn generated_size(&self, key: &GenKey) -> Option<u64> {
        fs::metadata(self.gen_path(key)).ok().map(|m| m.len())
    }
}

/// Make the file for `node` unless it's cached; returns its size.
pub async fn ensure(shared: &Arc<Shared>, node: &Node) -> Result<u64, ReadError> {
    let key = GenKey::of(node).ok_or_else(|| ReadError::Failed("not a generated file".into()))?;
    if let Some(n) = shared.cache.generated_size(&key) {
        return Ok(n);
    }
    let lock = shared.generator.locks.lock().unwrap().entry(key.clone()).or_default().clone();
    let _one = lock.lock().await;
    if let Some(n) = shared.cache.generated_size(&key) {
        return Ok(n);
    }
    let client = shared.client.load_full().ok_or(ReadError::Offline)?;
    let entry = shared.entries.load().get(&key.uuid).cloned().ok_or_else(|| ReadError::Failed("document is gone".into()))?;
    let t0 = std::time::Instant::now();
    let bytes = match key.variant {
        Variant::Rmdoc => rmdoc(&shared.cache, &client, &entry).await?,
        Variant::Highlights => highlights(&shared.cache, &client, &entry, &node.name).await?,
        Variant::Rendered => render_notebook(&shared.cache, &client, &entry).await?,
        Variant::Annotated => annotated(shared, &client, &entry, node).await?,
        _ => unreachable!(),
    };
    let path = shared.cache.gen_path(&key);
    fs::create_dir_all(path.parent().unwrap())?;
    // Drop older versions of the same file.
    let prefix = key.file_name().split('@').next().unwrap().to_owned() + "@";
    if let Ok(rd) = fs::read_dir(path.parent().unwrap()) {
        for d in rd.flatten() {
            if d.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(d.path());
            }
        }
    }
    let tmp = path.with_extension("tmp");
    File::create(&tmp)?.write_all(&bytes)?;
    fs::rename(&tmp, &path)?;
    shared.log(format_args!("made {:?} ({} bytes) in {:?}", node.name, bytes.len(), t0.elapsed()));
    shared.generator.locks.lock().unwrap().remove(&key);
    Ok(bytes.len() as u64)
}

async fn read_all(client: &Client, uuid: Uuid, file: FileKind, size: u64) -> Result<Vec<u8>, ReadError> {
    let mut out = Vec::with_capacity(size as usize);
    while (out.len() as u64) < size {
        let chunk = client.read(uuid, file, out.len() as u64, MAX_READ, None).await?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// One ReadPages request asks for at most this much (and this many files).
const BATCH_BYTES: u64 = 8 << 20;
const BATCH_FILES: usize = 256;

impl Cache {
    fn page_path(&self, uuid: &Uuid, name: &str, mtime_ms: i64) -> PathBuf {
        self.dir().join("pages").join(uuid.to_string()).join(format!("{name}@{mtime_ms}"))
    }

    fn cached_page(&self, uuid: &Uuid, name: &str, mtime_ms: i64) -> Option<Vec<u8>> {
        fs::read(self.page_path(uuid, name, mtime_ms)).ok()
    }

    /// Keep a page file, dropping older versions of it.
    fn store_page(&self, uuid: &Uuid, name: &str, mtime_ms: i64, data: &[u8]) {
        let path = self.page_path(uuid, name, mtime_ms);
        let Some(dir) = path.parent() else { return };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Ok(rd) = fs::read_dir(dir) {
            let prefix = format!("{name}@");
            for d in rd.flatten() {
                if d.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = fs::remove_file(d.path());
                }
            }
        }
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

/// The page-folder files `names` of `e`: from the local page cache when their
/// mtime matches, the rest from the tablet in batched ReadPages requests sent
/// concurrently. Files the index doesn't list are skipped.
async fn fetch_pages(cache: &Cache, client: &Client, e: &Entry, names: &[String]) -> Result<HashMap<String, Vec<u8>>, ReadError> {
    let mut have = HashMap::new();
    let mut batches: Vec<Vec<String>> = vec![Vec::new()];
    let mut batch_bytes = 0u64;
    for name in names {
        let Some(pf) = e.page(name) else { continue };
        if let Some(data) = cache.cached_page(&e.uuid, name, pf.mtime_ms) {
            have.insert(name.clone(), data);
            continue;
        }
        let cur = batches.last_mut().unwrap();
        if !cur.is_empty() && (batch_bytes + pf.size > BATCH_BYTES || cur.len() >= BATCH_FILES) {
            batches.push(Vec::new());
            batch_bytes = 0;
        }
        batches.last_mut().unwrap().push(name.clone());
        batch_bytes += pf.size;
    }
    let mut tasks = tokio::task::JoinSet::new();
    for batch in batches.into_iter().filter(|b| !b.is_empty()) {
        let (c, uuid) = (client.clone(), e.uuid);
        tasks.spawn(async move { c.read_pages(uuid, batch).await });
    }
    while let Some(r) = tasks.join_next().await {
        for p in r.map_err(|err| ReadError::Failed(err.to_string()))?? {
            cache.store_page(&e.uuid, &p.name, p.mtime_ms, &p.data);
            have.insert(p.name, p.data);
        }
    }
    Ok(have)
}

/// Each page in the tablet's order with its original page and parsed strokes.
async fn pages(cache: &Cache, client: &Client, e: &Entry) -> Result<Vec<PageSpec>, ReadError> {
    let content = e.content.as_deref().map(Content::from_slice).transpose().map_err(|err| ReadError::Failed(format!("bad .content: {err}")))?;
    let list = content.map(|c| c.page_list()).unwrap_or_default();
    let names: Vec<String> = list.iter().map(|p| format!("{}.rm", p.id)).filter(|n| e.page(n).is_some_and(|f| f.size > 0)).collect();
    let mut raw = fetch_pages(cache, client, e, &names).await?;
    let mut specs = Vec::with_capacity(list.len());
    for p in list {
        let name = format!("{}.rm", p.id);
        let strokes = match raw.remove(&name) {
            Some(raw) => {
                match re_render::parse(&raw) {
                    Ok(page) => Some(page),
                    Err(err) => {
                        eprintln!("re-fs: {}/{name}: {err}; page left blank", e.uuid);
                        None
                    }
                }
            }
            _ => None,
        };
        specs.push(PageSpec { source: p.redir, strokes });
    }
    Ok(specs)
}

/// Run CPU-heavy rendering off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, re_render::Error> + Send + 'static) -> Result<T, ReadError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ReadError::Failed(e.to_string()))?
        .map_err(|e| ReadError::Failed(e.to_string()))
}

async fn render_notebook(cache: &Cache, client: &Client, e: &Entry) -> Result<Vec<u8>, ReadError> {
    let specs = pages(cache, client, e).await?;
    let pages: Vec<_> = specs.into_iter().map(|s| s.strokes).collect();
    blocking(move || pdf::notebook(if pages.is_empty() { &[None] } else { &pages })).await
}

async fn annotated(shared: &Arc<Shared>, client: &Client, e: &Entry, node: &Node) -> Result<Vec<u8>, ReadError> {
    if node.kind != NodeKind::Pdf {
        return Err(ReadError::Failed("only PDFs can be annotated".into()));
    }
    let specs = pages(&shared.cache, client, e).await?;
    let st = e.stat(FileKind::Pdf).ok_or_else(|| ReadError::Failed("no PDF".into()))?;
    // The original goes through the normal cache, so it's reused for X.pdf too.
    let key = Key { uuid: e.uuid, kind: FileKind::Pdf, mtime_ms: st.mtime_ms, size: st.size };
    let original = shared.cache.read(shared, &key, 0, u32::try_from(st.size).unwrap_or(u32::MAX)).await?;
    blocking(move || pdf::annotate(&original, &specs)).await
}

async fn highlights(cache: &Cache, client: &Client, e: &Entry, file_name: &str) -> Result<Vec<u8>, ReadError> {
    let specs = pages(cache, client, e).await?;
    let title = file_name.strip_suffix(".highlights.md").unwrap_or(file_name);
    let parsed: Vec<(usize, &re_render::Page)> = specs.iter().enumerate().filter_map(|(i, s)| s.strokes.as_ref().map(|p| (i + 1, p))).collect();
    Ok(re_render::highlights::markdown(title, &parsed).into_bytes())
}

/// The document's files as reMarkable's .rmdoc: a zip of `<uuid>.metadata`,
/// `.content`, `.pagedata`, the original, and the page folder.
async fn rmdoc(cache: &Cache, client: &Client, e: &Entry) -> Result<Vec<u8>, ReadError> {
    let mut files: Vec<(String, Vec<u8>)> = vec![(format!("{}.metadata", e.uuid), e.metadata.clone())];
    if let Some(c) = &e.content {
        files.push((format!("{}.content", e.uuid), c.clone()));
    }
    for kind in [FileKind::Pagedata, FileKind::Pdf, FileKind::Epub] {
        if let Some(st) = e.stat(kind) {
            files.push((format!("{}{}", e.uuid, kind.suffix()), read_all(client, e.uuid, kind, st.size).await?));
        }
    }
    let names: Vec<String> = e.pages.iter().map(|p| p.name.clone()).collect();
    let mut got = fetch_pages(cache, client, e, &names).await?;
    for name in names {
        if let Some(data) = got.remove(&name) {
            files.push((format!("{}/{name}", e.uuid), data));
        }
    }
    tokio::task::spawn_blocking(move || -> io::Result<Vec<u8>> {
        let mut zw = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            zw.start_file(name, opts).map_err(io::Error::other)?;
            zw.write_all(&data)?;
        }
        Ok(zw.finish().map_err(io::Error::other)?.into_inner())
    })
    .await
    .map_err(|e| ReadError::Failed(e.to_string()))?
    .map_err(ReadError::Io)
}
