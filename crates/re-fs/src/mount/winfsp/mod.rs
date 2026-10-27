//! The WinFsp backend (Windows): the tablet as a drive (`R:`) or a folder in
//! Explorer. As with FUSE, lookups and listings answer from the in-memory tree,
//! reads go through the cache, and writes become plans (write.rs). WinFsp calls in on
//! its own thread pool, so callbacks may block on tokio.
//!
//! Names are the tree's Windows names (`TreeOptions::windows_names`), looked up
//! ignoring case. Copying a PDF/EPUB in collects it in a staging file and sends it
//! on Cleanup (the copying program's CloseHandle). Deleting moves to the tablet's trash.

mod ffi;

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use re_proto::index::naming;
use re_proto::index::parse::ParentRef;
use re_proto::index::tree::{Invalidation, Node, NodeKind, Tree, View};

use crate::cache::Key;
use crate::errno::{self, Errno};
use crate::generate::{self, GenKey};
use crate::session::Shared;
use crate::write;
use ffi::*;

/// How long Windows may cache file info. Changes from the tablet are pushed with
/// FspFileSystemNotify, so this only bounds staleness if one is missed.
const FILE_INFO_TIMEOUT_MS: u32 = 5000;
const VOLUME_LABEL: &str = "reMarkable";
/// Full control for SYSTEM, Administrators and Everyone; what may actually change
/// is decided by the callbacks (and the read-only volume flag with --ro).
const SDDL: &str = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)";
/// Folders Windows makes on any fixed drive (Explorer's Recycle Bin, System Restore and
/// the indexer). Refused, so they don't turn up on the tablet; Explorer then deletes
/// for good (to the tablet's trash) instead of recycling.
const WINDOWS_FOLDERS: [&str; 2] = ["$RECYCLE.BIN", "System Volume Information"];
/// File IDs for files being copied in, until the tablet has them.
const FIRST_UPLOAD_ID: u64 = 1 << 62;

/// A file being copied in: collected locally, sent to the tablet on Cleanup.
struct Upload {
    /// Its path, case-folded: how later opens find it.
    key: String,
    parent: ParentRef,
    /// As typed (a Windows name).
    name: String,
    path: PathBuf,
    file: Arc<File>,
    id: u64,
    created_ms: i64,
    /// With --overwrite: the document this copy replaces, trashed once the copy is in.
    replaces: Option<Node>,
    state: Mutex<UploadState>,
}

struct UploadState {
    size: u64,
    /// Sent (or failed, or cancelled): no more writes.
    done: bool,
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone)]
enum Open {
    /// The node as it was opened; its inode finds it in later trees.
    Node(Node),
    Upload(Arc<Upload>),
}

/// The FileContext WinFsp keeps for each open.
struct Handle {
    open: Mutex<Open>,
    /// This handle created the upload (Create, or Overwrite), so its Cleanup sends it.
    creator: AtomicBool,
}

impl Handle {
    /// A new handle as the raw FileContext WinFsp keeps (freed in Close).
    fn into_context(open: Open, creator: bool) -> *mut c_void {
        Box::into_raw(Box::new(Handle { open: Mutex::new(open), creator: AtomicBool::new(creator) })).cast()
    }

    fn get(&self) -> Open {
        self.open.lock().unwrap().clone()
    }
}

struct Fs {
    shared: Arc<Shared>,
    rt: tokio::runtime::Handle,
    /// Self-relative security descriptor every file and folder gets.
    sd: Vec<u8>,
    uploads: Mutex<HashMap<String, Arc<Upload>>>,
    next_upload: AtomicU64,
}

enum Found {
    Node(Node),
    Upload(Arc<Upload>),
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Unix ms → FILETIME (100 ns since 1601).
fn filetime(ms: i64) -> u64 {
    (ms.max(0) as u64 + 11_644_473_600_000) * 10_000
}

/// An errno from write.rs or the cache as an NTSTATUS.
fn status(e: Errno) -> NTSTATUS {
    match e {
        errno::EROFS => STATUS_MEDIA_WRITE_PROTECTED,
        errno::EHOSTDOWN => STATUS_HOST_UNREACHABLE,
        errno::ENOENT => STATUS_OBJECT_NAME_NOT_FOUND,
        errno::EEXIST => STATUS_OBJECT_NAME_COLLISION,
        errno::EPERM | errno::EACCES | errno::EBUSY => STATUS_ACCESS_DENIED,
        errno::ENOTDIR => STATUS_NOT_A_DIRECTORY,
        errno::EISDIR => STATUS_FILE_IS_A_DIRECTORY,
        errno::ENOTEMPTY => STATUS_DIRECTORY_NOT_EMPTY,
        errno::EINVAL => STATUS_INVALID_PARAMETER,
        errno::ETIMEDOUT => STATUS_IO_TIMEOUT,
        errno::EAGAIN => STATUS_RETRY,
        errno::EBADF => STATUS_INVALID_HANDLE,
        _ => STATUS_UNEXPECTED_IO_ERROR,
    }
}

fn io_status(e: &std::io::Error) -> NTSTATUS {
    match (e.raw_os_error(), api()) {
        (Some(code), Ok(api)) => unsafe { (api.nt_status_from_win32)(code as u32) },
        _ => STATUS_UNEXPECTED_IO_ERROR,
    }
}

/// "\a\b\c" → ("\a\b", "c").
fn split(path: &str) -> (&str, &str) {
    match path.rfind('\\') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// A tree path ("Books/x.pdf") as WinFsp names it ("\Books\x.pdf").
fn win_path(tree_path: &str) -> String {
    format!("\\{}", tree_path.replace('/', "\\"))
}

/// "\Books\x.pdf" → its node, ignoring case.
fn resolve(tree: &Tree, path: &str) -> Result<Node, NTSTATUS> {
    let parts: Vec<&str> = path.split('\\').filter(|c| !c.is_empty()).collect();
    let mut cur = tree.root();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        cur = match tree.lookup_nocase(cur.ino, part) {
            Some(n) if last || n.kind.is_dir() => n,
            _ if last => return Err(STATUS_OBJECT_NAME_NOT_FOUND),
            _ => return Err(STATUS_OBJECT_PATH_NOT_FOUND),
        };
    }
    Ok(cur.clone())
}

impl Fs {
    fn tree(&self) -> Arc<Tree> {
        self.shared.tree.load_full()
    }

