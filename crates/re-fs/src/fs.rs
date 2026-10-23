//! The FUSE side. Everything here answers from the in-memory tree snapshot, so
//! browsing never waits on the network.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, ReplyStatfs, Request};
use libc::{EACCES, EISDIR, ENOENT, ENOSYS, ENOTDIR, EROFS};
use re_proto::index::tree::Node;

use crate::session::Shared;

/// How long the kernel may cache attributes and names. Changes from the tablet are
/// pushed as invalidations, so this can be long; it only bounds staleness if a
/// notification is ever missed.
const TTL: Duration = Duration::from_secs(60);

pub struct ReFs {
    shared: Arc<Shared>,
    uid: u32,
    gid: u32,
}

impl ReFs {
    pub fn new(shared: Arc<Shared>) -> Self {
        // SAFETY: getuid/getgid have no preconditions.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        ReFs { shared, uid, gid }
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
        match self.shared.tree.load().node(ino) {
            Some(n) if n.kind.is_dir() => reply.error(EISDIR),
            Some(n) if n.kind.data_file().is_none() => reply.error(EACCES),
            Some(_) => reply.opened(0, 0),
            None => reply.error(ENOENT),
        }
    }

    fn read(&mut self, _req: &Request<'_>, _ino: u64, _fh: u64, _offset: i64, _size: u32, _flags: i32, _lock: Option<u64>, reply: ReplyData) {
        // M3: cache + fetch from the tablet.
        reply.error(ENOSYS);
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
