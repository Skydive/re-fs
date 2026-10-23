//! The agent's in-memory copy of the index: raw `.metadata`/`.content` bytes and file
//! stats per UUID, with a generation counter for deltas.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use re_proto::index::{is_page_file, Entry, FileKind, FileStat, PageFile};
use re_proto::wire::{compress_entries, Op, Plan, Response};
use uuid::Uuid;

pub struct Store {
    dir: PathBuf,
    /// Uploads waiting for an Apply; beside xochitl/ so moving them in is a rename.
    staging: PathBuf,
    instance: u64,
    generation: u64,
    items: HashMap<Uuid, (Entry, u64)>,
    /// UUIDs removed, with the generation they disappeared in.
    tombstones: HashMap<Uuid, u64>,
}

pub fn mtime_ms(m: &Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// "<uuid><suffix>" → the UUID, for any file belonging to an item.
pub fn uuid_of(name: &str) -> Option<Uuid> {
    name.get(..36).and_then(|u| Uuid::parse_str(u).ok())
}

impl Store {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let staging = dir.parent().unwrap_or(dir).join(".re-fs-staging");
        // Uploads from an earlier run were never applied.
        let _ = fs::remove_dir_all(&staging);
        let mut store = Store {
            dir: dir.to_owned(),
            staging,
            instance: (now.as_nanos() as u64) ^ (u64::from(std::process::id()) << 32),
            generation: 1,
            items: HashMap::new(),
            tombstones: HashMap::new(),
        };
        store.rescan_all()?;
        Ok(store)
    }

    pub fn instance(&self) -> u64 {
        self.instance
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn path(&self, uuid: &Uuid, kind: FileKind) -> PathBuf {
        self.dir.join(format!("{uuid}{}", kind.suffix()))
    }

    /// Re-read everything: at start, and after an inotify queue overflow.
    pub fn rescan_all(&mut self) -> io::Result<bool> {
        let mut uuids: HashSet<Uuid> = self.items.keys().copied().collect();
        for dent in fs::read_dir(&self.dir)? {
            if let Some(u) = dent?.file_name().to_str().and_then(uuid_of) {
                uuids.insert(u);
            }
        }
        self.refresh(uuids)
    }

    /// Re-read these items. Returns whether anything changed (and bumps the
    /// generation once if so).
    pub fn refresh(&mut self, uuids: impl IntoIterator<Item = Uuid>) -> io::Result<bool> {
        let next = self.generation + 1;
        let mut changed = false;
        for uuid in uuids {
            match self.load(&uuid)? {
                Some(entry) => {
                    if self.items.get(&uuid).is_none_or(|(old, _)| *old != entry) {
                        self.items.insert(uuid, (entry, next));
                        self.tombstones.remove(&uuid);
                        changed = true;
                    }
                }
                None => {
                    if self.items.remove(&uuid).is_some() {
                        self.tombstones.insert(uuid, next);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.generation = next;
        }
        Ok(changed)
    }

    /// One item from disk; `None` if it has no `.metadata` (gone, or not yet written).
    fn load(&self, uuid: &Uuid) -> io::Result<Option<Entry>> {
        let metadata = match fs::read(self.path(uuid, FileKind::Metadata)) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let content = match fs::read(self.path(uuid, FileKind::Content)) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let mut files = Vec::new();
        for kind in FileKind::ALL {
            match fs::metadata(self.path(uuid, kind)) {
                Ok(m) if m.is_file() => files.push(FileStat { kind, size: m.len(), mtime_ms: mtime_ms(&m) }),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let mut pages = Vec::new();
        match fs::read_dir(self.dir.join(uuid.to_string())) {
            Ok(rd) => {
                for d in rd.flatten() {
                    let name = d.file_name().to_string_lossy().into_owned();
                    if !is_page_file(&name) {
                        continue;
                    }
                    if let Ok(m) = d.metadata() {
                        if m.is_file() {
                            pages.push(PageFile { name, size: m.len(), mtime_ms: mtime_ms(&m) });
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        pages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(Entry { uuid: *uuid, metadata, content, files, pages }))
    }

    /// The page folder of an item, for watching.
    pub fn page_dir(&self, uuid: &Uuid) -> PathBuf {
        self.dir.join(uuid.to_string())
    }

    /// Items that have a page folder.
    pub fn uuids(&self) -> Vec<Uuid> {
        self.items.keys().copied().collect()
    }

    /// Open a page-folder file for a ReadPage.
    pub fn open_page(&self, uuid: &Uuid, name: &str) -> io::Result<(File, u64, i64)> {
        if !is_page_file(name) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a page file"));
        }
        let f = File::open(self.page_dir(uuid).join(name))?;
        let m = f.metadata()?;
        Ok((f, m.len(), mtime_ms(&m)))
    }

    /// The whole index, or the delta since `since` if it's from this instance.
    pub fn index(&self, since: Option<(u64, u64)>) -> Response {
        let since = since.and_then(|(inst, g)| (inst == self.instance && g <= self.generation).then_some(g));
        let (full, entries, removed) = match since {
            None => (true, self.items.values().map(|(e, _)| e).collect::<Vec<_>>(), Vec::new()),
            Some(g) => (
                false,
                self.items.values().filter(|(_, eg)| *eg > g).map(|(e, _)| e).collect(),
                self.tombstones.iter().filter(|(_, tg)| **tg > g).map(|(u, _)| *u).collect(),
            ),
        };
        Response::Index {
            instance: self.instance,
            generation: self.generation,
            full,
            removed,
            entries: compress_entries(&entries),
        }
    }

    pub fn put_chunk(&self, token: u64, offset: u64, data: &[u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        fs::create_dir_all(&self.staging)?;
        let f = fs::OpenOptions::new().write(true).create(true).truncate(false).open(self.staging.join(token.to_string()))?;
        f.write_all_at(data, offset)
    }

    /// Check `plan.expect`, then write its ops: data files first, `.content`, and
    /// `.metadata` last, each via a temporary file and rename, so an item appears
    /// only once it's complete. Returns the new generation.
    pub fn apply(&mut self, plan: &Plan) -> Result<u64, ApplyError> {
        for e in &plan.expect {
            let actual = match fs::metadata(self.path(&e.uuid, FileKind::Metadata)) {
                Ok(m) => Some((mtime_ms(&m), m.len())),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(err.into()),
            };
            if actual != e.metadata {
                return Err(ApplyError::Conflict(e.uuid));
            }
        }
        let rank = |op: &Op| match op {
            Op::PutStaged { .. } => 0,
            Op::PutJson { file: FileKind::Content, .. } => 1,
            Op::PutJson { .. } => 2,
        };
        let mut ops: Vec<&Op> = plan.ops.iter().collect();
        ops.sort_by_key(|op| rank(op));
        for op in &ops {
            match op {
                Op::PutJson { file: FileKind::Metadata | FileKind::Content, .. } => {}
                Op::PutStaged { file: FileKind::Pdf | FileKind::Epub, token, .. } => {
                    if !self.staging.join(token.to_string()).is_file() {
                        return Err(ApplyError::Bad(format!("no staged upload {token}")));
                    }
                }
                _ => return Err(ApplyError::Bad("op writes a file kind it may not".into())),
            }
        }
        let mut touched = Vec::new();
        for op in ops {
            match op {
                Op::PutJson { uuid, file, json } => {
                    let dest = self.path(uuid, *file);
                    let tmp = dest.with_extension(format!("{}.re-fs-tmp", dest.extension().and_then(|e| e.to_str()).unwrap_or("")));
                    {
                        use std::io::Write;
                        let mut f = File::create(&tmp)?;
                        f.write_all(json)?;
                        f.sync_all()?;
                    }
                    fs::rename(&tmp, &dest)?;
                    touched.push(*uuid);
                }
                Op::PutStaged { uuid, file, token } => {
                    let src = self.staging.join(token.to_string());
                    let dest = self.path(uuid, *file);
                    File::open(&src)?.sync_all()?;
                    if fs::rename(&src, &dest).is_err() {
                        // Staging on another filesystem (only in odd test setups).
                        fs::copy(&src, &dest)?;
                        fs::remove_file(&src)?;
                    }
                    touched.push(*uuid);
                }
            }
        }
        self.refresh(touched)?;
        Ok(self.generation)
    }

    /// Open a file for a Read; returns it with its size and mtime.
    pub fn open_file(&self, uuid: &Uuid, kind: FileKind) -> io::Result<(File, u64, i64)> {
        let f = File::open(self.path(uuid, kind))?;
        let m = f.metadata()?;
        Ok((f, m.len(), mtime_ms(&m)))
    }
}

#[derive(Debug)]
pub enum ApplyError {
    Conflict(Uuid),
    Bad(String),
    Io(io::Error),
}

impl From<io::Error> for ApplyError {
    fn from(e: io::Error) -> Self {
        ApplyError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use re_proto::wire::decompress_entries;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("re-fsd-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn index_parts(r: Response) -> (bool, Vec<Entry>, Vec<Uuid>, u64) {
        match r {
            Response::Index { full, entries, removed, generation, .. } => (full, decompress_entries(&entries).unwrap(), removed, generation),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn scan_refresh_and_deltas() {
        let d = tmpdir("store");
        let (a, b) = (Uuid::from_bytes([1; 16]), Uuid::from_bytes([2; 16]));
        fs::write(d.join(format!("{a}.metadata")), b"{}").unwrap();
        fs::write(d.join(format!("{a}.pdf")), b"%PDF-1.4").unwrap();
        fs::create_dir(d.join(format!("{a}"))).unwrap(); // page folder: ignored
        fs::write(d.join("junk.txt"), b"x").unwrap();

        let mut s = Store::open(&d).unwrap();
        assert_eq!(s.len(), 1);
        let (full, e, _, g1) = index_parts(s.index(None));
        assert!(full);
        assert_eq!(e[0].stat(FileKind::Pdf).unwrap().size, 8);

        // Unchanged refresh doesn't bump the generation.
        assert!(!s.refresh([a]).unwrap());
        assert_eq!(s.generation(), g1);

        fs::write(d.join(format!("{b}.metadata")), b"{}").unwrap();
        assert!(s.refresh([b]).unwrap());
        let (full, e, removed, g2) = index_parts(s.index(Some((s.instance(), g1))));
        assert!(!full);
        assert_eq!((e.len(), e[0].uuid, removed.len(), g2), (1, b, 0, g1 + 1));

        fs::remove_file(d.join(format!("{a}.metadata"))).unwrap();
        assert!(s.refresh([a]).unwrap());
        let (_, e, removed, _) = index_parts(s.index(Some((s.instance(), g2))));
        assert_eq!((e.len(), removed), (0, vec![a]));

        // Another instance's generation gets the full index.
        let (full, e, _, _) = index_parts(s.index(Some((s.instance() ^ 1, g2))));
        assert!(full);
        assert_eq!(e.len(), 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn apply_checks_preconditions_and_writes_in_order() {
        use re_proto::wire::Expect;
        let d = tmpdir("apply").join("xochitl");
        fs::create_dir_all(&d).unwrap();
        let mut s = Store::open(&d).unwrap();
        let u = Uuid::from_bytes([3; 16]);
        s.put_chunk(9, 0, b"%PDF-").unwrap();
        s.put_chunk(9, 5, b"1.7").unwrap();
        let plan = Plan {
            expect: vec![Expect { uuid: u, metadata: None }],
            ops: vec![
                Op::PutJson { uuid: u, file: FileKind::Metadata, json: b"{}".to_vec() },
                Op::PutStaged { uuid: u, file: FileKind::Pdf, token: 9 },
            ],
            restart: false,
        };
        let g = s.apply(&plan).unwrap();
        assert_eq!(fs::read(d.join(format!("{u}.pdf"))).unwrap(), b"%PDF-1.7");
        assert_eq!(s.generation(), g);
        assert_eq!(s.len(), 1);
        // Same plan again: the item now exists, so "must not exist" fails.
        assert!(matches!(s.apply(&plan), Err(ApplyError::Conflict(x)) if x == u));
        // A stale expectation also fails, and writes nothing.
        let m = fs::metadata(d.join(format!("{u}.metadata"))).unwrap();
        let stale = Plan {
            expect: vec![Expect { uuid: u, metadata: Some((mtime_ms(&m), m.len() + 1)) }],
            ops: vec![Op::PutJson { uuid: u, file: FileKind::Metadata, json: b"{\"x\":1}".to_vec() }],
            restart: false,
        };
        assert!(matches!(s.apply(&stale), Err(ApplyError::Conflict(_))));
        assert_eq!(fs::read(d.join(format!("{u}.metadata"))).unwrap(), b"{}");
        // Only metadata/content may be written as JSON.
        let bad = Plan { expect: vec![], ops: vec![Op::PutJson { uuid: u, file: FileKind::Pdf, json: vec![] }], restart: false };
        assert!(matches!(s.apply(&bad), Err(ApplyError::Bad(_))));
        fs::remove_dir_all(d.parent().unwrap()).unwrap();
    }
}