    fn online(&self) -> bool {
        self.shared.online.load(Ordering::Relaxed)
    }

    fn find(&self, path: &str) -> Result<Found, NTSTATUS> {
        if let Some(u) = self.uploads.lock().unwrap().get(&naming::fold_case(path)) {
            return Ok(Found::Upload(u.clone()));
        }
        resolve(&self.tree(), path).map(Found::Node)
    }

    fn node_info(&self, n: &Node) -> FileInfo {
        let dir = n.kind.is_dir();
        let size = match GenKey::of(n) {
            _ if dir => 0,
            // Generated files: their size once made (on the first open for reading).
            Some(k) => self.shared.cache.generated_size(&k).unwrap_or(0),
            None => n.size,
        };
        let mut attrs = if dir { FILE_ATTRIBUTE_DIRECTORY } else { 0 };
        // Only the main tree's own documents can be renamed or deleted.
        let changeable = self.shared.writable && n.view == View::Main && (dir || n.is_primary());
        if !dir && !changeable {
            attrs |= FILE_ATTRIBUTE_READONLY;
        }
        if n.name.starts_with('.') {
            attrs |= FILE_ATTRIBUTE_HIDDEN; // .trash, .originals, … as on Linux
        }
        let mtime = filetime(n.mtime_ms);
        FileInfo {
            file_attributes: if attrs == 0 { FILE_ATTRIBUTE_NORMAL } else { attrs },
            allocation_size: size.div_ceil(4096) * 4096,
            file_size: size,
            creation_time: filetime(n.ctime_ms),
            last_access_time: mtime,
            last_write_time: mtime,
            change_time: mtime,
            index_number: n.ino,
            ..Default::default()
        }
    }

    fn upload_info(&self, u: &Upload) -> FileInfo {
        let size = u.state.lock().unwrap().size;
        let t = filetime(u.created_ms);
        FileInfo {
            file_attributes: FILE_ATTRIBUTE_ARCHIVE,
            allocation_size: size.div_ceil(4096) * 4096,
            file_size: size,
            creation_time: t,
            last_access_time: t,
            last_write_time: t,
            change_time: t,
            index_number: FIRST_UPLOAD_ID + u.id,
            ..Default::default()
        }
    }

    fn open_info(&self, open: &Open) -> FileInfo {
        match open {
            Open::Node(n) => self.node_info(self.tree().node(n.ino).unwrap_or(n)),
            Open::Upload(u) => self.upload_info(u),
        }
    }

    /// Copy the security descriptor out, WinFsp style: report the size, and fail
    /// with STATUS_BUFFER_OVERFLOW if the caller's buffer is too small.
    unsafe fn put_sd(&self, sd: *mut c_void, size: *mut usize) -> NTSTATUS {
        if size.is_null() {
            return STATUS_SUCCESS;
        }
        let need = self.sd.len();
        if *size < need {
            *size = need;
            return STATUS_BUFFER_OVERFLOW;
        }
        *size = need;
        if !sd.is_null() {
            std::ptr::copy_nonoverlapping(self.sd.as_ptr(), sd.cast(), need);
        }
        STATUS_SUCCESS
    }

