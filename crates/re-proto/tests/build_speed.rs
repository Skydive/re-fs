//! Times Tree::build on the real mirror (gitignored), if present:
//! cargo test -p re-proto --features json --release -- --ignored --nocapture
#![cfg(feature = "json")]

use std::time::Instant;

use re_proto::index::tree::{Tree, TreeOptions};
use re_proto::index::{Entry, FileKind, FileStat};

#[test]
#[ignore]
fn build_speed_on_mirror() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../mirror");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries = Vec::new();
    for d in rd.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        let Some(u) = name.strip_suffix(".metadata").and_then(|u| uuid::Uuid::parse_str(u).ok()) else { continue };
        let content = std::fs::read(format!("{dir}/{u}.content")).ok();
        entries.push(Entry {
            uuid: u,
            metadata: std::fs::read(d.path()).unwrap(),
            content,
            files: vec![FileStat { kind: FileKind::Pdf, size: 1, mtime_ms: 1 }],
            pages: vec![],
        });
    }
    let t = Instant::now();
    let tree = Tree::build(&entries, 0, &TreeOptions::default());
    eprintln!("Tree::build of {} entries: {:?} ({} nodes)", entries.len(), t.elapsed(), tree.nodes().count());
}
