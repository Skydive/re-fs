//! Reading a local copy of the tablet's xochitl directory (`mirror/`), for `re-fs ls`
//! and tests. The agent does the same scan on the tablet.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use re_proto::index::{Entry, FileKind, FileStat};
use uuid::Uuid;

pub fn scan(dir: &Path) -> Result<Vec<Entry>> {
    let mut by_uuid: BTreeMap<Uuid, Entry> = BTreeMap::new();
    for dent in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let dent = dent?;
        let name = dent.file_name();
        let Some(name) = name.to_str() else { continue };
        // "<36-char uuid><suffix>"; anything else (page folders, thumbnails) is skipped.
        let Some((u, suffix)) = name.split_at_checked(36) else { continue };
        let (Ok(uuid), Some(kind)) = (Uuid::parse_str(u), FileKind::from_suffix(suffix)) else { continue };
        let meta = dent.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let entry = by_uuid.entry(uuid).or_insert_with(|| Entry { uuid, metadata: Vec::new(), content: None, files: Vec::new(), pages: Vec::new() });
        match kind {
            FileKind::Metadata => entry.metadata = fs::read(dent.path())?,
            FileKind::Content => entry.content = Some(fs::read(dent.path())?),
            FileKind::Pdf | FileKind::Epub | FileKind::Pagedata => {}
        }
        let mtime_ms = meta.modified()?.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        entry.files.push(FileStat { kind, size: meta.len(), mtime_ms });
    }
    // An item without .metadata isn't an item (e.g. a half-written import).
    Ok(by_uuid.into_values().filter(|e| !e.metadata.is_empty()).collect())
}
