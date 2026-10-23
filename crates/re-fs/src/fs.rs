//! The FUSE side. Lookups and listings answer from the in-memory tree snapshot, so
//! browsing never waits on the network. Writes (with --rw) are handed to tokio and
//! answered once the tablet has applied them (see write.rs).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs,
    ReplyWrite, ReplyXattr, Request, TimeOrNow,
};
use libc::{EACCES, EBADF, EEXIST, EHOSTDOWN, EINVAL, EISDIR, ENOENT, ENOTDIR, EPERM, EROFS};
use re_proto::index::parse::ParentRef;
use re_proto::index::plan::DocKind;
use re_proto::index::tree::{Node, NodeKind, View};

use crate::cache::Key;
use crate::generate::{self, GenKey};
use crate::session::Shared;
use crate::write;

/// How long the kernel may cache attributes and names. Changes from the tablet are
/// pushed as invalidations, so this can be long; it only bounds staleness if a
/// notification is ever missed.
const TTL: Duration = Duration::from_secs(60);
/// Inodes for files being copied in, until the tablet has them.
const FIRST_UPLOAD_INO: u64 = 1 << 62;

/// A file being copied in: collected locally, sent to the tablet on close.
struct Upload {
    parent: ParentRef,
    parent_ino: u64,
    name: String,
    kind: DocKind,
    path: PathBuf,
    file: Arc<File>,
    size: u64,
    created: SystemTime,
    /// Set once the tablet has it (or it failed); later flushes repeat the result.
    committed: Option<Result<(), i32>>,
}

pub struct ReFs {
    shared: Arc<Shared>,
    rt: tokio::runtime::Handle,
    uid: u32,
    gid: u32,
    uploads: Arc<Mutex<HashMap<u64, Upload>>>,
    next_upload: u64,
}

impl ReFs {
    pub fn new(shared: Arc<Shared>, rt: tokio::runtime::Handle) -> Self {
        // SAFETY: getuid/getgid have no preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        ReFs { shared, rt, uid, gid, uploads: Arc::default(), next_upload: FIRST_UPLOAD_INO }
    }

    fn base_attr(&self, ino: u64, dir: bool, size: u64) -> FileAttr {
        // With --rw directories are writable (mkdir, mv, rm); files never are:
        // a document is replaced by copying a new one in, not edited in place.
        let w = if self.shared.writable { 0o200 } else { 0 };
        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: if dir { FileType::Directory } else { FileType::RegularFile },
            perm: if dir { 0o555 | w } else { 0o444 },
            nlink: if dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn attr(&self, n: &Node) -> FileAttr {
        let t = |ms: i64| UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64);
        // Generated files: their size once made, 0 until then (read with direct I/O).
        let size = match GenKey::of(n) {
            Some(k) => self.shared.cache.generated_size(&k).unwrap_or(0),
            None => n.size,
        };
        let mut a = FileAttr {
            atime: t(n.mtime_ms),
            mtime: t(n.mtime_ms),
            ctime: t(n.mtime_ms),
            crtime: t(n.ctime_ms),
            ..self.base_attr(n.ino, n.kind.is_dir(), size)
        };
        // Only the main tree's own documents and folders can be changed.
        if !(n.view == View::Main && (n.kind.is_dir() || n.is_primary())) {
            a.perm &= !0o222;
        }
        a
    }

    fn upload_attr(&self, ino: u64, u: &Upload) -> FileAttr {
        let mut a = self.base_attr(ino, false, u.size);
        (a.atime, a.mtime, a.ctime, a.crtime) = (u.created, u.created, u.created, u.created);
        a.perm = 0o644; // the copying program may write and chmod it
        a
    }

    /// Run a write on tokio and answer `reply` with its result.
    fn spawn_write<F>(&self, reply: ReplyEmpty, f: F)
    where
        F: std::future::Future<Output = Result<(), i32>> + Send + 'static,
    {
        let shared = self.shared.clone();
        self.rt.spawn(async move {
            match f.await {
                Ok(()) => reply.ok(),
                Err(e) => {
                    shared.log(format_args!("write failed: {}", std::io::Error::from_raw_os_error(e)));
                    reply.error(e)
                }
            }
        });
    }