    fn writable_dir(&self, dir: &Node) -> Result<(), NTSTATUS> {
        if !self.shared.writable {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        if !self.online() {
            return Err(STATUS_HOST_UNREACHABLE);
        }
        match (dir.kind, dir.view) {
            (NodeKind::Root | NodeKind::Folder | NodeKind::Trash, View::Main) => Ok(()),
            (NodeKind::LostFound | NodeKind::ViewDir | NodeKind::Folder, _) => Err(STATUS_ACCESS_DENIED),
            _ => Err(STATUS_NOT_A_DIRECTORY),
        }
    }

    /// Before the first read: make a generated file (so its size is known), or check
    /// that an original can be read. Called only for opens that read data, so
    /// Explorer listing a folder doesn't render or download everything in it.
    fn prepare_read(&self, n: &Node) -> Result<(), NTSTATUS> {
        if let Some(gk) = GenKey::of(n) {
            if self.shared.cache.generated(&gk).is_some() {
                return Ok(());
            }
            if !self.online() {
                return Err(STATUS_HOST_UNREACHABLE);
            }
            return match self.rt.block_on(generate::ensure(&self.shared, n)) {
                Ok(_) => Ok(()),
                Err(e) => {
                    self.shared.log(format_args!("making {:?}: {e}", n.name));
                    Err(status(e.errno()))
                }
            };
        }
        if let Some(key) = Key::of(n) {
            // Fail here rather than in Read, where an error looks like a broken file.
            if !self.online() && self.shared.cache.complete(&key).is_none() {
                return Err(STATUS_HOST_UNREACHABLE);
            }
        }
        Ok(())
    }

    /// A file being copied into `dir` as `name`: a new document, or (with --overwrite)
    /// one replacing `replaces`.
    fn start_upload(&self, dir: &Node, name: &str, path: &str, replaces: Option<Node>) -> Result<Arc<Upload>, NTSTATUS> {
        self.writable_dir(dir)?;
        // Only PDFs and EPUBs can live on the tablet (a .rmdoc becomes one with its strokes).
        if crate::import::Incoming::from_name(name).is_none() {
            eprintln!("re-fs: {name:?}: only .pdf, .epub and .rmdoc files can go on the tablet");
            return Err(STATUS_ACCESS_DENIED);
        }
        if let Some(old) = &replaces {
            // Off by default: replacing a document by accident loses its annotations.
            if !self.shared.overwrite || !old.is_primary() || old.kind.is_dir() || old.uuid.is_none() {
                return Err(STATUS_ACCESS_DENIED);
            }
        }
        let parent = match write::parent_ref(&self.tree(), dir.ino) {
            Ok(ParentRef::Trash) => return Err(STATUS_ACCESS_DENIED),
            Ok(p) => p,
            Err(e) => return Err(status(e)),
        };
        let id = self.next_upload.fetch_add(1, Ordering::Relaxed);
        let (staging, file) = self.shared.cache.staging_file(FIRST_UPLOAD_ID + id).map_err(|e| io_status(&e))?;
        let u = Arc::new(Upload {
            key: naming::fold_case(path),
            parent,
            name: name.to_owned(),
            path: staging,
            file: Arc::new(file),
            id,
            created_ms: now_ms(),
            replaces,
            state: Mutex::new(UploadState { size: 0, done: false }),
        });
        self.uploads.lock().unwrap().insert(u.key.clone(), u.clone());
        Ok(u)
    }

    /// Send a copied-in file to the tablet. Cleanup can't return an error, so a
    /// failure is reported on stderr (and the file doesn't appear).
    fn commit(&self, u: &Arc<Upload>) {
        let size = {
            let mut st = u.state.lock().unwrap();
            if st.done {
                return;
            }
            st.done = true;
            st.size
        };
        let (shared, parent, name, file, replaces) = (self.shared.clone(), u.parent.clone(), naming::from_windows_name(&u.name), u.file.clone(), u.replaces.clone());
        let r = self.rt.block_on(async move {
            let r = write::import(shared.clone(), parent, name, file, size).await;
            // The new copy is on the tablet: only now does the old one go to the trash.
            match (r, replaces) {
                (Ok(()), Some(old)) => {
                    shared.log(format_args!("overwrite: {:?} replaced, old copy to the trash", old.name));
                    write::trash(shared, old).await
                }
                (r, _) => r,
            }
        });
        // The tree has it now (or never will): later opens find the document.
        self.uploads.lock().unwrap().remove(&u.key);
        if let Err(e) = r {
            eprintln!("re-fs: copying {:?} to the tablet failed: {}", u.name, describe(e));
        }
    }

    fn cancel(&self, u: &Upload) {
        u.state.lock().unwrap().done = true;
        self.uploads.lock().unwrap().remove(&u.key);
    }
}

fn describe(e: Errno) -> &'static str {
    match e {
        errno::EHOSTDOWN => "the tablet isn't connected",
        errno::EINVAL => "not a valid PDF, EPUB or .rmdoc",
        errno::ETIMEDOUT => "the tablet didn't confirm it in time",
        errno::EAGAIN => "the item changed on the tablet meanwhile; try again",
        _ => "an I/O error",
    }
}

// ---- callbacks ----------------------------------------------------------------------

/// A panic must not unwind into WinFsp; it fails the one operation instead.
fn guarded(f: impl FnOnce() -> NTSTATUS) -> NTSTATUS {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(STATUS_UNEXPECTED_IO_ERROR)
}

unsafe fn fs<'a>(f: *mut FileSystem) -> &'a Fs {
    &*((*f.cast::<FileSystemHead>()).user_context as *const Fs)
}

unsafe fn handle<'a>(ctx: *mut c_void) -> &'a Handle {
    &*(ctx as *const Handle)
}

unsafe extern "system" fn get_volume_info(f: *mut FileSystem, vi: *mut VolumeInfo) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let used: u64 = fs.tree().nodes().map(|n| n.size).sum();
        // Some programs refuse to copy onto a volume reporting no free space.
        let free = if fs.shared.writable { 1 << 30 } else { 0 };
        let vi = &mut *vi;
        vi.total_size = used + free;
        vi.free_size = free;
        let label: Vec<u16> = VOLUME_LABEL.encode_utf16().collect();
        vi.volume_label[..label.len()].copy_from_slice(&label);
        vi.volume_label_length = (label.len() * 2) as u16;
        STATUS_SUCCESS
    })
}

