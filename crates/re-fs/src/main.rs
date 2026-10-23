mod cache;
mod client;
mod fs;
mod local;
mod session;
mod transport;
mod write;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use re_proto::index::tree::{Node, NodeKind, Tree, TreeOptions};
use re_proto::wire::{decompress_entries, Request, Response};

use crate::transport::Target;

/// Where `re-fs deploy` puts the agent, and where `--ssh` runs it from.
const AGENT_DIR: &str = "/home/root/re-fs";
const DEFAULT_AGENT: &str = "/home/root/re-fs/re-fsd";

#[derive(Parser)]
#[command(version, about = "FUSE view of a reMarkable tablet's documents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Remote {
    /// Reach the tablet with `ssh HOST re-fsd --stdio`.
    #[arg(long, value_name = "HOST", conflicts_with = "tcp")]
    ssh: Option<String>,
    /// Reach a listening re-fsd.
    #[arg(long, value_name = "ADDR")]
    tcp: Option<SocketAddr>,
    /// re-fsd's path on the tablet (with --ssh).
    #[arg(long, default_value = DEFAULT_AGENT)]
    agent: String,
}

impl Remote {
    fn target(&self) -> Option<Target> {
        match (&self.ssh, self.tcp) {
            (Some(host), _) => Some(Target::Ssh { host: host.clone(), agent: self.agent.clone() }),
            (None, Some(a)) => Some(Target::Tcp(a)),
            (None, None) => None,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Mount the tablet's documents (read-only).
    Mount {
        mountpoint: PathBuf,
        #[command(flatten)]
        remote: Remote,
        /// Include notebooks (not readable yet).
        #[arg(long)]
        notebooks: bool,
        /// Allow writes: mkdir, copying PDFs/EPUBs in, mv, and rm (to the tablet's trash).
        #[arg(long)]
        rw: bool,
        /// Where downloaded files and the last index are kept [default: ~/.cache/re-fs].
        #[arg(long)]
        cache_dir: Option<PathBuf>,
        /// Largest the cache may grow, in MiB.
        #[arg(long, default_value_t = 5120)]
        cache_mib: u64,
        /// Log connections, index updates and warnings to stderr.
        #[arg(short, long)]
        verbose: bool,
    },
    /// List the folder tree as re-fs would show it.
    Ls {
        /// A local copy of the tablet's xochitl directory.
        #[arg(long, conflicts_with_all = ["ssh", "tcp"])]
        mirror: Option<PathBuf>,
        #[command(flatten)]
        remote: Remote,
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
    /// Copy re-fsd to the tablet over ssh.
    Deploy {
        /// ssh destination, e.g. root@tablet.vpn
        host: String,
        /// The armv7 re-fsd binary (default: the one this re-fs was built with).
        #[arg(long)]
        binary: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Mount { mountpoint, remote, notebooks, rw, cache_dir, cache_mib, verbose } => {
            let target = remote.target().context("give --ssh HOST or --tcp ADDR")?;
            let dir = cache_dir.unwrap_or_else(cache::Cache::default_dir);
            let cache = cache::Cache::open(dir.clone(), cache_mib << 20).with_context(|| format!("cache {}", dir.display()))?;
            runtime()?.block_on(mount(target, &mountpoint, TreeOptions { notebooks }, cache, rw, verbose))
        }
        Cmd::Ls { mirror, remote, path, recursive, long, paths, notebooks, warnings } => {
            let opts = TreeOptions { notebooks };
            let tree = match (mirror, remote.target()) {
                (Some(m), _) => Tree::build(&local::scan(&m)?, 0, &opts),
                (None, Some(t)) => runtime()?.block_on(fetch_tree(t, &opts))?,
                (None, None) => bail!("give --mirror DIR, --ssh HOST or --tcp ADDR"),
            };
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
            Ok(())
        }
        Cmd::Deploy { host, binary } => deploy(&host, binary),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?)
}

async fn fetch_tree(target: Target, opts: &TreeOptions) -> Result<Tree> {
    let c = client::Client::start(transport::connect(&target).await?);
    c.hello().await?;
    let Response::Index { generation, entries, .. } = c.call_msg(&Request::GetIndex { since: None }).await? else {
        bail!("expected an index");
    };
    Ok(Tree::build(&decompress_entries(&entries)?, generation, opts))
}

async fn mount(target: Target, mountpoint: &Path, opts: TreeOptions, cache: Arc<cache::Cache>, rw: bool, verbose: bool) -> Result<()> {
    let shared = session::Shared::new(&opts, cache, rw, verbose);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(session::run(target.clone(), shared.clone(), opts, ready_tx));
    match tokio::time::timeout(Duration::from_secs(60), ready_rx).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => return Err(e),
        Ok(Err(_)) => bail!("session ended before connecting"),
        Err(_) => bail!("{target}: no index after 60 s"),
    }
    let tree = shared.tree.load();
    let fs = fs::ReFs::new(Arc::clone(&shared), tokio::runtime::Handle::current());
    let session = fuser::Session::new(fs, mountpoint, &fs::mount_options(rw))
        .with_context(|| format!("mounting on {}", mountpoint.display()))?;
    *shared.notifier.lock().unwrap() = Some(session.notifier());
    let bg = session.spawn()?;
    eprintln!("re-fs: {} items from {target} mounted on {} (Ctrl-C to unmount)", tree.nodes().count(), mountpoint.display());
    drop(tree);

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // The session thread ends when someone else unmounts (fusermount3 -u).
    let unmounted = async {
        while !bg.guard.is_finished() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = unmounted => eprintln!("re-fs: unmounted"),
    }
    drop(bg); // unmounts, if still mounted
    Ok(())
}

/// Copy re-fsd to AGENT_DIR on the tablet, replacing any running copy atomically.
fn deploy(host: &str, binary: Option<PathBuf>) -> Result<()> {
    let binary = match binary.or_else(|| option_env!("RE_FSD_ARMV7").map(PathBuf::from)) {
        Some(b) => b,
        None => bail!("give --binary (e.g. $(nix build .#re-fsd-armv7 --print-out-paths)/bin/re-fsd)"),
    };
    let bytes = std::fs::read(&binary).with_context(|| format!("reading {}", binary.display()))?;
    let script = format!(
        "set -e; mkdir -p {AGENT_DIR}; cat > {AGENT_DIR}/re-fsd.new; chmod 755 {AGENT_DIR}/re-fsd.new; \
         mv -f {AGENT_DIR}/re-fsd.new {DEFAULT_AGENT}; {DEFAULT_AGENT} --version"
    );
    let mut child = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", host, &script])
        .stdin(Stdio::piped())
        .spawn()
        .context("running ssh")?;
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), &bytes)?;
    let status = child.wait()?;
    if !status.success() {
        bail!("deploy failed ({status})");
    }
    eprintln!("re-fs: installed {} ({} bytes) as {host}:{DEFAULT_AGENT}", binary.display(), bytes.len());
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
