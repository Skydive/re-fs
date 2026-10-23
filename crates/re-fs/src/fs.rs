//! The FUSE side. Everything here answers from the in-memory tree snapshot, so
//! browsing never waits on the network.

use std::ffi::OsStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, ReplyStatfs, Request};
use libc::{EACCES, EHOSTDOWN, EISDIR, ENOENT, ENOTDIR, EROFS};
use re_proto::index::tree::Node;

use crate::cache::Key;
use crate::session::Shared;

/// How long the kernel may cache attributes and names. Changes from the tablet are
/// pushed as invalidations, so this can be long; it only bounds staleness if a
/// notification is ever missed.
const TTL: Duration = Duration::from_secs(60);

pub struct ReFs {
    shared: Arc<Shared>,
    rt: tokio::runtime::Handle,
    uid: u32,
    gid: u32,
}

impl ReFs {
    pub fn new(shared: Arc<Shared>, rt: tokio::runtime::Handle) -> Self {
        // SAFETY: getuid/getgid have no preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        ReFs { shared, rt, uid, gid }
    }

    fn attr(&self, n: &Node) -> FileAttr {
        let t = |ms: i64| UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64);
        let dir = n.kind.is_dir();
        FileAttr {
            ino: n.ino,
            size: n.size,
            blocks: n.size.div_ceil(512),
            atime: t(n.mtime_ms),
            mtime: t(n.mtime_ms),
            ctime: t(n.mtime_ms),
            crtime: t(n.ctime_ms),
            kind: if dir { FileType::Directory } else { FileType::RegularFile },
            perm: if dir { 0o555 } else { 0o444 },
            nlink: if dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

impl Filesystem for ReFs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let tree = self.shared.tree.load();
        match name.to_str().and_then(|name| tree.lookup(parent, name)) {
            Some(n) => reply.entry(&TTL, &self.attr(n), 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.shared.tree.load().node(ino) {
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
            return reply.error(EROFS);
        }
        let tree = self.shared.tree.load();
        let Some(n) = tree.node(ino) else { return reply.error(ENOENT) };
        if n.kind.is_dir() {
            return reply.error(EISDIR);
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

    fn read(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, offset: i64, size: u32, _flags: i32, _lock: Option<u64>, reply: ReplyData) {
        let tree = self.shared.tree.load();
        let Some(key) = tree.node(ino).and_then(Key::of) else { return reply.error(ENOENT) };
        let offset = offset.max(0) as u64;
        // Cached: answer on this thread.
        if let Some(f) = self.shared.cache.complete(&key) {
            let end = (offset + u64::from(size)).min(key.size);
            let mut buf = vec![0u8; end.saturating_sub(offset) as usize];
            return match std::os::unix::fs::FileExt::read_exact_at(&f, &mut buf, offset) {
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

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        let tree = self.shared.tree.load();
        let used: u64 = tree.nodes().map(|n| n.size.div_ceil(4096)).sum();
        reply.statfs(used, 0, 0, tree.nodes().count() as u64, 0, 4096, 255, 4096);
    }
}

/// Mount options shared by every mount.
pub fn mount_options() -> Vec<fuser::MountOption> {
    use fuser::MountOption::*;
    vec![FSName("re-fs".into()), Subtype("re-fs".into()), RO, NoAtime, NoDev, NoSuid, NoExec, DefaultPermissions]
}