unsafe extern "system" fn get_security_by_name(f: *mut FileSystem, name: PWSTR, attrs: *mut u32, sd: *mut c_void, sd_size: *mut usize) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let info = match fs.find(&from_pwstr(name)) {
            Ok(Found::Node(n)) => fs.node_info(&n),
            Ok(Found::Upload(u)) => fs.upload_info(&u),
            Err(s) => return s,
        };
        if !attrs.is_null() {
            *attrs = info.file_attributes;
        }
        fs.put_sd(sd, sd_size)
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn create(
    f: *mut FileSystem,
    name: PWSTR,
    options: u32,
    _granted: u32,
    _attrs: u32,
    _sd: *mut c_void,
    _alloc: u64,
    ctx: *mut *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let path = from_pwstr(name);
        let (dir_path, leaf) = split(&path);
        let tree = fs.tree();
        let dir = match resolve(&tree, dir_path) {
            Ok(d) if d.kind.is_dir() => d,
            _ => return STATUS_OBJECT_PATH_NOT_FOUND,
        };
        if tree.lookup_nocase(dir.ino, leaf).is_some() || fs.uploads.lock().unwrap().contains_key(&naming::fold_case(&path)) {
            return STATUS_OBJECT_NAME_COLLISION;
        }
        if options & FILE_DIRECTORY_FILE != 0 {
            if WINDOWS_FOLDERS.iter().any(|w| w.eq_ignore_ascii_case(leaf)) {
                return STATUS_ACCESS_DENIED;
            }
            if let Err(s) = fs.writable_dir(&dir) {
                return s;
            }
            if let Err(e) = fs.rt.block_on(write::mkdir(fs.shared.clone(), dir.ino, naming::from_windows_name(leaf))) {
                return status(e);
            }
            let Some(n) = fs.tree().lookup_nocase(dir.ino, leaf).cloned() else { return STATUS_UNEXPECTED_IO_ERROR };
            *info = fs.node_info(&n);
            *ctx = Handle::into_context(Open::Node(n), false);
            return STATUS_SUCCESS;
        }
        match fs.start_upload(&dir, leaf, &path, None) {
            Ok(u) => {
                *info = fs.upload_info(&u);
                *ctx = Handle::into_context(Open::Upload(u), true);
                STATUS_SUCCESS
            }
            Err(s) => s,
        }
    })
}

unsafe extern "system" fn open(f: *mut FileSystem, name: PWSTR, _options: u32, granted: u32, ctx: *mut *mut c_void, info: *mut FileInfo) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let n = match fs.find(&from_pwstr(name)) {
            Ok(Found::Node(n)) => n,
            Ok(Found::Upload(u)) => {
                *info = fs.upload_info(&u);
                *ctx = Handle::into_context(Open::Upload(u), false);
                return STATUS_SUCCESS;
            }
            Err(s) => return s,
        };
        if granted & FILE_READ_DATA != 0 && !n.kind.is_dir() {
            if let Err(s) = fs.prepare_read(&n) {
                return s;
            }
        }
        *info = fs.node_info(&n);
        *ctx = Handle::into_context(Open::Node(n), false);
        STATUS_SUCCESS
    })
}

/// CREATE_ALWAYS / TRUNCATE_EXISTING on an existing file: with --overwrite, a copy
/// replacing the document.
unsafe extern "system" fn overwrite(f: *mut FileSystem, ctx: *mut c_void, _attrs: u32, _replace: u8, _alloc: u64, info: *mut FileInfo) -> NTSTATUS {
    guarded(|| {
        let (fs, h) = (fs(f), handle(ctx));
        let n = match h.get() {
            Open::Upload(u) => {
                let mut st = u.state.lock().unwrap();
                if st.done {
                    return STATUS_ACCESS_DENIED;
                }
                if let Err(e) = u.file.set_len(0) {
                    return io_status(&e);
                }
                st.size = 0;
                drop(st);
                *info = fs.upload_info(&u);
                return STATUS_SUCCESS;
            }
            Open::Node(n) => n,
        };
        let tree = fs.tree();
        let Some(n) = tree.node(n.ino).cloned() else { return STATUS_OBJECT_NAME_NOT_FOUND };
        let dir = tree.parent(&n).clone();
        match fs.start_upload(&dir, &n.name.clone(), &win_path(&tree.path(&n)), Some(n)) {
            Ok(u) => {
                *info = fs.upload_info(&u);
                *h.open.lock().unwrap() = Open::Upload(u);
                // This handle now owns the upload, though it didn't create the file.
                h.creator.store(true, Ordering::Relaxed);
                STATUS_SUCCESS
            }
            Err(s) => s,
        }
    })
}

unsafe extern "system" fn cleanup(f: *mut FileSystem, ctx: *mut c_void, _name: PWSTR, flags: u32) {
    guarded(|| {
        let (fs, h) = (fs(f), handle(ctx));
        match (h.get(), flags & FSP_CLEANUP_DELETE != 0) {
            (Open::Node(n), true) => {
                if let Err(e) = fs.rt.block_on(write::trash(fs.shared.clone(), n.clone())) {
                    eprintln!("re-fs: deleting {:?} failed: {}", n.name, describe(e));
                }
            }
            (Open::Upload(u), true) => fs.cancel(&u),
            (Open::Upload(u), false) if h.creator.load(Ordering::Relaxed) => fs.commit(&u),
            _ => {}
        }
        STATUS_SUCCESS
    });
}

