//! The folder tree rebuilt from the flat list of entries.
//!
//! The tree is immutable and rebuilt from scratch on every change: a few hundred
//! entries take a few milliseconds, and a full rebuild can't drift.
//!
//! ```text
//! /                     the tablet's folders; each document once:
//!   X.pdf                 a PDF (original file)
//!   X.annotated.pdf       … with the tablet's strokes drawn on (if it has any)
//!   X.highlights.md       … its highlights as Markdown (PDF/EPUB with annotations)
//!   Y.epub                an EPUB (original file)
//!   Z.pdf                 a notebook, rendered
//! /.trash  /.lost+found the same, for trashed and orphaned items
//! /.originals           mirror of the folders with only the original PDFs/EPUBs
//! /.rmdoc               mirror of the folders with every document as X.rmdoc
//! /.recent  /.pinned    recently opened (mtime = last opened) and favourite documents
//! /.tags/<tag>          documents by tag
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use uuid::Uuid;

use super::entry::{Entry, FileKind};
use super::ino;
use super::naming;
use super::parse::{ContentKind, FileType, ItemType, Metadata, ParentRef};

pub type NodeId = usize;

const ROOT_ID: NodeId = 0;
const TRASH_ID: NodeId = 1;
const LOST_FOUND_ID: NodeId = 2;

pub const TRASH_NAME: &str = ".trash";
pub const LOST_FOUND_NAME: &str = ".lost+found";
pub const ORIGINALS_NAME: &str = ".originals";
pub const RMDOC_NAME: &str = ".rmdoc";
pub const RECENT_NAME: &str = ".recent";
pub const PINNED_NAME: &str = ".pinned";
pub const TAGS_NAME: &str = ".tags";
/// How many documents .recent lists.
const RECENT_MAX: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Root,
    Trash,
    LostFound,
    /// A directory of a view (.originals, .rmdoc, .recent, .pinned, .tags, a tag).
    ViewDir,
    Folder,
    Pdf,
    Epub,
    Notebook,
}

/// Which file a document node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// Directories.
    Dir,
    /// The PDF/EPUB as imported.
    Original,
    /// A PDF with its strokes drawn on.
    Annotated,
    /// A notebook rendered to PDF.
    Rendered,
    /// The document's files as a reMarkable .rmdoc archive.
    Rmdoc,
    /// Highlights as Markdown.
    Highlights,
}

impl Variant {
    /// Made by re-fs on demand (rendered, archived), rather than a tablet file.
    pub fn generated(self) -> bool {
        matches!(self, Variant::Annotated | Variant::Rendered | Variant::Rmdoc | Variant::Highlights)
    }
}

/// The part of the tree a node belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    /// The tablet's folders (including .trash and .lost+found).
    Main,
    Originals,
    Rmdoc,
    Recent,
    Pinned,
    Tag,
}

impl NodeKind {
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Root | Self::Trash | Self::LostFound | Self::ViewDir | Self::Folder)
    }

    /// The document's own extension (the original's, or ".pdf" for notebooks).
    pub fn ext(self) -> &'static str {
        match self {
            Self::Pdf | Self::Notebook => ".pdf",
            Self::Epub => ".epub",
            _ => "",
        }
    }

    /// The tablet file of the original, if any.
    pub fn original_file(self) -> Option<FileKind> {
        match self {
            Self::Pdf => Some(FileKind::Pdf),
            Self::Epub => Some(FileKind::Epub),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// `None` for fixed and view directories.
    pub uuid: Option<Uuid>,
    pub ino: u64,
    pub name: String,
    pub kind: NodeKind,
    pub variant: Variant,
    pub view: View,
    pub parent: NodeId,
    /// Size of the file as stored on the tablet; 0 for generated variants (their
    /// size is known once made).
    pub size: u64,
    /// The item's `lastModified` (in .recent: `lastOpened`), in ms.
    pub mtime_ms: i64,
    /// `createdTime`, falling back to `lastModified`.
    pub ctime_ms: i64,
    /// mtime of the original file on the tablet; the cache key for originals.
    pub data_mtime_ms: i64,
    /// Changes whenever anything a generated variant is made from changes.
    pub version: u64,
    pub doc: Option<DocInfo>,
}

impl Node {
    /// The variant writes act on (the document itself), as opposed to derived files.
    pub fn is_primary(&self) -> bool {
        self.view == View::Main && matches!(self.variant, Variant::Dir | Variant::Original | Variant::Rendered)
    }

    /// The tablet file served for reads, for originals.
    pub fn data_file(&self) -> Option<FileKind> {
        (self.variant == Variant::Original).then(|| self.kind.original_file()).flatten()
    }
}

/// Document details shown as extended attributes and used by views.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocInfo {
    pub pages: Option<u32>,
    /// 1-based.
    pub last_page: Option<u32>,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub last_opened_ms: Option<i64>,
    /// Has any stroke files (annotations, or a notebook's writing).
    pub has_strokes: bool,
}

#[derive(Debug, Clone)]
pub struct TreeOptions {
    /// List notebooks (rendered to PDF).
    pub notebooks: bool,
    /// Add .originals, .rmdoc, .recent, .pinned and .tags.
    pub views: bool,
}

