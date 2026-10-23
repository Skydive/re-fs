//! Local copies of tablet files, keyed by UUID, file kind and the tablet-side mtime.
//!
//! ```text
//! ~/.cache/re-fs/objects/<uuid>.pdf@<mtime>    complete files, read straight from disk
//! ~/.cache/re-fs/partial/<uuid>.pdf@<mtime>    downloads in progress
//! ~/.cache/re-fs/index.zstd                    last index, for mounting offline
//! ```
//!
//! The first open of a file starts one background download of the whole file;
//! reads wait only until their own range has arrived. A read far beyond the
//! download front (seeking deep into a big PDF) is fetched directly instead.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use re_proto::index::tree::Node;
use re_proto::index::{Entry, FileKind};
use re_proto::wire::MAX_READ;
use tokio::sync::watch;
use uuid::Uuid;

use crate::client::{CallError, Client};
use crate::session::Shared;

/// Bytes per Read request during a download, and how many are in flight.
const CHUNK: u32 = 4 << 20;
const DEPTH: usize = 4;
/// A read starting this far past the download front is fetched on its own.
const SKIP_AHEAD: u64 = 16 << 20;
/// How long a read waits for its range before giving up.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub uuid: Uuid,
    pub kind: FileKind,
    pub mtime_ms: i64,
    pub size: u64,
}

impl Key {
    pub fn of(n: &Node) -> Option<Key> {
        Some(Key { uuid: n.uuid?, kind: n.kind.data_file()?, mtime_ms: n.data_mtime_ms, size: n.size })
    }

