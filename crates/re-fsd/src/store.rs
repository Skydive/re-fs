//! The agent's in-memory copy of the index: raw `.metadata`/`.content` bytes and file
//! stats per UUID, with a generation counter for deltas.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use re_proto::index::{Entry, FileKind, FileStat};
use re_proto::wire::{compress_entries, Response};
use uuid::Uuid;

pub struct Store {
    dir: PathBuf,
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
        let mut store = Store {
            dir: dir.to_owned(),
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
        Ok(Some(Entry { uuid: *uuid, metadata, content, files }))
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

    /// Open a file for a Read; returns it with its size and mtime.
    pub fn open_file(&self, uuid: &Uuid, kind: FileKind) -> io::Result<(File, u64, i64)> {
        let f = File::open(self.path(uuid, kind))?;
        let m = f.metadata()?;
        Ok((f, m.len(), mtime_ms(&m)))
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
}
