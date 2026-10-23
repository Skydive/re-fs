//! re-fsd: serves the reMarkable's xochitl directory to re-fs.
//!
//! re-fsd --stdio                    speak the protocol on stdin/stdout (over ssh)
//! re-fsd --listen 127.0.0.1:7745    serve TCP (reached through WireGuard / USB)

use re_fsd::{server, sshauth, tls};

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

const DEFAULT_DIR: &str = "/home/root/.local/share/remarkable/xochitl";
const DEFAULT_TLS_KEY: &str = "/home/root/re-fs/tls.key";

const USAGE: &str = "usage: re-fsd [--dir DIR] [--stdio] [--listen ADDR]... [--allow-local IP]... [--tls-key PATH] [-v]
       re-fsd --print-tls-key [--tls-key PATH]

  --dir DIR       the xochitl directory (default: /home/root/.local/share/remarkable/xochitl)
  --stdio         serve one client on stdin/stdout, exit when it closes
  --listen ADDR   serve TCP on ADDR, e.g. 127.0.0.1:7745 (repeatable)
  --allow-local IP
                  accept TCP only when the client reached us on this local
                  address (repeatable), e.g. 127.0.0.1 and 10.11.99.1 (USB)
                  with --listen 0.0.0.0:7745, so the USB interface can come
                  and go and Wi-Fi peers are refused
  --tls-key PATH   accept other TCP clients (Wi-Fi) over TLS 1.3, with this
                  ed25519 key (created if missing) as our identity; clients
                  need a certificate for a key in /home/root/.ssh/authorized_keys
  --authorized-keys PATH
                  keys allowed over TLS (default: /home/root/.ssh/authorized_keys)
  --print-tls-key  print our public key (hex) for clients to pin, and exit
  --idle-exit SECS
                  exit after SECS with no clients (with systemd socket activation,
                  which starts re-fsd again on the next connection)
  -v, --verbose   log to stderr
  -V, --version";

fn main() -> ExitCode {
    match parse() {
        Ok(Some(Args { dir, stdio, listen, allow_local, tls_key, authorized_keys, print_tls_key, idle_exit, verbose })) => {
            let key_path = tls_key.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_TLS_KEY));
            if print_tls_key {
                return match tls::load_or_create_key(&key_path) {
                    Ok(k) => {
                        println!("{}", tls::public_hex(&k));
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("re-fsd: {}: {e}", key_path.display());
                        ExitCode::FAILURE
                    }
                };
            }
            let tls = match tls_key.map(|p| tls::load_or_create_key(&p).and_then(|k| tls::server_config(&k, authorized_keys.clone()))) {
                None => None,
                Some(Ok(cfg)) => Some(cfg),
                Some(Err(e)) => {
                    eprintln!("re-fsd: TLS: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // Only the real xochitl directory belongs to a running xochitl.
            let restart_xochitl = dir == std::path::Path::new(DEFAULT_DIR);
            match server::run(server::Options { dir: &dir, stdio, listen: &listen, allow_local: &allow_local, tls, idle_exit: idle_exit.map(std::time::Duration::from_secs), verbose, restart_xochitl }) {
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
    tls_key: Option<PathBuf>,
    authorized_keys: PathBuf,
    print_tls_key: bool,
    idle_exit: Option<u64>,
    verbose: bool,
}

fn parse() -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;
    let mut a = Args { dir: PathBuf::from(DEFAULT_DIR), stdio: false, listen: Vec::new(), allow_local: Vec::new(), tls_key: None, authorized_keys: sshauth::AUTHORIZED_KEYS.into(), print_tls_key: false, idle_exit: None, verbose: false };
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Long("dir") => a.dir = p.value()?.into(),
            Long("stdio") => a.stdio = true,
            Long("listen") => a.listen.push(p.value()?.parse()?),
            Long("allow-local") => a.allow_local.push(p.value()?.parse()?),
            Long("tls-key") => a.tls_key = Some(p.value()?.into()),
            Long("print-tls-key") => a.print_tls_key = true,
            Long("authorized-keys") => a.authorized_keys = p.value()?.into(),
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
    if a.print_tls_key {
        return Ok(Some(a));
    }
    let activated = std::env::var("LISTEN_FDS").is_ok();
    if !a.stdio && a.listen.is_empty() && !activated {
        return Err("nothing to serve: give --stdio or --listen (or start from re-fsd.socket)".into());
    }
    Ok(Some(a))
}