unsafe extern "system" fn close(f: *mut FileSystem, ctx: *mut c_void) {
    guarded(|| {
        let h = Box::from_raw(ctx as *mut Handle);
        // A created file Cleanup never sent (nothing was written): forget it.
        if let (Open::Upload(u), true) = (h.get(), h.creator.load(Ordering::Relaxed)) {
            if !u.state.lock().unwrap().done {
                fs(f).cancel(&u);
            }
        }
        STATUS_SUCCESS
    });
}

unsafe extern "system" fn read(f: *mut FileSystem, ctx: *mut c_void, buf: *mut c_void, offset: u64, len: u32, done: *mut u32) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let out = std::slice::from_raw_parts_mut(buf.cast::<u8>(), len as usize);
        let mut local = |file: &File, size: u64| -> NTSTATUS {
            if offset >= size {
                return STATUS_END_OF_FILE;
            }
            let n = (size - offset).min(u64::from(len)) as usize;
            match crate::pio::read_exact_at(file, &mut out[..n], offset) {
                Ok(()) => {
                    *done = n as u32;
                    STATUS_SUCCESS
                }
                Err(e) => io_status(&e),
            }
        };
        let n = match handle(ctx).get() {
            // Reading back a file being copied in (some programs verify what they wrote).
            Open::Upload(u) => return local(&u.file, u.state.lock().unwrap().size),
            Open::Node(n) => n,
        };
        if let Some(gk) = GenKey::of(&n) {
            let Some(file) = fs.shared.cache.generated(&gk) else { return STATUS_UNEXPECTED_IO_ERROR };
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            return local(&file, size);
        }
        let Some(key) = Key::of(&n) else { return STATUS_ACCESS_DENIED };
        if offset >= key.size {
            return STATUS_END_OF_FILE;
        }
        if let Some(file) = fs.shared.cache.complete(&key) {
            return local(&file, key.size);
        }
        // Not cached: wait for the download (or read straight from the tablet).
        match fs.rt.block_on(fs.shared.cache.read(&fs.shared, &key, offset, len)) {
            Ok(data) => {
                out[..data.len()].copy_from_slice(&data);
                *done = data.len() as u32;
                STATUS_SUCCESS
            }
            Err(e) => {
                fs.shared.log(format_args!("read {}{}: {e}", key.uuid, key.kind.suffix()));
                status(e.errno())
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn write(
    f: *mut FileSystem,
    ctx: *mut c_void,
    buf: *mut c_void,
    offset: u64,
    len: u32,
    to_end: u8,
    constrained: u8,
    done: *mut u32,
    info: *mut FileInfo,
) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        // Documents on the tablet are replaced whole (--overwrite), never edited.
        let Open::Upload(u) = handle(ctx).get() else { return STATUS_ACCESS_DENIED };
        let mut st = u.state.lock().unwrap();
        if st.done {
            return STATUS_ACCESS_DENIED;
        }
        let offset = if to_end != 0 { st.size } else { offset };
        let mut n = u64::from(len);
        // Paging I/O: never past the end.
        if constrained != 0 {
            n = n.min(st.size.saturating_sub(offset));
        }
        let data = std::slice::from_raw_parts(buf.cast::<u8>(), n as usize);
        if let Err(e) = crate::pio::write_all_at(&u.file, data, offset) {
            return io_status(&e);
        }
        st.size = st.size.max(offset + n);
        drop(st);
        *done = n as u32;
        *info = fs.upload_info(&u);
        STATUS_SUCCESS
    })
}

unsafe extern "system" fn flush(f: *mut FileSystem, ctx: *mut c_void, info: *mut FileInfo) -> NTSTATUS {
    guarded(|| {
        // A null context flushes the volume: nothing to do.
        if !ctx.is_null() {
            *info = fs(f).open_info(&handle(ctx).get());
        }
        STATUS_SUCCESS
    })
}

unsafe extern "system" fn get_file_info(f: *mut FileSystem, ctx: *mut c_void, info: *mut FileInfo) -> NTSTATUS {
    guarded(|| {
        *info = fs(f).open_info(&handle(ctx).get());
        STATUS_SUCCESS
    })
}

/// Attributes and times: accepted so copying programs don't fail, and ignored.
unsafe extern "system" fn set_basic_info(f: *mut FileSystem, ctx: *mut c_void, _attrs: u32, _c: u64, _a: u64, _w: u64, _ch: u64, info: *mut FileInfo) -> NTSTATUS {
    get_file_info(f, ctx, info)
}

unsafe extern "system" fn set_file_size(f: *mut FileSystem, ctx: *mut c_void, size: u64, allocation: u8, info: *mut FileInfo) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let open = handle(ctx).get();
        match &open {
            Open::Upload(u) if allocation == 0 => {
                let mut st = u.state.lock().unwrap();
                if st.done {
                    return STATUS_ACCESS_DENIED;
                }
                if let Err(e) = u.file.set_len(size) {
                    return io_status(&e);
                }
                st.size = size;
            }
            // Preallocation (Explorer does it before copying): nothing to reserve.
            Open::Upload(_) => {}
            Open::Node(_) if allocation != 0 => {}
            Open::Node(_) => {
                if fs.open_info(&open).file_size != size {
                    return STATUS_ACCESS_DENIED;
                }
            }
        }
        *info = fs.open_info(&open);
        STATUS_SUCCESS
    })
}

