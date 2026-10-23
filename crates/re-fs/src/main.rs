mod local;

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use re_proto::index::tree::{Node, NodeKind, Tree, TreeOptions};

#[derive(Parser)]
#[command(version, about = "FUSE view of a reMarkable tablet's documents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the folder tree as re-fs would show it.
    Ls {
        /// A local copy of the tablet's xochitl directory.
        #[arg(long)]
        mirror: PathBuf,
        /// Path inside the tree.
        #[arg(default_value = "")]
        path: String,
        /// List subdirectories recursively.
        #[arg(short = 'R', long)]
        recursive: bool,
        /// Show size, date and kind.
        #[arg(short, long)]
        long: bool,
        /// Print full paths, one per line (for diffing).
        #[arg(long, conflicts_with = "long")]
        paths: bool,
        /// Include notebooks (shown as .pdf).
        #[arg(long)]
        notebooks: bool,
        /// Print warnings found while building the tree.
        #[arg(short, long)]
        warnings: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Ls { mirror, path, recursive, long, paths, notebooks, warnings } => {
            let entries = local::scan(&mirror)?;
            let tree = Tree::build(&entries, 0, &TreeOptions { notebooks });
            if warnings {
                for w in tree.warnings() {
                    eprintln!("warning: {w}");
                }
            }
            let Some(start) = tree.resolve(&path) else { bail!("no such path: {path}") };
            if !start.kind.is_dir() {
                print_node(&tree, start, long, paths, "");
                return Ok(());
            }
            list(&tree, start, recursive, long, paths, "");
        }
    }
    Ok(())
}

fn list(tree: &Tree, dir: &Node, recursive: bool, long: bool, paths: bool, indent: &str) {
    for n in tree.children(dir.ino).into_iter().flatten() {
        print_node(tree, n, long, paths, indent);
        if recursive && n.kind.is_dir() {
            list(tree, n, recursive, long, paths, &format!("{indent}  "));
        }
    }
}

fn print_node(tree: &Tree, n: &Node, long: bool, paths: bool, indent: &str) {
    let slash = if n.kind.is_dir() { "/" } else { "" };
    if paths {
        println!("{}{slash}", tree.path(n));
    } else if long {
        let kind = match n.kind {
            NodeKind::Pdf => "pdf",
            NodeKind::Epub => "epub",
            NodeKind::Notebook => "notebook",
            _ => "dir",
        };
        println!("{indent}{:>10}  {}  {:<8} {}{slash}", n.size, date(n.mtime_ms), kind, n.name);
    } else {
        println!("{indent}{}{slash}", n.name);
    }
}

/// YYYY-MM-DD in UTC, without pulling in a date crate.
fn date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn date() {
        assert_eq!(super::date(0), "1970-01-01");
        assert_eq!(super::date(1_701_100_163_735), "2023-11-27");
        assert_eq!(super::date(951_782_400_000), "2000-02-29");
    }
}
