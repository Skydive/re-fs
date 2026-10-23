mod auth;
mod cache;
mod client;
mod fs;
mod generate;
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
/// The tablet service's port.
const AGENT_PORT: u16 = 7745;

#[derive(Parser)]
#[command(version, about = "FUSE view of a reMarkable tablet's documents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// How to reach the tablet. Several may be given; they're tried in the order
/// USB (while plugged in), each --tcp, then --ssh.
#[derive(Args, Clone)]
struct Remote {
    /// Use the tablet's USB network (10.11.99.1) whenever the cable is plugged in.
    #[arg(long)]
    usb: bool,
    /// A listening re-fsd, e.g. 10.0.0.5:7745 over WireGuard (repeatable).
    #[arg(long, value_name = "ADDR")]
    tcp: Vec<SocketAddr>,
    /// Fall back to `ssh HOST re-fsd --stdio`.
    #[arg(long, value_name = "HOST")]
    ssh: Option<String>,
    /// re-fsd's path on the tablet (with --ssh).
    #[arg(long, default_value = DEFAULT_AGENT)]
    agent: String,
    /// re-fsd's TCP port (with --usb).
    #[arg(long, default_value_t = AGENT_PORT)]
    port: u16,
    /// ed25519 private key for logging in over Wi-Fi [default: ~/.ssh/id_ed25519].
    #[arg(long)]
    identity: Option<PathBuf>,
}

