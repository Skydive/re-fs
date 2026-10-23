//! Filesystem writes: each one becomes a `Plan` (re_proto::index::plan), is applied
//! by the agent, and completes once the client's tree includes it.

use std::fs::File;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use libc::{EBUSY, EHOSTDOWN, EINVAL, EIO, ENOENT, ENOTEMPTY, EPERM, ETIMEDOUT};
use re_proto::index::naming;
use re_proto::index::parse::{Metadata, ParentRef};
use re_proto::index::plan::{self, DocKind};
use re_proto::index::tree::{Node, NodeKind, Tree};
use re_proto::index::FileKind;
use re_proto::wire::{ErrorCode, Expect, Plan};
use uuid::Uuid;

use crate::client::{CallError, Client};
use crate::session::Shared;

pub type Errno = i32;

/// Upload chunk size.
const CHUNK: usize = 4 << 20;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn client(shared: &Shared) -> Result<Arc<Client>, Errno> {
    shared.client.load_full().ok_or(EHOSTDOWN)
}

fn errno(e: CallError) -> Errno {
    match e {
        CallError::Closed => EHOSTDOWN,
        // The tablet changed the item since we last looked; the caller can retry.
        CallError::Remote { code: ErrorCode::Conflict, .. } => libc::EAGAIN,
        CallError::Remote { code: ErrorCode::NotFound, .. } => ENOENT,
        _ => EIO,
    }
}

/// Where children of directory `ino` hang on the tablet.
pub fn parent_ref(tree: &Tree, ino: u64) -> Result<ParentRef, Errno> {
    let n = tree.node(ino).ok_or(ENOENT)?;
    match n.kind {
        NodeKind::Root => Ok(ParentRef::Root),
        NodeKind::Trash => Ok(ParentRef::Trash),
        NodeKind::Folder => Ok(ParentRef::Folder(n.uuid.expect("folders have uuids"))),
        NodeKind::LostFound => Err(EPERM),
        _ => Err(libc::ENOTDIR),
    }
}

/// Send a plan, then wait until our tree includes it.
async fn run(shared: &Shared, plan: Plan) -> Result<(), Errno> {
    let generation = client(shared)?.apply(plan).await.map_err(errno)?;
    let mut rx = shared.generation.subscribe();
    let seen = tokio::time::timeout(Duration::from_secs(15), async { rx.wait_for(|g| *g >= generation).await.is_ok() }).await;
    match seen {
        Ok(true) => Ok(()),
        Ok(false) => Err(EIO),
        Err(_) => Err(ETIMEDOUT),
    }
}

/// The plan's precondition for an existing item: its `.metadata` as we know it.
fn expect_unchanged(shared: &Shared, uuid: Uuid) -> Result<(Vec<u8>, Expect), Errno> {
    let entries = shared.entries.load();
    let e = entries.get(&uuid).ok_or(ENOENT)?;
    let st = e.stat(FileKind::Metadata).ok_or(ENOENT)?;
    Ok((e.metadata.clone(), Expect { uuid, metadata: Some((st.mtime_ms, st.size)) }))
}

pub async fn mkdir(shared: Arc<Shared>, parent: u64, name: String) -> Result<(), Errno> {
    let tree = shared.tree.load_full();
    let parent = match parent_ref(&tree, parent)? {
        ParentRef::Trash => return Err(EPERM),
        p => p,
    };
    let uuid = Uuid::new_v4();
    let visible = naming::visible_name(&name);
    let plan = Plan { expect: vec![Expect { uuid, metadata: None }], ops: plan::new_folder(uuid, &parent, &visible, now_ms()), restart: true };
    shared.log(format_args!("mkdir {name:?} as {uuid}"));
    run(&shared, plan).await
}