    fn writable_dir(&self, ino: u64) -> Result<(), i32> {
        if !self.shared.writable {
            return Err(EROFS);
        }
        if !self.shared.online.load(Ordering::Relaxed) {
            return Err(EHOSTDOWN);
        }
        match self.shared.tree.load().node(ino).map(|n| (n.kind, n.view)) {
            Some((NodeKind::Root | NodeKind::Folder | NodeKind::Trash, View::Main)) => Ok(()),
            Some((NodeKind::LostFound | NodeKind::ViewDir | NodeKind::Folder, _)) => Err(EPERM),
            Some(_) => Err(ENOTDIR),
            None => Err(ENOENT),
        }
    }

    /// Extended attributes: `user.rm.*` from the tablet's metadata.
    fn xattrs(n: &Node) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if let Some(u) = n.uuid {
            v.push(("user.rm.uuid", u.to_string()));
        }
        if let Some(d) = &n.doc {
            if let Some(p) = d.pages {
                v.push(("user.rm.pages", p.to_string()));
            }
            if let Some(p) = d.last_page {
                v.push(("user.rm.last_page", p.to_string()));
            }
            if !d.tags.is_empty() {
                v.push(("user.rm.tags", d.tags.join(",")));
            }
            if d.pinned {
                v.push(("user.rm.pinned", "1".into()));
            }
        }
        v
    }
}

