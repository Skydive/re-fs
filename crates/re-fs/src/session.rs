//! Keeps the tree in step with the tablet: connect, fetch the index, follow change
//! events, and reconnect with backoff when the link drops (e.g. the tablet sleeps).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use arc_swap::{ArcSwap, ArcSwapOption};
use re_proto::index::tree::{Invalidation, Tree, TreeOptions};
use re_proto::index::Entry;
use re_proto::wire::{decompress_entries, Response};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::cache::Cache;
use crate::client::Client;
use crate::transport::{self, Target};

/// What the mount backend (FUSE or WinFsp) must drop from the OS's caches when the
/// tree changes. Calls may block; they're made off the async workers.
pub trait Invalidate: Send + Sync {
    fn inode(&self, ino: u64);
    fn entry(&self, parent: u64, name: &str);

    /// One tree change at once. Backends that notify by path (WinFsp) need `old` to
    /// name what went away; inode-based ones (FUSE) take the default.
    fn tree_changed(&self, old: &Tree, new: &Tree, inval: &[Invalidation]) {
        let _ = (old, new);
        for i in inval {
            match i {
                Invalidation::Inode(ino) => self.inode(*ino),
                Invalidation::Entry { parent, name } => self.entry(*parent, name),
            }
        }
    }
}

pub struct Shared {
    pub tree: ArcSwap<Tree>,
    /// The raw entries the tree was built from (writes edit their JSON).
    pub entries: ArcSwap<HashMap<Uuid, Entry>>,
    /// Generation of the current tree, for writes waiting to see their result.
    pub generation: tokio::sync::watch::Sender<u64>,
    /// Writable (the default; not with --ro).
    pub writable: bool,
    /// With --overwrite: copying or moving onto a document replaces it.
    pub overwrite: bool,
    pub client: ArcSwapOption<Client>,
    pub online: AtomicBool,
    /// Tells the OS filesystem layer what it may have cached that changed.
    pub notifier: Mutex<Option<Arc<dyn Invalidate>>>,
    pub cache: Arc<Cache>,
    pub generator: crate::generate::Generator,
    pub verbose: bool,
}

/// What a mount lets programs change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writes {
    /// --ro: nothing.
    ReadOnly,
    /// The default: mkdir, copying documents in, mv and rm, never replacing one.
    NoOverwrite,
    /// --overwrite: also replace a document by copying or moving onto it.
    Overwrite,
}

impl Shared {
    pub fn new(opts: &TreeOptions, cache: Arc<Cache>, writes: Writes, verbose: bool) -> Arc<Self> {
        Arc::new(Shared {
            tree: ArcSwap::from_pointee(Tree::build(&[], 0, opts)),
            entries: ArcSwap::from_pointee(HashMap::new()),
            generation: tokio::sync::watch::Sender::new(0),
            writable: writes != Writes::ReadOnly,
            overwrite: writes == Writes::Overwrite,
            client: ArcSwapOption::empty(),
            online: AtomicBool::new(false),
            notifier: Mutex::new(None),
            cache,
            generator: Default::default(),
            verbose,
        })
    }

    /// Make the kernel forget an inode's cached attributes and pages (FUSE).
    #[cfg(unix)]
    pub async fn invalidate_inode(&self, ino: u64) {
        let notifier = self.notifier.lock().unwrap().clone();
        if let Some(n) = notifier {
            let _ = tokio::task::spawn_blocking(move || n.inode(ino)).await;
        }
    }

    pub fn log(&self, msg: std::fmt::Arguments) {
        if self.verbose {
            eprintln!("re-fs: {msg}");
        }
    }
}

struct Index {
    entries: HashMap<Uuid, Entry>,
    /// (agent instance, generation) we're in step with.
    at: Option<(u64, u64)>,
}

