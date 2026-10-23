//! re-fsd: serves the reMarkable's xochitl directory to re-fs.
//!
//! re-fsd --stdio                    speak the protocol on stdin/stdout (over ssh)
//! re-fsd --listen 127.0.0.1:7745    serve TCP (reached through WireGuard / USB)

mod conn;
mod server;
mod sshauth;
mod store;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

const DEFAULT_DIR: &str = "/home/root/.local/share/remarkable/xochitl";

const USAGE: &str = "usage: re-fsd [--dir DIR] [--stdio] [--listen ADDR]... [--allow-local IP]... [--ssh-auth] [-v]

  --dir DIR       the xochitl directory (default: /home/root/.local/share/remarkable/xochitl)
  --stdio         serve one client on stdin/stdout, exit when it closes
  --listen ADDR   serve TCP on ADDR, e.g. 127.0.0.1:7745 (repeatable)
  --allow-local IP
                  accept TCP only when the client reached us on this local
                  address (repeatable), e.g. 127.0.0.1 and 10.11.99.1 (USB)
                  with --listen 0.0.0.0:7745, so the USB interface can come
                  and go and Wi-Fi peers are refused
  --ssh-auth      accept other TCP clients (Wi-Fi) once they log in with an
                  ed25519 key from /home/root/.ssh/authorized_keys
  --idle-exit SECS
                  exit after SECS with no clients (with systemd socket activation,
                  which starts re-fsd again on the next connection)
  -v, --verbose   log to stderr
  -V, --version";

fn main() -> ExitCode {
    match parse() {
        Ok(Some(Args { dir, stdio, listen, allow_local, ssh_auth, idle_exit, verbose })) => {
            // Only the real xochitl directory belongs to a running xochitl.
            let restart_xochitl = dir == std::path::Path::new(DEFAULT_DIR);
            match server::run(server::Options { dir: &dir, stdio, listen: &listen, allow_local: &allow_local, ssh_auth, idle_exit: idle_exit.map(std::time::Duration::from_secs), verbose, restart_xochitl }) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("re-fsd: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("re-fsd: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    dir: PathBuf,
    stdio: bool,
    listen: Vec<SocketAddr>,
    allow_local: Vec<IpAddr>,
    ssh_auth: bool,
    idle_exit: Option<u64>,
    verbose: bool,
}

fn parse() -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;
    let mut a = Args { dir: PathBuf::from(DEFAULT_DIR), stdio: false, listen: Vec::new(), allow_local: Vec::new(), ssh_auth: false, idle_exit: None, verbose: false };
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Long("dir") => a.dir = p.value()?.into(),
            Long("stdio") => a.stdio = true,
            Long("listen") => a.listen.push(p.value()?.parse()?),
            Long("allow-local") => a.allow_local.push(p.value()?.parse()?),
            Long("ssh-auth") => a.ssh_auth = true,
            Long("idle-exit") => a.idle_exit = Some(p.value()?.parse()?),
            Short('v') | Long("verbose") => a.verbose = true,
            Short('V') | Long("version") => {
                println!("re-fsd {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Short('h') | Long("help") => {
                println!("{USAGE}");
                return Ok(None);
            }
            _ => return Err(arg.unexpected()),
        }
    }
    let activated = std::env::var("LISTEN_FDS").is_ok();
    if !a.stdio && a.listen.is_empty() && !activated {
        return Err("nothing to serve: give --stdio or --listen (or start from re-fsd.socket)".into());
    }
    Ok(Some(a))
}