impl Filesystem for ReFs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let tree = self.shared.tree.load();
        let Some(name) = name.to_str() else { return reply.error(ENOENT) };
        if let Some(n) = tree.lookup(parent, name) {
            return reply.entry(&TTL, &self.attr(n), 0);
        }
        let uploads = self.uploads.lock().unwrap();
        match uploads.iter().find(|(_, u)| u.parent_ino == parent && u.name == name && u.committed.is_none()) {
            Some((&ino, u)) => reply.entry(&Duration::ZERO, &self.upload_attr(ino, u), 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if let Some(n) = self.shared.tree.load().node(ino) {
            return reply.attr(&TTL, &self.attr(n));
        }
        match self.uploads.lock().unwrap().get(&ino) {
            Some(u) => reply.attr(&Duration::ZERO, &self.upload_attr(ino, u)),
            None => reply.error(ENOENT),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // Uploads: truncation is real; modes and times are accepted and ignored.
        if let Some(u) = self.uploads.lock().unwrap().get_mut(&ino) {
            if let Some(sz) = size {
                if let Err(e) = u.file.set_len(sz) {
                    return reply.error(e.raw_os_error().unwrap_or(libc::EIO));
                }
                u.size = sz;
            }
            return reply.attr(&Duration::ZERO, &self.upload_attr(ino, u));
        }
        // Tablet documents: their size can't change; modes and times (cp -p, touch)
        // are accepted so tools don't fail, and ignored.
        match self.shared.tree.load().node(ino) {
            Some(_) if size.is_some() => reply.error(EROFS),
            Some(n) => reply.attr(&TTL, &self.attr(n)),
            None => reply.error(ENOENT),
        }
    }

    fn opendir(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.shared.tree.load().node(ino) {
            Some(n) if n.kind.is_dir() => reply.opened(0, 0),
            Some(_) => reply.error(ENOTDIR),
            None => reply.error(ENOENT),
        }
    }

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let tree = self.shared.tree.load();
        let Some(dir) = tree.node(ino) else { return reply.error(ENOENT) };
        let Some(children) = tree.children(ino) else { return reply.error(ENOTDIR) };
        let parent = tree.parent(dir).ino;
        let dots = [(dir.ino, FileType::Directory, "."), (parent, FileType::Directory, "..")];
        let kids = children.map(|n| (n.ino, if n.kind.is_dir() { FileType::Directory } else { FileType::RegularFile }, n.name.as_str()));
        for (i, (ino, kind, name)) in dots.into_iter().chain(kids).enumerate().skip(offset as usize) {
            if reply.add(ino, i as i64 + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        if flags & libc::O_ACCMODE != libc::O_RDONLY {
            return reply.error(if self.uploads.lock().unwrap().contains_key(&ino) { EBADF } else { EROFS });
        }
        let tree = self.shared.tree.load();
        let Some(n) = tree.node(ino) else { return reply.error(ENOENT) };
        if n.kind.is_dir() {
            return reply.error(EISDIR);
        }
        if let Some(gk) = GenKey::of(n) {
            if self.shared.cache.generated(&gk).is_some() {
                return reply.opened(0, fuser::consts::FOPEN_DIRECT_IO);
            }
            if !self.shared.online.load(Ordering::Relaxed) {
                return reply.error(EHOSTDOWN);
            }
            // Make it now (rendering can take a moment); reads then come from the cache.
            let (shared, node) = (self.shared.clone(), n.clone());
            self.rt.spawn(async move {
                match generate::ensure(&shared, &node).await {
                    Ok(_) => {
                        reply.opened(0, fuser::consts::FOPEN_DIRECT_IO);
                        // Its size is known now; `ls -l` shouldn't keep showing 0.
                        shared.invalidate_inode(node.ino).await;
                    }
                    Err(e) => {
                        shared.log(format_args!("making {:?}: {e}", node.name));
                        reply.error(e.errno())
                    }
                }
            });
            return;
        }
        let Some(key) = Key::of(n) else { return reply.error(EACCES) };
        // Fail here rather than in read: read errors pass through the page cache,
        // which turns every one of them into EIO.
        if !self.shared.online.load(Ordering::Relaxed) && self.shared.cache.complete(&key).is_none() {
            return reply.error(EHOSTDOWN);
        }
        // Start the download now, so the first read finds data on its way.
        let _guard = self.rt.enter();
        self.shared.cache.prefetch(&self.shared, &key);
        // Keep the kernel's page cache across opens; a change on the tablet
        // invalidates the inode (and so its pages).
        reply.opened(0, fuser::consts::FOPEN_KEEP_CACHE);
    }

    fn read(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, size: u32, _flags: i32, _lock: Option<u64>, reply: ReplyData) {
        let offset = offset.max(0) as u64;
        // Reading back a file being copied in (some tools verify what they wrote).
        if let Some(u) = self.uploads.lock().unwrap().get(&fh).filter(|_| fh >= FIRST_UPLOAD_INO) {
            let end = (offset + u64::from(size)).min(u.size);
            let mut buf = vec![0u8; end.saturating_sub(offset) as usize];
            return match u.file.read_exact_at(&mut buf, offset) {
                Ok(()) => reply.data(&buf),
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            };
        }
        let tree = self.shared.tree.load();
        if let Some(gk) = tree.node(ino).and_then(GenKey::of) {
            let Some(f) = self.shared.cache.generated(&gk) else { return reply.error(libc::EIO) };
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            let end = (offset + u64::from(size)).min(len);
            let mut buf = vec![0u8; end.saturating_sub(offset) as usize];
            return match f.read_exact_at(&mut buf, offset) {
                Ok(()) => reply.data(&buf),
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            };
        }
        let Some(key) = tree.node(ino).and_then(Key::of) else { return reply.error(ENOENT) };
        // Cached: answer on this thread.
        if let Some(f) = self.shared.cache.complete(&key) {
            let end = (offset + u64::from(size)).min(key.size);
            let mut buf = vec![0u8; end.saturating_sub(offset) as usize];
            return match f.read_exact_at(&mut buf, offset) {
                Ok(()) => reply.data(&buf),
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            };
        }
        // Not yet: wait for the download (or the tablet) without blocking FUSE.
        let shared = self.shared.clone();
        self.rt.spawn(async move {
            match shared.cache.read(&shared, &key, offset, size).await {
                Ok(buf) => reply.data(&buf),
                Err(e) => {
                    shared.log(format_args!("read {}{}: {e}", key.uuid, key.kind.suffix()));
                    reply.error(e.errno());
                }
            }
        });
    }

    // ---- writes (--rw) --------------------------------------------------------

    fn mkdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
        if let Err(e) = self.writable_dir(parent) {
            return reply.error(e);
        }
        let Some(name) = name.to_str().map(str::to_owned) else { return reply.error(EINVAL) };
        if self.shared.tree.load().lookup(parent, &name).is_some() {
            return reply.error(EEXIST);
        }
        let (shared, attr_of) = (self.shared.clone(), self.attr_fn());
        self.rt.spawn(async move {
            match write::mkdir(shared.clone(), parent, name.clone()).await {
                Ok(()) => match shared.tree.load().lookup(parent, &name) {
                    Some(n) => reply.entry(&TTL, &attr_of(n), 0),
                    None => reply.error(libc::EIO),
                },
                Err(e) => reply.error(e),
            }
        });
    }

    fn create(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, _mode: u32, _umask: u32, _flags: i32, reply: ReplyCreate) {
        if let Err(e) = self.writable_dir(parent) {
            return reply.error(e);
        }
        let tree = self.shared.tree.load();
        let Some(name) = name.to_str().map(str::to_owned) else { return reply.error(EINVAL) };
        // Only PDFs and EPUBs can live on the tablet.
        let Some(kind) = DocKind::from_name(&name) else { return reply.error(EINVAL) };
        if tree.lookup(parent, &name).is_some() {
            return reply.error(EEXIST);
        }
        let parent_ref = match write::parent_ref(&tree, parent) {
            Ok(ParentRef::Trash) => return reply.error(EPERM),
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let ino = self.next_upload;
        self.next_upload += 1;
        let (path, file) = match self.shared.cache.staging_file(ino) {
            Ok(x) => x,
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        };
        let u = Upload {
            parent: parent_ref,
            parent_ino: parent,
            name,
            kind,
            path,
            file: Arc::new(file),
            size: 0,
            created: SystemTime::now(),
            committed: None,
        };
        let attr = self.upload_attr(ino, &u);
        self.uploads.lock().unwrap().insert(ino, u);
        // fh = the upload's inode, so writes find it.
        reply.created(&Duration::ZERO, &attr, 0, ino, 0);
    }

    #[allow(clippy::too_many_arguments)]
    fn write(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, offset: i64, data: &[u8], _wf: u32, _flags: i32, _lock: Option<u64>, reply: ReplyWrite) {
        let mut uploads = self.uploads.lock().unwrap();
        let Some(u) = uploads.get_mut(&fh) else { return reply.error(EBADF) };
        if u.committed.is_some() {
            return reply.error(EBADF);
        }
        let offset = offset.max(0) as u64;
        match u.file.write_all_at(data, offset) {
            Ok(()) => {
                u.size = u.size.max(offset + data.len() as u64);
                reply.written(data.len() as u32)
            }
            Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        }
    }

    /// close(): send a copied-in file to the tablet. Errors here reach the
    /// program as close()'s return value, so `cp` reports them.
    fn flush(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        let (parent, name, kind, file, size) = {
            let uploads = self.uploads.lock().unwrap();
            let Some(u) = uploads.get(&fh) else { return reply.ok() };
            match u.committed {
                Some(Ok(())) => return reply.ok(),
                Some(Err(e)) => return reply.error(e),
                None => (u.parent.clone(), u.name.clone(), u.kind, u.file.clone(), u.size),
            }
        };
        let (shared, uploads) = (self.shared.clone(), self.uploads.clone());
        self.rt.spawn(async move {
            let r = write::import(shared, parent, name, kind, file, size).await;
            if let Some(u) = uploads.lock().unwrap().get_mut(&fh) {
                u.committed = Some(r);
            }
            match r {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e),
            }
        });
    }

    fn release(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, _flags: i32, _lock: Option<u64>, _flush: bool, reply: ReplyEmpty) {
        if let Some(u) = self.uploads.lock().unwrap().remove(&fh) {
            let _ = std::fs::remove_file(&u.path);
        }
        reply.ok();
    }

    fn rename(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr, flags: u32, reply: ReplyEmpty) {
        if let Err(e) = self.writable_dir(newparent) {
            return reply.error(e);
        }
        if flags & libc::RENAME_EXCHANGE != 0 {
            return reply.error(EINVAL);
        }
        let tree = self.shared.tree.load();
        let (Some(name), Some(newname)) = (name.to_str(), newname.to_str().map(str::to_owned)) else { return reply.error(EINVAL) };
        let Some(node) = tree.lookup(parent, name).cloned() else { return reply.error(ENOENT) };
        if !node.is_primary() {
            return reply.error(EPERM); // derived files (X.annotated.pdf …) follow their document
        }
        // No overwriting: replacing a document would orphan its annotations.
        if tree.lookup(newparent, &newname).is_some_and(|t| t.ino != node.ino) {
            return reply.error(EEXIST);
        }
        self.spawn_write(reply, write::rename(self.shared.clone(), node, newparent, newname));
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.remove(parent, name, false, reply);
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.remove(parent, name, true, reply);
    }

    fn getxattr(&mut self, _req: &Request<'_>, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        let tree = self.shared.tree.load();
        let Some(n) = tree.node(ino) else { return reply.error(ENOENT) };
        let Some((_, v)) = Self::xattrs(n).into_iter().find(|(k, _)| OsStr::new(k) == name) else { return reply.error(libc::ENODATA) };
        match size {
            0 => reply.size(v.len() as u32),
            s if (s as usize) < v.len() => reply.error(libc::ERANGE),
            _ => reply.data(v.as_bytes()),
        }
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        let tree = self.shared.tree.load();
        let Some(n) = tree.node(ino) else { return reply.error(ENOENT) };
        let mut names = Vec::new();
        for (k, _) in Self::xattrs(n) {
            names.extend_from_slice(k.as_bytes());
            names.push(0);
        }
        match size {
            0 => reply.size(names.len() as u32),
            s if (s as usize) < names.len() => reply.error(libc::ERANGE),
            _ => reply.data(&names),
        }
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        let tree = self.shared.tree.load();
        let used: u64 = tree.nodes().map(|n| n.size.div_ceil(4096)).sum();
        // Some file managers refuse to copy onto a filesystem reporting no free space.
        let free = if self.shared.writable { 1 << 20 } else { 0 };
        reply.statfs(used + free, free, free, tree.nodes().count() as u64, 1 << 20, 4096, 255, 4096);
    }
}

impl ReFs {
    fn attr_fn(&self) -> impl Fn(&Node) -> FileAttr + Send + 'static {
        let me = ReFs { shared: self.shared.clone(), rt: self.rt.clone(), uid: self.uid, gid: self.gid, uploads: Arc::default(), next_upload: 0 };
        move |n| me.attr(n)
    }

    fn remove(&mut self, parent: u64, name: &OsStr, dir: bool, reply: ReplyEmpty) {
        if let Err(e) = self.writable_dir(parent) {
            return reply.error(e);
        }
        let tree = self.shared.tree.load();
        let Some(node) = name.to_str().and_then(|n| tree.lookup(parent, n)).cloned() else { return reply.error(ENOENT) };
        if !node.is_primary() {
            return reply.error(EPERM);
        }
        match (dir, node.kind.is_dir()) {
            (false, true) => return reply.error(EISDIR),
            (true, false) => return reply.error(ENOTDIR),
            _ => {}
        }
        self.spawn_write(reply, write::trash(self.shared.clone(), node));
    }
}

/// Mount options shared by every mount.
pub fn mount_options(writable: bool) -> Vec<fuser::MountOption> {
    use fuser::MountOption::*;
    let mut o = vec![FSName("re-fs".into()), Subtype("re-fs".into()), NoAtime, NoDev, NoSuid, NoExec, DefaultPermissions];
    if !writable {
        o.push(RO);
    }
    o
}