impl Default for TreeOptions {
    fn default() -> Self {
        TreeOptions { notebooks: true, views: true }
    }
}

/// Something the kernel may have cached that a tree change made stale.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Invalidation {
    /// Attributes or contents of an inode (for a directory: its listing).
    Inode(u64),
    /// A name in a directory, positive or negative dentry.
    Entry { parent: u64, name: String },
}

#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<Node>,
    children: Vec<BTreeMap<String, NodeId>>,
    by_ino: HashMap<u64, NodeId>,
    by_uuid: HashMap<Uuid, NodeId>,
    generation: u64,
    warnings: Vec<String>,
}

struct Item {
    uuid: Uuid,
    meta: Metadata,
    kind: NodeKind,
    size: u64,
    data_mtime_ms: i64,
    version: u64,
    info: DocInfo,
}

/// Where an item hangs: a folder (item index) or a fixed directory.
type Place = Result<usize, NodeId>;

struct Builder {
    tree: Tree,
    taken_inos: HashSet<u64>,
}

impl Builder {
    fn ino_for(&mut self, uuid: Option<&Uuid>, salt: u64) -> u64 {
        let base = match uuid {
            Some(u) => ino::preferred(u) ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15),
            None => salt.wrapping_mul(0x9e37_79b9_7f4a_7c15),
        };
        let mut ino = (base & (i64::MAX as u64)).max(ino::FIRST_DYNAMIC);
        while !self.taken_inos.insert(ino) {
            ino = if ino >= i64::MAX as u64 { ino::FIRST_DYNAMIC } else { ino + 1 };
        }
        ino
    }

    fn push(&mut self, node: Node) -> NodeId {
        let id = self.tree.nodes.len();
        self.tree.by_ino.insert(node.ino, id);
        self.tree.nodes.push(node);
        self.tree.children.push(BTreeMap::new());
        id
    }

    /// Add `node` under `parent`, numbering its name if it clashes.
    fn attach(&mut self, mut node: Node, parent: NodeId, ext: &str) -> NodeId {
        node.parent = parent;
        let first = node.name.clone();
        let mut name = first.clone();
        let mut n = 2;
        while self.tree.children[parent].contains_key(&name) {
            name = naming::numbered(&first, ext, n);
            n += 1;
        }
        node.name = name.clone();
        let id = self.push(node);
        self.tree.children[parent].insert(name, id);
        id
    }

    fn fixed_dir(&mut self, ino: u64, name: &str, kind: NodeKind, view: View, parent: NodeId) -> NodeId {
        self.taken_inos.insert(ino);
        let node = Node {
            uuid: None,
            ino,
            name: name.into(),
            kind,
            variant: Variant::Dir,
            view,
            parent,
            size: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            data_mtime_ms: 0,
            version: 0,
            doc: None,
        };
        let id = self.push(node);
        if id != parent {
            self.tree.children[parent].insert(name.into(), id);
        }
        id
    }
}

fn salt(view: View, variant: Variant) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (view, variant).hash(&mut h);
    // Main-view primaries keep the plain UUID-derived inode (stable since M1).
    if view == View::Main && matches!(variant, Variant::Dir | Variant::Original | Variant::Rendered) {
        0
    } else {
        h.finish() | 1
    }
}