/// Whether a delete may go ahead (it happens in Cleanup, which can't fail).
unsafe extern "system" fn can_delete(f: *mut FileSystem, ctx: *mut c_void, _name: PWSTR) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let Open::Node(n) = handle(ctx).get() else { return STATUS_SUCCESS };
        let tree = fs.tree();
        let Some(n) = tree.node(n.ino) else { return STATUS_OBJECT_NAME_NOT_FOUND };
        if n.kind == NodeKind::Root || !n.is_primary() {
            return STATUS_ACCESS_DENIED;
        }
        let parent = tree.parent(n);
        if let Err(s) = fs.writable_dir(parent) {
            return s;
        }
        // Items already in the trash stay there (write::trash refuses them too).
        if parent.kind == NodeKind::Trash {
            return STATUS_ACCESS_DENIED;
        }
        if n.kind.is_dir() && tree.children(n.ino).is_some_and(|mut c| c.next().is_some()) {
            return STATUS_DIRECTORY_NOT_EMPTY;
        }
        STATUS_SUCCESS
    })
}

unsafe extern "system" fn rename(f: *mut FileSystem, ctx: *mut c_void, name: PWSTR, new_name: PWSTR, replace: u8) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        if let Open::Upload(_) = handle(ctx).get() {
            return STATUS_ACCESS_DENIED; // still being copied in
        }
        let tree = fs.tree();
        let node = match resolve(&tree, &from_pwstr(name)) {
            Ok(n) => n,
            Err(s) => return s,
        };
        let new_path = from_pwstr(new_name);
        let (dir_path, leaf) = split(&new_path);
        let dir = match resolve(&tree, dir_path) {
            Ok(d) if d.kind.is_dir() => d,
            _ => return STATUS_OBJECT_PATH_NOT_FOUND,
        };
        if let Err(s) = fs.writable_dir(&dir) {
            return s;
        }
        if !node.is_primary() {
            return STATUS_ACCESS_DENIED; // derived files (X.annotated.pdf …) follow their document
        }
        // No overwriting by default: replacing a document would lose its annotations.
        // With --overwrite a file may replace a file; the old one goes to the trash.
        let target = tree.lookup_nocase(dir.ino, leaf).filter(|t| t.ino != node.ino).cloned();
        match &target {
            None => {}
            Some(_) if replace == 0 => return STATUS_OBJECT_NAME_COLLISION,
            Some(t) if !fs.shared.overwrite || !t.is_primary() || t.kind.is_dir() || node.kind.is_dir() => return STATUS_ACCESS_DENIED,
            Some(_) => {}
        }
        if tree.parent(&node).ino == dir.ino && node.name == leaf {
            return STATUS_SUCCESS;
        }
        match fs.rt.block_on(write::rename(fs.shared.clone(), node, dir.ino, naming::from_windows_name(leaf), target)) {
            Ok(()) => STATUS_SUCCESS,
            Err(e) => status(e),
        }
    })
}

unsafe extern "system" fn get_security(f: *mut FileSystem, _ctx: *mut c_void, sd: *mut c_void, sd_size: *mut usize) -> NTSTATUS {
    guarded(|| fs(f).put_sd(sd, sd_size))
}

/// Permissions aren't stored on the tablet: accepted and ignored (like chmod on Linux).
unsafe extern "system" fn set_security(_f: *mut FileSystem, _ctx: *mut c_void, _what: u32, _sd: *mut c_void) -> NTSTATUS {
    STATUS_SUCCESS
}

unsafe extern "system" fn read_directory(f: *mut FileSystem, ctx: *mut c_void, _pattern: PWSTR, marker: PWSTR, buf: *mut c_void, len: u32, done: *mut u32) -> NTSTATUS {
    guarded(|| {
        let fs = fs(f);
        let Ok(api) = api() else { return STATUS_INVALID_DEVICE_REQUEST };
        let Open::Node(dir) = handle(ctx).get() else { return STATUS_NOT_A_DIRECTORY };
        let tree = fs.tree();
        let Some(dir) = tree.node(dir.ino) else { return STATUS_OBJECT_NAME_NOT_FOUND };
        let Some(children) = tree.children(dir.ino) else { return STATUS_NOT_A_DIRECTORY };
        // "." and ".." first (not in the root), then the children in name order. The
        // marker is the last name already returned.
        let mut entries: Vec<(&str, FileInfo)> = Vec::new();
        if dir.kind != NodeKind::Root {
            entries.push((".", fs.node_info(dir)));
            entries.push(("..", fs.node_info(tree.parent(dir))));
        }
        let dots = entries.len();
        entries.extend(children.map(|n| (n.name.as_str(), fs.node_info(n))));
        let start = match (marker.is_null(), from_pwstr(marker)) {
            (true, _) => 0,
            (false, m) if m == "." && dots > 0 => 1,
            (false, m) if m == ".." && dots > 0 => 2,
            (false, m) => dots + entries[dots..].iter().position(|(name, _)| *name > m.as_str()).unwrap_or(entries.len() - dots),
        };
        *done = 0;
        let mut record: Vec<u64> = Vec::new();
        for (name, info) in &entries[start..] {
            let wname: Vec<u16> = name.encode_utf16().collect();
            let size = DIR_INFO_SIZE + wname.len() * 2;
            record.clear();
            record.resize(size.div_ceil(8), 0);
            let di = record.as_mut_ptr().cast::<DirInfo>();
            (*di).size = size as u16;
            (*di).file_info = *info;
            std::ptr::copy_nonoverlapping(wname.as_ptr(), record.as_mut_ptr().cast::<u8>().add(DIR_INFO_SIZE).cast::<u16>(), wname.len());
            if (api.add_dir_info)(di, buf, len, done) == 0 {
                return STATUS_SUCCESS; // full; the next call continues after the last one added
            }
        }
        // The end of the listing.
        (api.add_dir_info)(std::ptr::null_mut(), buf, len, done);
        STATUS_SUCCESS
    })
}

