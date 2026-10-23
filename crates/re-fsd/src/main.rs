//! re-fsd: serves the reMarkable's xochitl directory to re-fs.
//!
//! re-fsd --stdio                    speak the protocol on stdin/stdout (over ssh)
//! re-fsd --listen 127.0.0.1:7745    serve TCP (reached through WireGuard / USB)

mod conn;
mod server;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const DEFAULT_DIR: &str = "/home/root/.local/share/remarkable/xochitl";

const USAGE: &str = "usage: re-fsd [--dir DIR] [--stdio] [--listen ADDR]... [-v]

  --dir DIR       the xochitl directory (default: /home/root/.local/share/remarkable/xochitl)
  --stdio         serve one client on stdin/stdout, exit when it closes
  --listen ADDR   serve TCP on ADDR, e.g. 127.0.0.1:7745 (repeatable)
  -v, --verbose   log to stderr
  -V, --version";

fn main() -> ExitCode {
    match parse() {
        Ok(Some((dir, stdio, listen, verbose))) => {
            // Only the real xochitl directory belongs to a running xochitl.
            let restart_xochitl = dir == std::path::Path::new(DEFAULT_DIR);
            match server::run(server::Options { dir: &dir, stdio, listen: &listen, verbose, restart_xochitl }) {
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

type Args = (PathBuf, bool, Vec<SocketAddr>, bool);

fn parse() -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;
    let (mut dir, mut stdio, mut listen, mut verbose) = (PathBuf::from(DEFAULT_DIR), false, Vec::new(), false);
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Long("dir") => dir = p.value()?.into(),
            Long("stdio") => stdio = true,
            Long("listen") => listen.push(p.value()?.parse()?),
            Short('v') | Long("verbose") => verbose = true,
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
    if !stdio && listen.is_empty() {
        return Err("nothing to serve: give --stdio or --listen".into());
    }
    Ok(Some((dir, stdio, listen, verbose)))
}