impl Tree {
    pub fn build(entries: &[Entry], generation: u64, opts: &TreeOptions) -> Self {
        let mut warnings = Vec::new();
        let items = parse_items(entries, opts, &mut warnings);

        let folder_ix: HashMap<Uuid, usize> =
            items.iter().enumerate().filter(|(_, it)| it.kind == NodeKind::Folder).map(|(i, it)| (it.uuid, i)).collect();
        let mut place: Vec<Place> = items
            .iter()
            .map(|it| match &it.meta.parent {
                ParentRef::Root => Err(ROOT_ID),
                ParentRef::Trash => Err(TRASH_ID),
                ParentRef::Folder(p) => match folder_ix.get(p) {
                    Some(&i) => Ok(i),
                    None => {
                        warnings.push(format!("{}: parent {p} is not a known folder", it.uuid));
                        Err(LOST_FOUND_ID)
                    }
                },
                ParentRef::Invalid(s) => {
                    warnings.push(format!("{}: invalid parent {s:?}", it.uuid));
                    Err(LOST_FOUND_ID)
                }
            })
            .collect();
        break_cycles(&items, &mut place, &mut warnings);

        let mut b = Builder {
            tree: Tree {
                nodes: Vec::with_capacity(items.len() * 4),
                children: Vec::new(),
                by_ino: HashMap::new(),
                by_uuid: HashMap::new(),
                generation,
                warnings: Vec::new(),
            },
            taken_inos: (0..ino::FIRST_DYNAMIC).collect(),
        };
        b.fixed_dir(ino::ROOT, "", NodeKind::Root, View::Main, ROOT_ID);
        b.fixed_dir(ino::TRASH, TRASH_NAME, NodeKind::Trash, View::Main, ROOT_ID);
        // lost+found is detached until something lands in it.
        let lf = b.fixed_dir(ino::LOST_FOUND, LOST_FOUND_NAME, NodeKind::LostFound, View::Main, ROOT_ID);
        b.tree.children[ROOT_ID].remove(LOST_FOUND_NAME);
        debug_assert_eq!(lf, LOST_FOUND_ID);

        // Oldest first, then UUID: the item that keeps the plain name on a clash
        // doesn't change between rebuilds.
        let mut order: Vec<usize> = (0..items.len()).collect();
        let ctime = |it: &Item| it.meta.created_time.or(it.meta.last_modified).unwrap_or(0);
        order.sort_by_key(|&i| (ctime(&items[i]), items[i].uuid));

        // ---- main view (folders first, so documents can hang below them) ----
        let main_folders = build_folders(&mut b, &items, &place, &order, View::Main, &[ROOT_ID, TRASH_ID, LOST_FOUND_ID]);
        for &i in &order {
            if items[i].kind == NodeKind::Folder {
                continue;
            }
            let parent = resolve(&place[i], &main_folders, |fixed| fixed);
            let it = &items[i];
            let base = naming::strip_ext(&it.meta.visible_name, it.kind.ext()).unwrap_or(&it.meta.visible_name).to_owned();
            let primary = match it.kind {
                NodeKind::Notebook => Variant::Rendered,
                _ => Variant::Original,
            };
            let id = add_doc(&mut b, it, &base, primary, View::Main, parent);
            b.tree.by_uuid.insert(it.uuid, id);
            if it.info.has_strokes && it.kind == NodeKind::Pdf {
                add_doc(&mut b, it, &base, Variant::Annotated, View::Main, parent);
            }
            if it.info.has_strokes && matches!(it.kind, NodeKind::Pdf | NodeKind::Epub) {
                add_doc(&mut b, it, &base, Variant::Highlights, View::Main, parent);
            }
        }
        if !b.tree.children[LOST_FOUND_ID].is_empty() {
            b.tree.children[ROOT_ID].insert(LOST_FOUND_NAME.into(), LOST_FOUND_ID);
        }

        if opts.views {
            let live = |i: usize| -> bool {
                // Items not in the trash (directly or via a trashed folder).
                let mut p = place[i];
                loop {
                    match p {
                        Ok(f) => p = place[f],
                        Err(fixed) => return fixed == ROOT_ID,
                    }
                }
            };
            // .originals and .rmdoc: the folder structure again, one file kind each.
            for (name, ino_n, view) in [(ORIGINALS_NAME, 4, View::Originals), (RMDOC_NAME, 5, View::Rmdoc)] {
                let top = b.fixed_dir(ino_n, name, NodeKind::ViewDir, view, ROOT_ID);
                let folders = build_folders(&mut b, &items, &place, &order, view, &[top]);
                for &i in &order {
                    let it = &items[i];
                    if it.kind == NodeKind::Folder || !live(i) {
                        continue;
                    }
                    let parent = resolve(&place[i], &folders, |_| top);
                    let base = naming::strip_ext(&it.meta.visible_name, it.kind.ext()).unwrap_or(&it.meta.visible_name).to_owned();
                    match view {
                        View::Originals if it.kind.original_file().is_some() => {
                            add_doc(&mut b, it, &base, Variant::Original, view, parent);
                        }
                        View::Rmdoc => {
                            add_doc(&mut b, it, &base, Variant::Rmdoc, view, parent);
                        }
                        _ => {}
                    }
                }
            }
            // .recent: most recently opened first (by mtime; names keep their own order).
            let recent = b.fixed_dir(6, RECENT_NAME, NodeKind::ViewDir, View::Recent, ROOT_ID);
            let mut opened: Vec<usize> =
                order.iter().copied().filter(|&i| items[i].kind != NodeKind::Folder && live(i) && items[i].info.last_opened_ms.unwrap_or(0) > 0).collect();
            opened.sort_by_key(|&i| std::cmp::Reverse(items[i].info.last_opened_ms));
            for &i in opened.iter().take(RECENT_MAX) {
                let it = &items[i];
                let base = naming::strip_ext(&it.meta.visible_name, it.kind.ext()).unwrap_or(&it.meta.visible_name).to_owned();
                let id = add_doc(&mut b, it, &base, if it.kind == NodeKind::Notebook { Variant::Rendered } else { Variant::Original }, View::Recent, recent);
                b.tree.nodes[id].mtime_ms = it.info.last_opened_ms.unwrap_or(0);
            }
            // .pinned and .tags/<tag>.
            let pinned = b.fixed_dir(7, PINNED_NAME, NodeKind::ViewDir, View::Pinned, ROOT_ID);
            let tags_dir = b.fixed_dir(8, TAGS_NAME, NodeKind::ViewDir, View::Tag, ROOT_ID);
            let mut tag_dirs: BTreeMap<String, NodeId> = BTreeMap::new();
            for &i in &order {
                let it = &items[i];
                if it.kind == NodeKind::Folder || !live(i) {
                    continue;
                }
                let base = naming::strip_ext(&it.meta.visible_name, it.kind.ext()).unwrap_or(&it.meta.visible_name).to_owned();
                let primary = if it.kind == NodeKind::Notebook { Variant::Rendered } else { Variant::Original };
                if it.info.pinned {
                    add_doc(&mut b, it, &base, primary, View::Pinned, pinned);
                }
                for tag in &it.info.tags {
                    let dir = match tag_dirs.get(tag) {
                        Some(&d) => d,
                        None => {
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            tag.hash(&mut h);
                            let ino = b.ino_for(None, h.finish());
                            b.taken_inos.remove(&ino);
                            let d = b.fixed_dir(ino, &naming::file_name(tag, ""), NodeKind::ViewDir, View::Tag, tags_dir);
                            tag_dirs.insert(tag.clone(), d);
                            d
                        }
                    };
                    add_doc(&mut b, it, &base, primary, View::Tag, dir);
                }
            }
        }

        let mut tree = b.tree;
        tree.warnings = warnings;
        tree
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Problems found while building (bad JSON, missing parents, cycles, ...).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn root(&self) -> &Node {
        &self.nodes[ROOT_ID]
    }

    pub fn node(&self, ino: u64) -> Option<&Node> {
        self.by_ino.get(&ino).map(|&id| &self.nodes[id])
    }

    /// The document or folder itself (main view).
    pub fn by_uuid(&self, uuid: &Uuid) -> Option<&Node> {
        self.by_uuid.get(uuid).map(|&id| &self.nodes[id])
    }

    pub fn parent(&self, node: &Node) -> &Node {
        &self.nodes[node.parent]
    }

    pub fn lookup(&self, parent_ino: u64, name: &str) -> Option<&Node> {
        let pid = *self.by_ino.get(&parent_ino)?;
        self.children[pid].get(name).map(|&id| &self.nodes[id])
    }

    /// Children of a directory in name order; `None` if `ino` isn't a directory.
    pub fn children(&self, ino: u64) -> Option<impl Iterator<Item = &Node> + '_> {
        let pid = *self.by_ino.get(&ino)?;
        self.nodes[pid].kind.is_dir().then(|| self.children[pid].values().map(|&id| &self.nodes[id]))
    }