static INTERFACE: Interface = Interface {
    get_volume_info: Some(get_volume_info),
    set_volume_label: 0,
    get_security_by_name: Some(get_security_by_name),
    create: Some(create),
    open: Some(open),
    overwrite: Some(overwrite),
    cleanup: Some(cleanup),
    close: Some(close),
    read: Some(read),
    write: Some(write),
    flush: Some(flush),
    get_file_info: Some(get_file_info),
    set_basic_info: Some(set_basic_info),
    set_file_size: Some(set_file_size),
    can_delete: Some(can_delete),
    rename: Some(rename),
    get_security: Some(get_security),
    set_security: Some(set_security),
    read_directory: Some(read_directory),
    unused: [0; 45],
};

// ---- mounting -------------------------------------------------------------------------

/// FSP_FILE_SYSTEM*, shared with the notifier; null once unmounted.
struct FsPtr(*mut FileSystem);
// SAFETY: WinFsp's API is thread-safe; the pointer is only used under the mutex.
unsafe impl Send for FsPtr {}

struct CtxPtr(*const Fs);
// SAFETY: Fs is Send + Sync; this is just its Arc in raw form.
unsafe impl Send for CtxPtr {}

pub struct Mounted {
    api: &'static Api,
    fs: Arc<Mutex<FsPtr>>,
    ctx: CtxPtr,
    point: String,
}

impl Mounted {
    /// Mount on `mountpoint`: a drive ("R:"), a folder that doesn't exist yet, or "*"
    /// for the next free drive letter.
    pub fn start(shared: Arc<Shared>, rt: tokio::runtime::Handle, mountpoint: &Path, writable: bool) -> Result<Mounted> {
        let api = api().map_err(anyhow::Error::msg)?;
        // SAFETY: VolumeParams is plain data; all-zero is its documented default.
        let mut params: VolumeParams = unsafe { std::mem::zeroed() };
        params.version = std::mem::size_of::<VolumeParams>() as u16;
        params.sector_size = 4096;
        params.sectors_per_allocation_unit = 1;
        params.max_component_length = naming::NAME_MAX as u16;
        params.volume_creation_time = filetime(now_ms());
        params.volume_serial_number = 0x7e4a_b1e5;
        params.file_info_timeout = FILE_INFO_TIMEOUT_MS;
        params.flags = vp::CASE_PRESERVED_NAMES
            | vp::UNICODE_ON_DISK
            | vp::PERSISTENT_ACLS
            | vp::POST_CLEANUP_WHEN_MODIFIED_ONLY
            | vp::UM_FILE_CONTEXT_IS_USER_CONTEXT2
            | if writable { 0 } else { vp::READ_ONLY_VOLUME };
        let fs_name: Vec<u16> = "re-fs".encode_utf16().collect();
        params.file_system_name[..fs_name.len()].copy_from_slice(&fs_name);

        let ctx = Arc::into_raw(Arc::new(Fs { shared: shared.clone(), rt, sd: security_descriptor()?, uploads: Mutex::default(), next_upload: AtomicU64::new(0) }));
        let mut fsp: *mut FileSystem = std::ptr::null_mut();
        let mut device = wide("WinFsp.Disk");
        // SAFETY: the arguments are as winfsp.h declares; INTERFACE is 'static.
        let r = unsafe { (api.create)(device.as_mut_ptr(), &params, &INTERFACE, &mut fsp) };
        if r != STATUS_SUCCESS {
            unsafe { drop(Arc::from_raw(ctx)) };
            bail!("WinFsp couldn't create the file system (NTSTATUS {:#010x}); is the WinFsp driver installed?", r as u32);
        }
        // SAFETY: fsp is a live FSP_FILE_SYSTEM; callbacks read UserContext back as &Fs.
        unsafe { (*fsp.cast::<FileSystemHead>()).user_context = ctx as *mut c_void };
        let mut mounted = Mounted { api, fs: Arc::new(Mutex::new(FsPtr(fsp))), ctx: CtxPtr(ctx), point: String::new() };

        let mp = mountpoint.to_string_lossy();
        let mut mp_wide = (mp != "*").then(|| wide(&mp));
        let r = unsafe { (api.set_mount_point)(fsp, mp_wide.as_mut().map_or(std::ptr::null_mut(), |w| w.as_mut_ptr())) };
        if r != STATUS_SUCCESS {
            bail!("can't mount on {mp} (NTSTATUS {:#010x}): is the drive letter taken, or the folder already there?", r as u32);
        }
        let r = unsafe { (api.start_dispatcher)(fsp, 0) };
        if r != STATUS_SUCCESS {
            bail!("WinFsp couldn't start its dispatcher (NTSTATUS {:#010x})", r as u32);
        }
        mounted.point = unsafe { from_pwstr((api.mount_point)(fsp)) };
        *shared.notifier.lock().unwrap() = Some(Arc::new(Notifier { api, fs: mounted.fs.clone() }));
        Ok(mounted)
    }