/// Upload a finished local file as a new document.
pub async fn import(shared: Arc<Shared>, parent: ParentRef, name: String, kind: DocKind, file: Arc<File>, size: u64) -> Result<(), Errno> {
    let mut head = [0u8; 64];
    let n = crate::pio::read_at(&file, &mut head, 0).map_err(|_| EIO)?;
    if !kind.sniff(&head[..n]) {
        shared.log(format_args!("{name:?} isn't a valid {} file", kind.ext()));
        return Err(EINVAL);
    }
    let c = client(&shared)?;
    let uuid = Uuid::new_v4();
    let token = crate::rand_u64();
    let mut off = 0u64;
    while off < size {
        let len = (size - off).min(CHUNK as u64) as usize;
        let mut buf = vec![0u8; len];
        crate::pio::read_exact_at(&file, &mut buf, off).map_err(|_| EIO)?;
        c.put_chunk(token, off, buf).await.map_err(errno)?;
        off += len as u64;
    }
    let base = naming::strip_ext(&name, kind.ext()).unwrap_or(&name);
    let visible = naming::visible_name(base);
    let ops = plan::new_document(uuid, &parent, &visible, kind, token, size, now_ms());
    shared.log(format_args!("import {name:?} ({size} bytes) as {uuid}"));
    run(&shared, Plan { expect: vec![Expect { uuid, metadata: None }], ops, restart: true }).await
}

pub async fn rename(shared: Arc<Shared>, node: Node, new_parent: u64, new_name: String) -> Result<(), Errno> {
    let tree = shared.tree.load_full();
    let uuid = node.uuid.ok_or(EBUSY)?;
    let parent = parent_ref(&tree, new_parent)?;
    let visible = if node.kind.is_dir() {
        // A folder can't move into itself or below itself.
        let mut cur = tree.node(new_parent).ok_or(ENOENT)?;
        loop {
            if cur.ino == node.ino {
                return Err(EINVAL);
            }
            if cur.kind == NodeKind::Root || cur.kind == NodeKind::Trash {
                break;
            }
            cur = tree.parent(cur);
        }
        naming::visible_name(&new_name)
    } else {
        // The type's extension is shown, not stored: "x.pdf" → "y.pdf" stores "y".
        // Any other new name is stored as typed ("x.pdf" → "notes.epub" stores
        // "notes.epub"); the file keeps its type, so it then shows as
        // "notes.epub.pdf".
        let base = naming::strip_ext(&new_name, node.kind.ext()).unwrap_or(&new_name);
        if base.is_empty() {
            return Err(EINVAL);
        }
        naming::visible_name(base)
    };
    let (raw, expect) = expect_unchanged(&shared, uuid)?;
    let old = Metadata::from_slice(&raw).map_err(|_| EIO)?;
    let parent_change = (old.parent != parent).then_some(&parent);
    let name_change = (node.name != new_name).then_some(visible.as_str());
    if parent_change.is_none() && name_change.is_none() {
        return Ok(());
    }
    let json = plan::edit_metadata(&raw, parent_change, name_change, now_ms()).map_err(|_| EIO)?;
    shared.log(format_args!("rename {:?} → {new_name:?} ({uuid})", node.name));
    let ops = vec![re_proto::wire::Op::PutJson { uuid, file: FileKind::Metadata, json }];
    run(&shared, Plan { expect: vec![expect], ops, restart: true }).await
}

/// rm / rmdir: move to the tablet's trash. Items already in the trash stay there.
pub async fn trash(shared: Arc<Shared>, node: Node) -> Result<(), Errno> {
    let uuid = node.uuid.ok_or(EBUSY)?;
    let tree = shared.tree.load_full();
    if tree.parent(&node).kind == NodeKind::Trash {
        return Err(EPERM);
    }
    if node.kind.is_dir() {
        // Hidden notebooks count too: look at the raw entries, not the tree.
        let entries = shared.entries.load();
        let busy = entries.values().any(|e| {
            Metadata::from_slice(&e.metadata).is_ok_and(|m| !m.deleted && m.parent == ParentRef::Folder(uuid))
        });
        if busy {
            return Err(ENOTEMPTY);
        }
    }
    let (raw, expect) = expect_unchanged(&shared, uuid)?;
    let json = plan::edit_metadata(&raw, Some(&ParentRef::Trash), None, now_ms()).map_err(|_| EIO)?;
    shared.log(format_args!("trash {:?} ({uuid})", node.name));
    let ops = vec![re_proto::wire::Op::PutJson { uuid, file: FileKind::Metadata, json }];
    run(&shared, Plan { expect: vec![expect], ops, restart: true }).await
}

