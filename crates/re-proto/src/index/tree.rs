//! The folder tree rebuilt from the flat list of entries.
//!
//! The tree is immutable and rebuilt from scratch on every change: a few hundred
//! entries take well under a millisecond, and a full rebuild can't drift.

use std::collections::{BTreeMap, HashMap, HashSet};

use uuid::Uuid;

use super::entry::{Entry, FileKind};
use super::ino;
use super::naming;
use super::parse::{Content, FileType, ItemType, Metadata, ParentRef};

pub type NodeId = usize;

const ROOT_ID: NodeId = 0;
const TRASH_ID: NodeId = 1;
const LOST_FOUND_ID: NodeId = 2;

pub const TRASH_NAME: &str = ".trash";
pub const LOST_FOUND_NAME: &str = ".lost+found";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Root,
    Trash,
    LostFound,
    Folder,
    Pdf,
    Epub,
    Notebook,
}

impl NodeKind {
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Root | Self::Trash | Self::LostFound | Self::Folder)
    }

    /// Extension added to the visible name. Notebooks will be rendered to PDF.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Pdf | Self::Notebook => ".pdf",
            Self::Epub => ".epub",
            _ => "",
        }
    }

    /// The tablet file a read of this node is served from, if any.
    pub fn data_file(self) -> Option<FileKind> {
        match self {
            Self::Pdf => Some(FileKind::Pdf),
            Self::Epub => Some(FileKind::Epub),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// `None` for the fixed directories (root, trash, lost+found).
    pub uuid: Option<Uuid>,
    pub ino: u64,
    pub name: String,
    pub kind: NodeKind,
    pub parent: NodeId,
    pub size: u64,
    /// The item's `lastModified`, in ms.
    pub mtime_ms: i64,
    /// `createdTime`, falling back to `lastModified`.
    pub ctime_ms: i64,
    /// mtime of the data file (`.pdf`/`.epub`) on the tablet; the cache key.
    pub data_mtime_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct TreeOptions {
    /// List notebooks (as `.pdf`). Off until rendering exists.
    pub notebooks: bool,
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
}

impl Tree {
    pub fn build(entries: &[Entry], generation: u64, opts: &TreeOptions) -> Self {
        let mut warnings = Vec::new();
        let items = parse_items(entries, opts, &mut warnings);

        // Folders by UUID, for parent resolution.
        let folder_ix: HashMap<Uuid, usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.kind == NodeKind::Folder)
            .map(|(i, it)| (it.uuid, i))
            .collect();

        // Where each item hangs: Ok(item index of its folder) or Err(fixed dir).
        let mut parent_of: Vec<Result<usize, NodeId>> = items
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
        break_cycles(&items, &mut parent_of, &mut warnings);

        let mut tree = Tree {
            nodes: Vec::with_capacity(items.len() + 3),
            children: Vec::with_capacity(items.len() + 3),
            by_ino: HashMap::new(),
            by_uuid: HashMap::new(),
            generation,
            warnings: Vec::new(),
        };
        for (ino, kind) in [(ino::ROOT, NodeKind::Root), (ino::TRASH, NodeKind::Trash), (ino::LOST_FOUND, NodeKind::LostFound)] {
            tree.push(Node {
                uuid: None,
                ino,
                name: String::new(),
                kind,
                parent: ROOT_ID,
                size: 0,
                mtime_ms: 0,
                ctime_ms: 0,
                data_mtime_ms: 0,
            });
        }
        tree.nodes[TRASH_ID].name = TRASH_NAME.into();
        tree.nodes[LOST_FOUND_ID].name = LOST_FOUND_NAME.into();

        // Node id of item i is i + 3.
        let node_of = |r: Result<usize, NodeId>| r.map_or_else(|fixed| fixed, |i| i + 3);
        let mut taken_inos: HashSet<u64> = (0..ino::FIRST_DYNAMIC).collect();
        for (i, it) in items.iter().enumerate() {
            let mut ino = ino::preferred(&it.uuid);
            while !taken_inos.insert(ino) {
                ino = if ino >= i64::MAX as u64 { ino::FIRST_DYNAMIC } else { ino + 1 };
            }
            let mtime_ms = it.meta.last_modified.unwrap_or(0);
            tree.push(Node {
                uuid: Some(it.uuid),
                ino,
                name: String::new(),
                kind: it.kind,
                parent: node_of(parent_of[i]),
                size: it.size,
                mtime_ms,
                ctime_ms: it.meta.created_time.unwrap_or(mtime_ms),
                data_mtime_ms: it.data_mtime_ms,
            });
        }

        // Names: siblings in a stable order (oldest first, then UUID), so the item
        // that keeps the plain name doesn't change between rebuilds.
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by_key(|&i| (tree.nodes[i + 3].ctime_ms, items[i].uuid));
        tree.children[ROOT_ID].insert(TRASH_NAME.into(), TRASH_ID);
        for i in order {
            let id = i + 3;
            let (parent, kind) = (tree.nodes[id].parent, tree.nodes[id].kind);
            let ext = kind.ext();
            let first = naming::file_name(&items[i].meta.visible_name, ext);
            let mut name = first.clone();
            let mut n = 2;
            while tree.children[parent].contains_key(&name) || (parent == ROOT_ID && name == LOST_FOUND_NAME) {
                name = naming::numbered(&first, ext, n);
                n += 1;
            }
            tree.children[parent].insert(name.clone(), id);
            tree.nodes[id].name = name;
        }
        if !tree.children[LOST_FOUND_ID].is_empty() {
            tree.children[ROOT_ID].insert(LOST_FOUND_NAME.into(), LOST_FOUND_ID);
        }
        tree.warnings = warnings;
        tree
    }

    fn push(&mut self, node: Node) {
        let id = self.nodes.len();
        self.by_ino.insert(node.ino, id);
        if let Some(u) = node.uuid {
            self.by_uuid.insert(u, id);
        }
        self.nodes.push(node);
        self.children.push(BTreeMap::new());
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
        path.split('/')
            .filter(|c| !c.is_empty())
            .try_fold(self.root(), |dir, name| self.lookup(dir.ino, name))
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
        let kind = match meta.item_type {
            ItemType::Folder => NodeKind::Folder,
            ItemType::Document => {
                let ft = match e.content.as_deref().map(Content::from_slice) {
                    Some(Ok(c)) => c.file_type,
                    Some(Err(err)) => {
                        warnings.push(format!("{}: bad .content: {err}", e.uuid));
                        None
                    }
                    None => None,
                };
                match ft {
                    Some(FileType::Pdf) => NodeKind::Pdf,
                    Some(FileType::Epub) => NodeKind::Epub,
                    Some(FileType::Notebook) | None => NodeKind::Notebook,
                    Some(FileType::Other) => {
                        warnings.push(format!("{}: unknown fileType, skipped", e.uuid));
                        continue;
                    }
                }
            }
            ItemType::Unknown => {
                warnings.push(format!("{}: unknown item type, skipped", e.uuid));
                continue;
            }
        };
        if kind == NodeKind::Notebook && !opts.notebooks {
            continue;
        }
        let stat = kind.data_file().and_then(|k| e.stat(k));
        if kind.data_file().is_some() && stat.is_none() {
            warnings.push(format!("{}: {:?} document has no data file", e.uuid, kind));
        }
        items.push(Item {
            uuid: e.uuid,
            size: stat.map_or(0, |s| s.size),
            data_mtime_ms: stat.map_or(meta.last_modified.unwrap_or(0), |s| s.mtime_ms),
            meta,
            kind,
        });
    }
    items
}

/// A folder that is (indirectly) its own parent would never be reachable from the
/// root. Re-hang the first folder found on each cycle under lost+found.
fn break_cycles(items: &[Item], parent_of: &mut [Result<usize, NodeId>], warnings: &mut Vec<String>) {
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
                    parent_of[cur] = Err(LOST_FOUND_ID);
                    break;
                }
                S::New => {
                    state[cur] = S::Walking;
                    path.push(cur);
                    match parent_of[cur] {
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
    use crate::index::FileStat;

    fn u(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn folder(id: u8, name: &str, parent: &str) -> Entry {
        Entry {
            uuid: u(id),
            metadata: format!(r#"{{"type":"CollectionType","visibleName":"{name}","parent":"{parent}","lastModified":"{id}"}}"#).into_bytes(),
            content: None,
            files: vec![],
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
        }
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
            &TreeOptions::default(),
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
        assert_eq!(paths(&Tree::build(&entries, 0, &TreeOptions::default())), [".trash"]);
        let t = Tree::build(&entries, 0, &TreeOptions { notebooks: true });
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
            &TreeOptions::default(),
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
        let t = Tree::build(&[doc(1, "A", "", "pdf", 1)], 0, &TreeOptions::default());
        assert!(t.resolve(".lost+found").is_none());
    }

    #[test]
    fn clashes_numbered_oldest_first() {
        // createdTime = id, so 3 is older than 7 and keeps the plain name.
        let t = Tree::build(
            &[doc(7, "Wiki", "trash", "pdf", 1), doc(3, "Wiki", "trash", "pdf", 1), doc(5, "Wiki", "trash", "pdf", 1)],
            0,
            &TreeOptions::default(),
        );
        assert_eq!(t.by_uuid(&u(3)).unwrap().name, "Wiki.pdf");
        assert_eq!(t.by_uuid(&u(5)).unwrap().name, "Wiki (2).pdf");
        assert_eq!(t.by_uuid(&u(7)).unwrap().name, "Wiki (3).pdf");
    }

    #[test]
    fn pdf_and_folder_with_same_base_do_not_clash() {
        let t = Tree::build(&[folder(1, "Notes", ""), doc(2, "Notes", "", "pdf", 1)], 0, &TreeOptions::default());
        assert!(t.resolve("Notes").unwrap().kind.is_dir());
        assert_eq!(t.resolve("Notes.pdf").unwrap().kind, NodeKind::Pdf);
    }

    #[test]
    fn reserved_names_at_root() {
        let t = Tree::build(&[folder(1, ".trash", ""), folder(2, ".lost+found", "")], 0, &TreeOptions::default());
        assert_eq!(t.by_uuid(&u(1)).unwrap().name, ".trash (2)");
        assert_eq!(t.by_uuid(&u(2)).unwrap().name, ".lost+found (2)");
        assert_eq!(t.resolve(".trash").unwrap().kind, NodeKind::Trash);
    }

    #[test]
    fn slash_names() {
        let t = Tree::build(&[doc(1, "01/02/24", "", "pdf", 1)], 0, &TreeOptions::default());
        assert!(t.resolve("01∕02∕24.pdf").is_some());
    }

    #[test]
    fn folder_cycle_goes_to_lost_found() {
        let (a, b) = (u(1).to_string(), u(2).to_string());
        let t = Tree::build(&[folder(1, "A", &b), folder(2, "B", &a), doc(3, "x", &a, "pdf", 1)], 0, &TreeOptions::default());
        let p = paths(&t);
        assert!(p.iter().any(|p| p.starts_with(".lost+found/")), "{p:?}");
        assert!(p.iter().any(|p| p.ends_with("/x.pdf")), "{p:?}");
        assert_eq!(t.warnings().len(), 1);
    }

    #[test]
    fn inodes_stable_across_builds() {
        let e = [folder(1, "A", ""), doc(2, "x", "", "pdf", 1)];
        let (t1, t2) = (Tree::build(&e, 0, &TreeOptions::default()), Tree::build(&e, 1, &TreeOptions::default()));
        for n in t1.nodes() {
            assert_eq!(t2.node(n.ino).map(|m| &m.name), Some(&n.name));
        }
    }

    #[test]
    fn bad_json_is_a_warning_not_a_failure() {
        let mut bad = doc(1, "x", "", "pdf", 1);
        bad.metadata = b"{not json".to_vec();
        let t = Tree::build(&[bad, doc(2, "ok", "", "pdf", 1)], 0, &TreeOptions::default());
        assert_eq!(paths(&t), [".trash", "ok.pdf"]);
        assert_eq!(t.warnings().len(), 1);
    }
}