    /// Where it ended up, e.g. "R:".
    pub fn mount_point(&self) -> &str {
        &self.point
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let mut fs = self.fs.lock().unwrap();
        if !fs.0.is_null() {
            // SAFETY: stops the callbacks before the file system and its context go.
            unsafe {
                (self.api.stop_dispatcher)(fs.0);
                (self.api.delete)(fs.0);
            }
            fs.0 = std::ptr::null_mut();
        }
        // SAFETY: from Arc::into_raw in start; no callbacks run any more.
        unsafe { drop(Arc::from_raw(self.ctx.0)) };
    }
}

/// SDDL → a self-relative security descriptor.
fn security_descriptor() -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    let sddl = wide(SDDL);
    let mut sd: *mut c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: on success sd points to len bytes, freed with LocalFree.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, &mut len) == 0 {
            bail!("building the security descriptor: {}", std::io::Error::last_os_error());
        }
        let v = std::slice::from_raw_parts(sd.cast::<u8>(), len as usize).to_vec();
        LocalFree(sd);
        Ok(v)
    }
}

// ---- change notifications --------------------------------------------------------------

/// The WinFsp side of `session::Invalidate`: tells Windows (and so Explorer) what
/// changed, by path.
struct Notifier {
    api: &'static Api,
    fs: Arc<Mutex<FsPtr>>,
}

impl crate::session::Invalidate for Notifier {
    // Everything goes through tree_changed, which has the paths.
    fn inode(&self, _ino: u64) {}
    fn entry(&self, _parent: u64, _name: &str) {}

    fn tree_changed(&self, old: &Tree, new: &Tree, inval: &[Invalidation]) {
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        let mut add = |path: String, filter: u32, action: u32| {
            if seen.insert((path.clone(), action)) {
                changes.push((path, filter, action));
            }
        };
        for i in inval {
            match i {
                Invalidation::Entry { parent, name } => {
                    let (was, is) = (old.lookup(*parent, name), new.lookup(*parent, name));
                    let (tree, node, action) = match (was, is) {
                        (Some(o), None) => (old, o, FILE_ACTION_REMOVED),
                        (None, Some(n)) => (new, n, FILE_ACTION_ADDED),
                        (Some(_), Some(n)) => (new, n, FILE_ACTION_MODIFIED),
                        (None, None) => continue,
                    };
                    let filter = if node.kind.is_dir() { FILE_NOTIFY_CHANGE_DIR_NAME } else { FILE_NOTIFY_CHANGE_FILE_NAME };
                    add(win_path(&tree.path(node)), filter, action);
                }
                // Folders change through their entries; files can change in place.
                Invalidation::Inode(ino) => match new.node(*ino) {
                    Some(n) if !n.kind.is_dir() => add(
                        win_path(&new.path(n)),
                        FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                        FILE_ACTION_MODIFIED,
                    ),
                    _ => {}
                },
            }
        }
        if !changes.is_empty() {
            self.send(&changes);
        }
    }
}

impl Notifier {
    fn send(&self, changes: &[(String, u32, u32)]) {
        let fs = self.fs.lock().unwrap();
        if fs.0.is_null() {
            return;
        }
        let api = self.api;
        // NotifyBegin waits out renames in progress (a rename waits for the tree
        // that brought us here, so it finishes). Give up after a few seconds; the
        // file info timeout bounds how stale Explorer can get.
        // SAFETY (this block): fs.0 is live while we hold the lock.
        if !(0..10).any(|_| unsafe { (api.notify_begin)(fs.0, 500) } == STATUS_SUCCESS) {
            return;
        }
        let mut buf = vec![0u64; 1024];
        let cap = (buf.len() * 8) as u32;
        let mut used = 0u32;
        let mut record: Vec<u64> = Vec::new();
        for (path, filter, action) in changes {
            let wpath: Vec<u16> = path.encode_utf16().collect();
            let size = NOTIFY_INFO_SIZE + wpath.len() * 2;
            record.clear();
            record.resize(size.div_ceil(8), 0);
            let info = record.as_mut_ptr().cast::<NotifyInfo>();
            unsafe {
                (*info).size = size as u16;
                (*info).filter = *filter;
                (*info).action = *action;
                std::ptr::copy_nonoverlapping(wpath.as_ptr(), record.as_mut_ptr().cast::<u8>().add(NOTIFY_INFO_SIZE).cast::<u16>(), wpath.len());
                if (api.add_notify_info)(info, buf.as_mut_ptr().cast(), cap, &mut used) == 0 {
                    // Full: send what we have and start over.
                    (api.notify)(fs.0, buf.as_mut_ptr().cast(), used as usize);
                    used = 0;
                    (api.add_notify_info)(info, buf.as_mut_ptr().cast(), cap, &mut used);
                }
            }
        }
        unsafe {
            if used > 0 {
                (api.notify)(fs.0, buf.as_mut_ptr().cast(), used as usize);
            }
            (api.notify_end)(fs.0);
        }
    }
}