    fn file_name(&self) -> String {
        format!("{}{}@{}", self.uuid, self.kind.suffix(), self.mtime_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Bytes [0, n) are on disk.
    Have(u64),
    Done,
    Failed(String),
}

struct Fetch {
    file: File,
    state: watch::Receiver<State>,
}

pub struct Cache {
    dir: PathBuf,
    cap: u64,
    fetches: Mutex<HashMap<Key, Arc<Fetch>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("tablet offline and file not cached")]
    Offline,
    #[error("download failed: {0}")]
    Failed(String),
    #[error("timed out waiting for data")]
    Timeout,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Call(#[from] CallError),
}

impl ReadError {
    pub fn errno(&self) -> i32 {
        match self {
            ReadError::Offline => libc::EHOSTDOWN,
            ReadError::Timeout => libc::ETIMEDOUT,
            ReadError::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            ReadError::Call(CallError::Closed) => libc::EHOSTDOWN,
            ReadError::Failed(_) | ReadError::Call(_) => libc::EIO,
        }
    }
}

impl Cache {
    pub fn open(dir: PathBuf, cap: u64) -> io::Result<Arc<Self>> {
        fs::create_dir_all(dir.join("objects"))?;
        // Leftovers from a previous run can't be resumed safely; start them over.
        let _ = fs::remove_dir_all(dir.join("partial"));
        fs::create_dir_all(dir.join("partial"))?;
        Ok(Arc::new(Cache { dir, cap, fetches: Mutex::new(HashMap::new()) }))
    }

    pub fn default_dir() -> PathBuf {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("re-fs")
    }

    fn object(&self, key: &Key) -> PathBuf {
        self.dir.join("objects").join(key.file_name())
    }

    /// The complete local copy, if there is one. Marks it recently used.
    pub fn complete(&self, key: &Key) -> Option<File> {
        let f = File::open(self.object(key)).ok()?;
        let _ = f.set_modified(SystemTime::now());
        Some(f)
    }

    /// Start downloading `key` unless it's cached or already on its way.
    pub fn prefetch(self: &Arc<Self>, shared: &Shared, key: &Key) {
        if key.size == 0 || self.object(key).exists() {
            return;
        }
        if let Some(client) = shared.client.load_full() {
            let _ = self.fetch(&client, key);
        }
    }

    fn fetch(self: &Arc<Self>, client: &Client, key: &Key) -> io::Result<Arc<Fetch>> {
        let mut fetches = self.fetches.lock().unwrap();
        if let Some(f) = fetches.get(key) {
            if !matches!(*f.state.borrow(), State::Failed(_)) {
                return Ok(f.clone());
            }
        }
        let path = self.dir.join("partial").join(key.file_name());
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
        let (tx, rx) = watch::channel(State::Have(0));
        let fetch = Arc::new(Fetch { file: file.try_clone()?, state: rx });
        fetches.insert(key.clone(), fetch.clone());
        let (cache, client, key) = (self.clone(), client.clone(), key.clone());
        tokio::spawn(async move {
            let result = download(&client, &key, &file, &tx).await;
            let state = match result.and_then(|()| cache.finish(&key, &path)) {
                Ok(()) => State::Done,
                Err(e) => {
                    let _ = fs::remove_file(&path);
                    State::Failed(e.to_string())
                }
            };
            let _ = tx.send(state);
            cache.fetches.lock().unwrap().remove(&key);
        });
        Ok(fetch)
    }

    /// Move a finished download into place, drop older versions, trim to the cap.
    fn finish(&self, key: &Key, partial: &Path) -> io::Result<()> {
        fs::rename(partial, self.object(key))?;
        let prefix = format!("{}{}@", key.uuid, key.kind.suffix());
        let mut objects = Vec::new();
        for dent in fs::read_dir(self.dir.join("objects"))? {
            let dent = dent?;
            let name = dent.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name != key.file_name() {
                let _ = fs::remove_file(dent.path());
                continue;
            }
            let m = dent.metadata()?;
            objects.push((m.modified()?, m.len(), dent.path()));
        }
        let mut total: u64 = objects.iter().map(|o| o.1).sum();
        objects.sort();
        for (_, len, path) in objects {
            if total <= self.cap {
                break;
            }
            if path != self.object(key) {
                let _ = fs::remove_file(path);
                total -= len;
            }
        }
        Ok(())
    }

    /// Bytes `[offset, offset+size)` of `key`, from cache, the download, or the tablet.
    pub async fn read(self: &Arc<Self>, shared: &Shared, key: &Key, offset: u64, size: u32) -> Result<Vec<u8>, ReadError> {
        let end = (offset + u64::from(size)).min(key.size);
        if offset >= end {
            return Ok(Vec::new());
        }
        if let Some(f) = self.complete(key) {
            return Ok(read_at(&f, offset, end)?);
        }
        let Some(client) = shared.client.load_full() else { return Err(ReadError::Offline) };
        let fetch = self.fetch(&client, key)?;
        let mut state = fetch.state.clone();
        let offline = || !shared.online.load(std::sync::atomic::Ordering::Relaxed);
        let have = match &*state.borrow() {
            State::Have(n) => *n,
            State::Done => key.size,
            State::Failed(_) if offline() => return Err(ReadError::Offline),
            State::Failed(e) => return Err(ReadError::Failed(e.clone())),
        };
        if offset > have + SKIP_AHEAD {
            return direct(&client, key, offset, end).await;
        }
        let ready = tokio::time::timeout(READ_TIMEOUT, state.wait_for(|s| match s {
            State::Have(n) => *n >= end,
            State::Done | State::Failed(_) => true,
        }))
        .await
        .map_err(|_| ReadError::Timeout)?
        .map_err(|_| ReadError::Failed("download stopped".into()))?
        .clone();
        match ready {
            // The link dropped mid-download (tablet asleep, out of range).
            State::Failed(_) if offline() => Err(ReadError::Offline),
            State::Failed(e) => Err(ReadError::Failed(e)),
            // A finished download has been renamed; the open fd still reads it.
            _ => Ok(read_at(&fetch.file, offset, end)?),
        }
    }

    /// zstd over postcard `Vec<Entry>`, written atomically.
    pub fn save_index(&self, entries: &[&Entry]) {
        let Ok(raw) = postcard::to_stdvec(entries) else { return };
        let Ok(packed) = zstd::encode_all(&raw[..], 9) else { return };
        let tmp = self.dir.join("index.zstd.tmp");
        if fs::write(&tmp, packed).is_ok() {
            let _ = fs::rename(&tmp, self.dir.join("index.zstd"));
        }
    }

    pub fn load_index(&self) -> Option<Vec<Entry>> {
        let raw = zstd::decode_all(&fs::read(self.dir.join("index.zstd")).ok()?[..]).ok()?;
        postcard::from_bytes(&raw).ok()
    }
}

fn read_at(f: &File, offset: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; (end - offset) as usize];
    f.read_exact_at(&mut buf, offset)?;
    Ok(buf)
}

async fn direct(client: &Client, key: &Key, offset: u64, end: u64) -> Result<Vec<u8>, ReadError> {
    let mut out = Vec::with_capacity((end - offset) as usize);
    while (out.len() as u64) < end - offset {
        let off = offset + out.len() as u64;
        let len = (end - off).min(u64::from(MAX_READ)) as u32;
        let chunk = client.read(key.uuid, key.kind, off, len, Some(key.mtime_ms)).await?;
        if chunk.is_empty() {
            return Err(ReadError::Failed("file is shorter than the index says".into()));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Fetch the whole file in order, DEPTH requests in flight, advancing `tx` as the
/// contiguous prefix on disk grows.
async fn download(client: &Client, key: &Key, file: &File, tx: &watch::Sender<State>) -> io::Result<()> {
    let mut queue = std::collections::VecDeque::new();
    let mut next = 0u64;
    let mut have = 0u64;
    let issue = |queue: &mut std::collections::VecDeque<_>, next: &mut u64| {
        while queue.len() < DEPTH && *next < key.size {
            let len = (key.size - *next).min(u64::from(CHUNK)) as u32;
            let (c, k, off) = (client.clone(), key.clone(), *next);
            queue.push_back((off, len, tokio::spawn(async move { c.read(k.uuid, k.kind, off, len, Some(k.mtime_ms)).await })));
            *next += u64::from(len);
        }
    };
    issue(&mut queue, &mut next);
    while let Some((off, len, task)) = queue.pop_front() {
        let chunk = task.await.map_err(io::Error::other)?.map_err(io::Error::other)?;
        if chunk.len() != len as usize {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "file is shorter than the index says"));
        }
        file.write_all_at(&chunk, off)?;
        have = off + chunk.len() as u64;
        tx.send_replace(State::Have(have));
        issue(&mut queue, &mut next);
    }
    if have != key.size {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("got {have} of {} bytes", key.size)));
    }
    file.sync_data()?;
    Ok(())
}