impl Remote {
    fn targets(&self) -> Vec<Target> {
        if let Some(id) = &self.identity {
            auth::set_identity(id.clone());
        }
        let mut t = Vec::new();
        if self.usb {
            t.push(Target::Usb { port: self.port });
        }
        t.extend(self.tcp.iter().map(|a| Target::Tcp(*a)));
        if let Some(host) = &self.ssh {
            t.push(Target::Ssh { host: host.clone(), agent: self.agent.clone() });
        }
        t
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Mount the tablet's documents (read-only).
    Mount {
        mountpoint: PathBuf,
        #[command(flatten)]
        remote: Remote,
        /// Leave notebooks out.
        #[arg(long)]
        hide_notebooks: bool,
        /// Leave out .originals, .rmdoc, .recent, .pinned and .tags.
        #[arg(long)]
        no_views: bool,
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
        #[arg(long, conflicts_with_all = ["ssh", "tcp", "usb"])]
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
        /// Leave notebooks out.
        #[arg(long)]
        hide_notebooks: bool,
        /// Leave out .originals, .rmdoc, .recent, .pinned and .tags.
        #[arg(long)]
        no_views: bool,
        /// Print warnings found while building the tree.
        #[arg(short, long)]
        warnings: bool,
    },
    /// Copy PDFs/EPUBs onto the tablet without mounting.
    Push {
        /// Files to send (.pdf or .epub).
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Folder on the tablet, e.g. "Books/Tech" (default: the top level).
        #[arg(long, default_value = "")]
        to: String,
        #[command(flatten)]
        remote: Remote,
    },
    /// Copy re-fsd to the tablet over ssh.
    Deploy {
        /// ssh destination, e.g. root@tablet.vpn
        host: String,
        /// The armv7 re-fsd binary (default: the one this re-fs was built with).
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Also install the re-fsd socket-activated service (port 7745). Without it,
        /// a running agent is stopped so the next connection starts the new one.
        #[arg(long)]
        service: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Mount { mountpoint, remote, hide_notebooks, no_views, rw, cache_dir, cache_mib, verbose } => {
            let targets = remote.targets();
            if targets.is_empty() {
                bail!("give --usb, --tcp ADDR and/or --ssh HOST");
            }
            let dir = cache_dir.unwrap_or_else(cache::Cache::default_dir);
            let cache = cache::Cache::open(dir.clone(), cache_mib << 20).with_context(|| format!("cache {}", dir.display()))?;
            runtime()?.block_on(mount(targets, &mountpoint, TreeOptions { notebooks: !hide_notebooks, views: !no_views }, cache, rw, verbose))
        }
        Cmd::Ls { mirror, remote, path, recursive, long, paths, hide_notebooks, no_views, warnings } => {
            let opts = TreeOptions { notebooks: !hide_notebooks, views: !no_views };
            let tree = match mirror {
                Some(m) => Tree::build(&local::scan(&m)?, 0, &opts),
                None if remote.targets().is_empty() => bail!("give --mirror DIR, --usb, --tcp ADDR or --ssh HOST"),
                None => runtime()?.block_on(fetch_tree(remote.targets(), &opts))?,
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
        Cmd::Push { files, to, remote } => {
            if remote.targets().is_empty() {
                bail!("give --usb, --tcp ADDR and/or --ssh HOST");
            }
            runtime()?.block_on(push(remote.targets(), &files, &to))
        }
        Cmd::Deploy { host, binary, service } => deploy(&host, binary, service),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?)
}

/// Connect using the first target that answers, and log in.
async fn connect_any(targets: &[Target]) -> Result<client::Client> {
    let mut errors = Vec::new();
    for t in targets.iter().filter(|t| t.available()) {
        match transport::connect(t).await {
            Ok(link) => {
                let c = client::Client::start(link);
                c.hello().await?;
                return Ok(c);
            }
            Err(e) => errors.push(format!("{t}: {e}")),
        }
    }
    bail!("can't reach the tablet: {}", errors.join("; "))
}

async fn push(targets: Vec<Target>, files: &[PathBuf], to: &str) -> Result<()> {
    use re_proto::index::naming;
    use re_proto::index::parse::ParentRef;
    use re_proto::index::plan::{self, DocKind};
    use re_proto::wire::{Expect, Plan};

    let c = connect_any(&targets).await?;
    let Response::Index { entries, generation, .. } = c.call_msg(&Request::GetIndex { since: None }).await? else {
        bail!("expected an index");
    };
    let tree = Tree::build(&decompress_entries(&entries)?, generation, &TreeOptions::default());
    let dest = tree.resolve(to).with_context(|| format!("no folder {to:?} on the tablet"))?;
    let parent = match (dest.kind, dest.view) {
        (NodeKind::Root, _) => ParentRef::Root,
        (NodeKind::Folder, re_proto::index::tree::View::Main) => ParentRef::Folder(dest.uuid.expect("folders have uuids")),
        _ => bail!("{to:?} isn't a folder on the tablet"),
    };
    for (i, path) in files.iter().enumerate() {
        let name = path.file_name().and_then(|n| n.to_str()).with_context(|| format!("{}: bad file name", path.display()))?;
        let kind = DocKind::from_name(name).with_context(|| format!("{name}: only .pdf and .epub can go on the tablet"))?;
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if !kind.sniff(&data[..data.len().min(64)]) {
            bail!("{name} isn't a valid {} file", kind.ext());
        }
        let uuid = uuid::Uuid::new_v4();
        let token = write::rand_u64();
        for (n, chunk) in data.chunks(4 << 20).enumerate() {
            c.put_chunk(token, (n * (4 << 20)) as u64, chunk.to_vec()).await?;
        }
        let base = naming::strip_ext(name, kind.ext()).unwrap_or(name);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
        let ops = plan::new_document(uuid, &parent, &naming::visible_name(base), kind, token, data.len() as u64, now);
        // Restart xochitl only after the last file.
        let restart = i + 1 == files.len();
        c.apply(Plan { expect: vec![Expect { uuid, metadata: None }], ops, restart }).await?;
        let shown = if to.is_empty() { name.to_owned() } else { format!("{to}/{name}") };
        eprintln!("re-fs: {shown} ({} bytes)", data.len());
    }
    Ok(())
}

async fn fetch_tree(targets: Vec<Target>, opts: &TreeOptions) -> Result<Tree> {
    let mut link = None;
    let mut errors = Vec::new();
    for t in targets.iter().filter(|t| t.available()) {
        match transport::connect(t).await {
            Ok(l) => {
                link = Some(l);
                break;
            }
            Err(e) => errors.push(format!("{t}: {e}")),
        }
    }
    let Some(link) = link else { bail!("can't reach the tablet: {}", errors.join("; ")) };
    let c = client::Client::start(link);
    c.hello().await?;
    let Response::Index { generation, entries, .. } = c.call_msg(&Request::GetIndex { since: None }).await? else {
        bail!("expected an index");
    };
    Ok(Tree::build(&decompress_entries(&entries)?, generation, opts))
}

async fn mount(targets: Vec<Target>, mountpoint: &Path, opts: TreeOptions, cache: Arc<cache::Cache>, rw: bool, verbose: bool) -> Result<()> {
    let shared = session::Shared::new(&opts, cache, rw, verbose);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let via = targets.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    tokio::spawn(session::run(targets, shared.clone(), opts, ready_tx));
    match tokio::time::timeout(Duration::from_secs(60), ready_rx).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => return Err(e),
        Ok(Err(_)) => bail!("session ended before connecting"),
        Err(_) => bail!("{via}: no index after 60 s"),
    }
    let tree = shared.tree.load();
    let fs = fs::ReFs::new(Arc::clone(&shared), tokio::runtime::Handle::current());
    let session = fuser::Session::new(fs, mountpoint, &fs::mount_options(rw))
        .with_context(|| format!("mounting on {}", mountpoint.display()))?;
    *shared.notifier.lock().unwrap() = Some(session.notifier());
    let bg = session.spawn()?;
    eprintln!("re-fs: {} items mounted on {} via {via} (Ctrl-C to unmount)", tree.nodes().count(), mountpoint.display());
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

const SERVICE_UNIT: &str = include_str!("../../../deploy/re-fsd.service");
const SOCKET_UNIT: &str = include_str!("../../../deploy/re-fsd.socket");
const INSTALL_SH: &str = include_str!("../../../deploy/install.sh");

/// Copy re-fsd to AGENT_DIR on the tablet, replacing any running copy atomically,
/// and optionally install the service.
fn deploy(host: &str, binary: Option<PathBuf>, service: bool) -> Result<()> {
    let binary = match binary.or_else(|| option_env!("RE_FSD_ARMV7").map(PathBuf::from)) {
        Some(b) => b,
        None => bail!("give --binary (e.g. $(nix build .#re-fsd-armv7 --print-out-paths)/bin/re-fsd)"),
    };
    let bytes = std::fs::read(&binary).with_context(|| format!("reading {}", binary.display()))?;
    let mut script = format!(
        "set -e; mkdir -p {AGENT_DIR}; cat > {AGENT_DIR}/re-fsd.new; chmod 755 {AGENT_DIR}/re-fsd.new; \
         mv -f {AGENT_DIR}/re-fsd.new {DEFAULT_AGENT}; {DEFAULT_AGENT} --version\n"
    );
    if service {
        // Heredocs with a quoted delimiter: the files are written verbatim.
        script += &format!(
            "cat > {AGENT_DIR}/re-fsd.service <<'RE_FS_EOF'\n{SERVICE_UNIT}RE_FS_EOF\n\
             cat > {AGENT_DIR}/re-fsd.socket <<'RE_FS_EOF'\n{SOCKET_UNIT}RE_FS_EOF\n\
             cat > {AGENT_DIR}/install.sh <<'RE_FS_EOF'\n{INSTALL_SH}RE_FS_EOF\n\
             chmod 755 {AGENT_DIR}/install.sh\nsh {AGENT_DIR}/install.sh\n"
        );
    } else {
        // A running agent exits so the next connection starts the new binary.
        script += "if systemctl is-active -q re-fsd.service 2>/dev/null; then systemctl stop re-fsd.service; echo 're-fsd stopped; the next connection starts the new one'; fi\n";
    }
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
        use re_proto::index::tree::Variant;
        let kind = match (n.kind, n.variant) {
            (_, Variant::Annotated) => "annotated",
            (_, Variant::Rmdoc) => "rmdoc",
            (_, Variant::Highlights) => "notes",
            (NodeKind::Pdf, _) => "pdf",
            (NodeKind::Epub, _) => "epub",
            (NodeKind::Notebook, _) => "notebook",
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