    /// Resolve a `/`-separated path from the root. "" and "/" are the root.
    pub fn resolve(&self, path: &str) -> Option<&Node> {
        path.split('/').filter(|c| !c.is_empty()).try_fold(self.root(), |dir, name| self.lookup(dir.ino, name))
    }

    /// The path of a node from the mount root, e.g. "Books/Tech/x.pdf".
    pub fn path(&self, node: &Node) -> String {
        let mut parts = Vec::new();
        let mut id = self.by_ino[&node.ino];
        while id != ROOT_ID {
            parts.push(self.nodes[id].name.as_str());
            id = self.nodes[id].parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// Every node except the root, in no particular order.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().skip(1)
    }

    /// What to invalidate in the kernel when replacing `self` with `newer`.
    pub fn diff(&self, newer: &Tree) -> Vec<Invalidation> {
        use Invalidation::{Entry, Inode};
        let mut out = Vec::new();
        for o in self.nodes() {
            let o_parent = self.parent(o).ino;
            match newer.node(o.ino) {
                None => {
                    out.push(Entry { parent: o_parent, name: o.name.clone() });
                    out.push(Inode(o_parent));
                }
                Some(n) => {
                    let n_parent = newer.parent(n).ino;
                    if n.name != o.name || n_parent != o_parent {
                        out.push(Entry { parent: o_parent, name: o.name.clone() });
                        out.push(Entry { parent: n_parent, name: n.name.clone() });
                        out.push(Inode(o_parent));
                        out.push(Inode(n_parent));
                    }
                    if (n.kind, n.size, n.mtime_ms, n.ctime_ms, n.data_mtime_ms, n.version) != (o.kind, o.size, o.mtime_ms, o.ctime_ms, o.data_mtime_ms, o.version) {
                        out.push(Inode(n.ino));
                    }
                }
            }
        }
        for n in newer.nodes() {
            if self.node(n.ino).is_none() {
                let parent = newer.parent(n).ino;
                // The name may be cached as "doesn't exist".
                out.push(Entry { parent, name: n.name.clone() });
                out.push(Inode(parent));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn resolve(place: &Place, folders: &HashMap<usize, NodeId>, fixed: impl Fn(NodeId) -> NodeId) -> NodeId {
    match place {
        Ok(f) => folders[f],
        Err(id) => fixed(*id),
    }
}

/// Create folder nodes for `view`, parents before children. `roots[0]` stands in
/// for the tablet's top level; in the main view `roots` also holds trash and
/// lost+found. Folders under the trash/lost+found only appear in the main view.
fn build_folders(b: &mut Builder, items: &[Item], place: &[Place], order: &[usize], view: View, roots: &[NodeId]) -> HashMap<usize, NodeId> {
    let mut made: HashMap<usize, NodeId> = HashMap::new();
    // Resolve each folder's chain up to a fixed directory.
    fn ensure(i: usize, b: &mut Builder, items: &[Item], place: &[Place], view: View, roots: &[NodeId], made: &mut HashMap<usize, NodeId>) -> Option<NodeId> {
        if let Some(&id) = made.get(&i) {
            return Some(id);
        }
        let parent = match place[i] {
            Ok(f) => ensure(f, b, items, place, view, roots, made)?,
            Err(ROOT_ID) => roots[0],
            Err(fixed) if view == View::Main => fixed,
            Err(_) => return None,
        };
        let it = &items[i];
        let node = Node {
            uuid: Some(it.uuid),
            ino: b.ino_for(Some(&it.uuid), salt(view, Variant::Dir)),
            name: naming::file_name(&it.meta.visible_name, ""),
            kind: NodeKind::Folder,
            variant: Variant::Dir,
            view,
            parent,
            size: 0,
            mtime_ms: it.meta.last_modified.unwrap_or(0),
            ctime_ms: it.meta.created_time.or(it.meta.last_modified).unwrap_or(0),
            data_mtime_ms: 0,
            version: 0,
            doc: None,
        };
        let id = b.attach(node, parent, "");
        if view == View::Main {
            b.tree.by_uuid.insert(it.uuid, id);
        }
        made.insert(i, id);
        Some(id)
    }
    // Reserved names at the top level.
    if view == View::Main {
        for name in [LOST_FOUND_NAME, ORIGINALS_NAME, RMDOC_NAME, RECENT_NAME, PINNED_NAME, TAGS_NAME] {
            b.tree.children[ROOT_ID].entry(name.into()).or_insert(usize::MAX);
        }
    }
    for &i in order {
        if items[i].kind == NodeKind::Folder {
            ensure(i, b, items, place, view, roots, &mut made);
        }
    }
    if view == View::Main {
        b.tree.children[ROOT_ID].retain(|_, id| *id != usize::MAX);
    }
    made
}

/// Add one file for a document.
fn add_doc(b: &mut Builder, it: &Item, base: &str, variant: Variant, view: View, parent: NodeId) -> NodeId {
    let ext = match variant {
        Variant::Annotated => ".annotated.pdf",
        Variant::Rmdoc => ".rmdoc",
        Variant::Highlights => ".highlights.md",
        _ => it.kind.ext(),
    };
    let mtime_ms = it.meta.last_modified.unwrap_or(0);
    let node = Node {
        uuid: Some(it.uuid),
        ino: b.ino_for(Some(&it.uuid), salt(view, variant)),
        name: naming::file_name(base, ext),
        kind: it.kind,
        variant,
        view,
        parent,
        size: if variant == Variant::Original { it.size } else { 0 },
        mtime_ms,
        ctime_ms: it.meta.created_time.unwrap_or(mtime_ms),
        data_mtime_ms: it.data_mtime_ms,
        version: it.version,
        doc: Some(it.info.clone()),
    };
    b.attach(node, parent, ext)
}

/// A fingerprint of everything generated variants are made from.
fn version_of(e: &Entry) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &e.files {
        (f.kind as u8, f.size, f.mtime_ms).hash(&mut h);
    }
    for p in &e.pages {
        (&p.name, p.size, p.mtime_ms).hash(&mut h);
    }
    h.finish()
}

fn parse_items(entries: &[Entry], opts: &TreeOptions, warnings: &mut Vec<String>) -> Vec<Item> {
    let mut items = Vec::with_capacity(entries.len());
    for e in entries {
        let meta = match Metadata::from_slice(&e.metadata) {
            Ok(m) => m,
            Err(err) => {
                warnings.push(format!("{}: bad .metadata: {err}", e.uuid));
                continue;
            }
        };
        if meta.deleted {
            continue;
        }
        let content = match e.content.as_deref().map(ContentKind::from_slice) {
            Some(Ok(c)) => Some(c),
            Some(Err(err)) => {
                warnings.push(format!("{}: bad .content: {err}", e.uuid));
                None
            }
            None => None,
        };
        let kind = match meta.item_type {
            ItemType::Folder => NodeKind::Folder,
            ItemType::Document => match content.as_ref().and_then(|c| c.file_type) {
                Some(FileType::Pdf) => NodeKind::Pdf,
                Some(FileType::Epub) => NodeKind::Epub,
                Some(FileType::Notebook) | None => NodeKind::Notebook,
                Some(FileType::Other) => {
                    warnings.push(format!("{}: unknown fileType, skipped", e.uuid));
                    continue;
                }
            },
            ItemType::Unknown => {
                warnings.push(format!("{}: unknown item type, skipped", e.uuid));
                continue;
            }
        };
        if kind == NodeKind::Notebook && !opts.notebooks {
            continue;
        }
        let stat = kind.original_file().and_then(|k| e.stat(k));
        if kind.original_file().is_some() && stat.is_none() {
            warnings.push(format!("{}: {:?} document has no data file", e.uuid, kind));
        }
        let info = DocInfo {
            pages: content.as_ref().and_then(|c| c.page_count).and_then(|n| u32::try_from(n).ok()),
            last_page: meta.last_opened_page.and_then(|p| u32::try_from(p).ok()).map(|p| p + 1),
            tags: content.as_ref().map(|c| c.tags.iter().map(|t| t.0.clone()).collect()).unwrap_or_default(),
            pinned: meta.pinned,
            last_opened_ms: meta.last_opened.filter(|&t| t > 0),
            has_strokes: e.pages.iter().any(|p| p.name.ends_with(".rm") && p.size > 0),
        };
        items.push(Item {
            uuid: e.uuid,
            size: stat.map_or(0, |s| s.size),
            data_mtime_ms: stat.map_or(meta.last_modified.unwrap_or(0), |s| s.mtime_ms),
            version: version_of(e),
            meta,
            kind,
            info,
        });
    }
    items
}

/// A folder that is (indirectly) its own parent would never be reachable from the
/// root. Re-hang the first folder found on each cycle under lost+found.
fn break_cycles(items: &[Item], place: &mut [Place], warnings: &mut Vec<String>) {
    #[derive(Clone, Copy, PartialEq)]
    enum S {
        New,
        Walking,
        Done,
    }
    let mut state = vec![S::New; items.len()];
    for start in 0..items.len() {
        let mut path = Vec::new();
        let mut cur = start;
        loop {
            match state[cur] {
                S::Done => break,
                S::Walking => {
                    warnings.push(format!("{}: folder cycle, moved to {LOST_FOUND_NAME}", items[cur].uuid));
                    place[cur] = Err(LOST_FOUND_ID);
                    break;
                }
                S::New => {
                    state[cur] = S::Walking;
                    path.push(cur);
                    match place[cur] {
                        Ok(p) => cur = p,
                        Err(_) => break,
                    }
                }
            }
        }
        for i in path {
            state[i] = S::Done;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{FileStat, PageFile};

    fn u(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    /// The tree as it was before views: no notebooks, no .originals etc.
    fn opts() -> TreeOptions {
        TreeOptions { notebooks: false, views: false }
    }

    fn folder(id: u8, name: &str, parent: &str) -> Entry {
        Entry {
            uuid: u(id),
            metadata: format!(r#"{{"type":"CollectionType","visibleName":"{name}","parent":"{parent}","lastModified":"{id}"}}"#).into_bytes(),
            content: None,
            files: vec![],
            pages: vec![],
        }
    }

    fn doc(id: u8, name: &str, parent: &str, file_type: &str, size: u64) -> Entry {
        let kind = match file_type {
            "pdf" => Some(FileKind::Pdf),
            "epub" => Some(FileKind::Epub),
            _ => None,
        };
        Entry {
            uuid: u(id),
            metadata: format!(r#"{{"type":"DocumentType","visibleName":"{name}","parent":"{parent}","lastModified":"1000","createdTime":"{id}"}}"#).into_bytes(),
            content: Some(format!(r#"{{"fileType":"{file_type}"}}"#).into_bytes()),
            files: kind.map(|kind| FileStat { kind, size, mtime_ms: 2000 }).into_iter().collect(),
            pages: vec![],
        }
    }

    fn with_strokes(mut e: Entry) -> Entry {
        e.pages = vec![PageFile { name: "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f.rm".into(), size: 100, mtime_ms: 5 }];
        e
    }

    fn with_meta(mut e: Entry, extra: &str) -> Entry {
        let s = String::from_utf8(e.metadata).unwrap();
        e.metadata = s.replacen('{', &format!("{{{extra},"), 1).into_bytes();
        e
    }

    fn paths(t: &Tree) -> Vec<String> {
        let mut v: Vec<String> = t.nodes().filter(|n| t.resolve(&t.path(n)).is_some()).map(|n| t.path(n)).collect();
        v.sort();
        v
    }

    #[test]
    fn builds_nested_tree() {
        let books = u(1).to_string();
        let tech = u(2).to_string();
        let t = Tree::build(
            &[
                folder(1, "Books", ""),
                folder(2, "Tech", &books),
                doc(3, "Rust", &tech, "pdf", 42),
                doc(4, "Novel", &books, "epub", 7),
                doc(5, "Loose", "", "pdf", 1),
            ],
            9,
            &opts(),
        );
        assert_eq!(paths(&t), [".trash", "Books", "Books/Novel.epub", "Books/Tech", "Books/Tech/Rust.pdf", "Loose.pdf"]);
        let rust = t.resolve("/Books/Tech/Rust.pdf").unwrap();
        assert_eq!((rust.kind, rust.size, rust.data_mtime_ms), (NodeKind::Pdf, 42, 2000));
        assert_eq!(t.parent(rust).name, "Tech");
        assert_eq!(t.generation(), 9);
        assert!(t.warnings().is_empty());
    }

    #[test]
    fn notebooks_hidden_unless_enabled() {
        let entries = [doc(1, "Notes", "", "notebook", 0), doc(2, "Old", "", "", 0)];
        assert_eq!(paths(&Tree::build(&entries, 0, &opts())), [".trash"]);
        let t = Tree::build(&entries, 0, &TreeOptions { notebooks: true, views: false });
        assert_eq!(paths(&t), [".trash", "Notes.pdf", "Old.pdf"]);
    }

    #[test]
    fn trash_deleted_and_lost_found() {
        let t = Tree::build(
            &[
                doc(1, "Binned", "trash", "pdf", 1),
                doc(2, "Orphan", &u(99).to_string(), "pdf", 1),
                doc(3, "Bad parent", "not-a-uuid", "pdf", 1),
                Entry {
                    metadata: br#"{"type":"DocumentType","visibleName":"Gone","parent":"","deleted":true}"#.to_vec(),
                    ..doc(4, "", "", "pdf", 1)
                },
            ],
            0,
            &opts(),
        );
        assert_eq!(
            paths(&t),
            [".lost+found", ".lost+found/Bad parent.pdf", ".lost+found/Orphan.pdf", ".trash", ".trash/Binned.pdf"]
        );
        assert_eq!(t.warnings().len(), 2);
        assert_eq!(t.node(ino::TRASH).unwrap().name, ".trash");
    }

    #[test]
    fn lost_found_only_listed_when_used() {
        let t = Tree::build(&[doc(1, "A", "", "pdf", 1)], 0, &opts());
        assert!(t.resolve(".lost+found").is_none());
    }

    #[test]
    fn clashes_numbered_oldest_first() {
        // createdTime = id, so 3 is older than 7 and keeps the plain name.
        let t = Tree::build(
            &[doc(7, "Wiki", "trash", "pdf", 1), doc(3, "Wiki", "trash", "pdf", 1), doc(5, "Wiki", "trash", "pdf", 1)],
            0,
            &opts(),
        );
        assert_eq!(t.by_uuid(&u(3)).unwrap().name, "Wiki.pdf");
        assert_eq!(t.by_uuid(&u(5)).unwrap().name, "Wiki (2).pdf");
        assert_eq!(t.by_uuid(&u(7)).unwrap().name, "Wiki (3).pdf");
    }

    #[test]
    fn pdf_and_folder_with_same_base_do_not_clash() {
        let t = Tree::build(&[folder(1, "Notes", ""), doc(2, "Notes", "", "pdf", 1)], 0, &opts());
        assert!(t.resolve("Notes").unwrap().kind.is_dir());
        assert_eq!(t.resolve("Notes.pdf").unwrap().kind, NodeKind::Pdf);
    }

    #[test]
    fn reserved_names_at_root() {
        let t = Tree::build(&[folder(1, ".trash", ""), folder(2, ".lost+found", "")], 0, &opts());
        assert_eq!(t.by_uuid(&u(1)).unwrap().name, ".trash (2)");
        assert_eq!(t.by_uuid(&u(2)).unwrap().name, ".lost+found (2)");
        assert_eq!(t.resolve(".trash").unwrap().kind, NodeKind::Trash);
    }

    #[test]
    fn slash_names() {
        let t = Tree::build(&[doc(1, "01/02/24", "", "pdf", 1)], 0, &opts());
        assert!(t.resolve("01∕02∕24.pdf").is_some());
    }

    #[test]
    fn folder_cycle_goes_to_lost_found() {
        let (a, b) = (u(1).to_string(), u(2).to_string());
        let t = Tree::build(&[folder(1, "A", &b), folder(2, "B", &a), doc(3, "x", &a, "pdf", 1)], 0, &opts());
        let p = paths(&t);
        assert!(p.iter().any(|p| p.starts_with(".lost+found/")), "{p:?}");
        assert!(p.iter().any(|p| p.ends_with("/x.pdf")), "{p:?}");
        assert_eq!(t.warnings().len(), 1);
    }

    #[test]
    fn inodes_stable_across_builds() {
        let e = [folder(1, "A", ""), doc(2, "x", "", "pdf", 1)];
        let (t1, t2) = (Tree::build(&e, 0, &opts()), Tree::build(&e, 1, &opts()));
        for n in t1.nodes() {
            assert_eq!(t2.node(n.ino).map(|m| &m.name), Some(&n.name));
        }
    }

    #[test]
    fn diff_reports_moves_adds_removes_and_changes() {
        use Invalidation::{Entry as E, Inode as I};
        let opts = TreeOptions::default();
        let a = u(1).to_string();
        let old = Tree::build(&[folder(1, "A", ""), doc(2, "x", "", "pdf", 1), doc(3, "gone", "", "pdf", 1), doc(4, "same", "", "pdf", 1)], 0, &opts);
        let new = Tree::build(&[folder(1, "A", ""), doc(2, "x", &a, "pdf", 1), doc(4, "same", "", "pdf", 9), doc(5, "new", "", "pdf", 1)], 1, &opts);
        let ino = |t: &Tree, id: u8| t.by_uuid(&u(id)).unwrap().ino;
        let (root, dir_a) = (ino::ROOT, ino(&old, 1));
        let d = old.diff(&new);
        let expect = |i: &Invalidation| assert!(d.contains(i), "missing {i:?} in {d:?}");
        expect(&E { parent: root, name: "x.pdf".into() });
        expect(&E { parent: dir_a, name: "x.pdf".into() });
        expect(&I(dir_a));
        expect(&E { parent: root, name: "gone.pdf".into() });
        expect(&I(ino(&old, 4)));
        expect(&E { parent: root, name: "new.pdf".into() });
        expect(&I(root));
        assert!(!d.contains(&I(ino(&old, 2))), "a move alone doesn't change x's attributes");
        assert!(old.diff(&old).is_empty());
    }

    #[test]
    fn bad_json_is_a_warning_not_a_failure() {
        let mut bad = doc(1, "x", "", "pdf", 1);
        bad.metadata = b"{not json".to_vec();
        let t = Tree::build(&[bad, doc(2, "ok", "", "pdf", 1)], 0, &opts());
        assert_eq!(paths(&t), [".trash", "ok.pdf"]);
        assert_eq!(t.warnings().len(), 1);
    }

    #[test]
    fn annotated_pdfs_get_extra_files() {
        let t = Tree::build(
            &[with_strokes(doc(1, "Paper", "", "pdf", 9)), doc(2, "Plain", "", "pdf", 9), with_strokes(doc(3, "Book", "", "epub", 9)), with_strokes(doc(4, "Notes", "", "notebook", 0))],
            0,
            &TreeOptions { notebooks: true, views: false },
        );
        assert_eq!(
            paths(&t),
            [".trash", "Book.epub", "Book.highlights.md", "Notes.pdf", "Paper.annotated.pdf", "Paper.highlights.md", "Paper.pdf", "Plain.pdf"]
        );
        assert_eq!(t.resolve("Paper.annotated.pdf").unwrap().variant, Variant::Annotated);
        assert_eq!(t.resolve("Notes.pdf").unwrap().variant, Variant::Rendered);
        assert_eq!(t.resolve("Paper.pdf").unwrap().size, 9);
        assert_eq!(t.resolve("Paper.annotated.pdf").unwrap().size, 0, "generated: size unknown until made");
        assert!(t.resolve("Paper.pdf").unwrap().is_primary());
        assert!(!t.resolve("Paper.annotated.pdf").unwrap().is_primary());
        // The document's own inode is unchanged by views.
        assert_eq!(t.resolve("Paper.pdf").unwrap().ino, ino::preferred(&u(1)));
    }

    #[test]
    fn mirrors_and_virtual_folders() {
        let books = u(1).to_string();
        let t = Tree::build(
            &[
                folder(1, "Books", ""),
                with_meta(doc(2, "Old", &books, "pdf", 1), r#""lastOpened":"100""#),
                with_meta(doc(3, "New", &books, "epub", 1), r#""lastOpened":"300","pinned":true"#),
                with_meta(doc(4, "Notes", "", "notebook", 0), r#""lastOpened":"200""#),
                doc(5, "Binned", "trash", "pdf", 1),
            ],
            0,
            &TreeOptions::default(),
        );
        let p = paths(&t);
        for want in [
            ".originals/Books/Old.pdf",
            ".originals/Books/New.epub",
            ".rmdoc/Books/Old.rmdoc",
            ".rmdoc/Notes.rmdoc",
            ".recent/New.epub",
            ".recent/Notes.pdf",
            ".pinned/New.epub",
            ".trash/Binned.pdf",
        ] {
            assert!(p.iter().any(|x| x == want), "missing {want} in {p:?}");
        }
        assert!(!p.iter().any(|x| x.starts_with(".originals/") && x.contains("Notes")), "notebooks have no original");
        assert!(!p.iter().any(|x| x.contains("Binned") && !x.starts_with(".trash")), "trash stays out of views");
        // .recent: mtime is when it was last opened, so `ls -t` sorts it.
        assert_eq!(t.resolve(".recent/New.epub").unwrap().mtime_ms, 300);
        // Same document, different views: different inodes.
        let a = t.resolve("Books/Old.pdf").unwrap().ino;
        let b = t.resolve(".originals/Books/Old.pdf").unwrap().ino;
        assert_ne!(a, b);
        assert_eq!(t.by_uuid(&u(2)).unwrap().ino, a, "by_uuid finds the main-view document");
    }

    #[test]
    fn tags_folders() {
        let mut e = doc(1, "Paper", "", "pdf", 1);
        e.content = Some(br#"{"fileType":"pdf","tags":[{"name":"Work","timestamp":1},{"name":"a/b","timestamp":2}]}"#.to_vec());
        let t = Tree::build(&[e], 0, &TreeOptions::default());
        assert!(t.resolve(".tags/Work/Paper.pdf").is_some());
        assert!(t.resolve(".tags/a∕b/Paper.pdf").is_some());
        assert_eq!(t.resolve("Paper.pdf").unwrap().doc.as_ref().unwrap().tags, vec!["Work", "a/b"]);
    }

    #[test]
    fn view_names_reserved() {
        let t = Tree::build(&[folder(1, ".rmdoc", "")], 0, &TreeOptions::default());
        assert_eq!(t.by_uuid(&u(1)).unwrap().name, ".rmdoc (2)");
        assert_eq!(t.resolve(".rmdoc").unwrap().kind, NodeKind::ViewDir);
    }
}