/// Keep a link to the tablet: on each (re)connect try `targets` in order (USB only
/// while plugged in), then follow changes on that link until it ends.
pub async fn run(targets: Vec<Target>, shared: Arc<Shared>, opts: TreeOptions, ready: oneshot::Sender<Result<()>>) {
    let mut ready = Some(ready);
    let mut index = Index { entries: HashMap::new(), at: None };
    // The last index we saw, so the mount works while the tablet is asleep.
    if let Some(saved) = shared.cache.load_index() {
        index.entries = saved.into_iter().map(|e| (e.uuid, e)).collect();
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        let mut errors = Vec::new();
        let mut was_up = false;
        for target in targets.iter().filter(|t| t.available()) {
            match connected(target, &shared, &opts, &mut index, &mut ready).await {
                Ok(()) => {
                    was_up = true;
                    break;
                }
                Err(e) => {
                    shared.log(format_args!("{target}: {e:#}"));
                    errors.push(format!("{target}: {e:#}"));
                }
            }
        }
        shared.online.store(false, Ordering::Relaxed);
        shared.client.store(None);
        if was_up {
            // The link ended after working: try again right away.
            backoff = Duration::from_secs(1);
            continue;
        }
        let why = if errors.is_empty() { "no transport available".to_string() } else { errors.join("; ") };
        if let Some(tx) = ready.take() {
            if index.entries.is_empty() {
                let _ = tx.send(Err(anyhow::anyhow!("{why}")));
                return;
            }
            eprintln!("re-fs: {why}; mounting the saved index offline (cached files only)");
            publish(&shared, &opts, &index, 0);
            let _ = tx.send(Ok(()));
        }
        shared.log(format_args!("offline; reconnecting in {backoff:?}"));
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn connected(
    target: &Target,
    shared: &Arc<Shared>,
    opts: &TreeOptions,
    index: &mut Index,
    ready: &mut Option<oneshot::Sender<Result<()>>>,
) -> Result<()> {
    let link = transport::connect(target).await?;
    let client = Client::start(link);
    let (agent, instance, _gen) = tokio::time::timeout(Duration::from_secs(20), client.hello())
        .await
        .map_err(|_| anyhow::anyhow!("no answer from the agent"))??;
    shared.log(format_args!("connected to {agent} via {target}"));
    let mut events = client.watch()?;
    sync(&client, shared, opts, index).await?;
    shared.client.store(Some(Arc::new(client.clone())));
    shared.online.store(true, Ordering::Relaxed);
    if let Some(tx) = ready.take() {
        let _ = tx.send(Ok(()));
    }
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(Response::Event { generation }) => {
                    if index.at != Some((instance, generation)) {
                        sync(&client, shared, opts, index).await?;
                    }
                }
                Some(_) => {}
                None => return Ok(()),
            },
            _ = client.closed() => return Ok(()),
        }
    }
}

/// Fetch the index (a delta when we can), rebuild the tree, invalidate what changed.
async fn sync(client: &Client, shared: &Shared, opts: &TreeOptions, index: &mut Index) -> Result<()> {
    let t0 = Instant::now();
    let Response::Index { instance, generation, full, removed, entries } = client.get_index(index.at).await? else {
        bail!("expected an index");
    };
    let n = entries.len();
    let entries = decompress_entries(&entries)?;
    if full {
        index.entries.clear();
    }
    for u in &removed {
        index.entries.remove(u);
    }
    let changed = entries.len() + removed.len();
    for e in entries {
        index.entries.insert(e.uuid, e);
    }
    index.at = Some((instance, generation));
    let (old, inval) = publish(shared, opts, index, generation);
    if changed > 0 {
        // Off the critical path: writes wait for the tree, not for this.
        let (cache, snapshot) = (shared.cache.clone(), shared.entries.load_full());
        tokio::task::spawn_blocking(move || cache.save_index(&snapshot.values().collect::<Vec<_>>()));
    }
    shared.log(format_args!(
        "generation {generation}: {} index, {changed} changed, {n} bytes ({:?}), {} invalidations, {:?}; link {:.1} MB/s",
        if full { "full" } else { "delta" },
        client.index_codec(),
        inval.len(),
        t0.elapsed(),
        client.rate() as f64 / 1e6
    ));
    invalidate(shared, old, inval).await;
    Ok(())
}

/// Build the tree from `index`, swap it in, and return the old tree and what the
/// kernel must forget.
fn publish(shared: &Shared, opts: &TreeOptions, index: &Index, generation: u64) -> (Arc<Tree>, Vec<Invalidation>) {
    let list: Vec<Entry> = index.entries.values().cloned().collect();
    let tree = Tree::build(&list, generation, opts);
    for w in tree.warnings() {
        shared.log(format_args!("warning: {w}"));
    }
    let old = shared.tree.swap(Arc::new(tree));
    shared.entries.store(Arc::new(index.entries.clone()));
    shared.generation.send_replace(generation);
    let inval = old.diff(&shared.tree.load());
    (old, inval)
}

async fn invalidate(shared: &Shared, old: Arc<Tree>, inval: Vec<Invalidation>) {
    let Some(notifier) = shared.notifier.lock().unwrap().clone() else { return };
    if inval.is_empty() {
        return;
    }
    let new = shared.tree.load_full();
    // Writes to /dev/fuse (or calls into WinFsp); keep it off the async workers.
    let _ = tokio::task::spawn_blocking(move || notifier.tree_changed(&old, &new, &inval)).await;
}
