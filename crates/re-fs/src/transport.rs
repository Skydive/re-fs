//! Ways to reach re-fsd. Each yields a byte stream carrying the same protocol.

use std::io;
use std::net::SocketAddr;
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};

#[derive(Debug, Clone)]
pub enum Target {
    /// `ssh HOST AGENT --stdio`: works anywhere ssh does, no service needed.
    Ssh { host: String, agent: String },
    /// A listening re-fsd over plain TCP (USB or WireGuard addresses only).
    Tcp(HostPort),
    /// A listening re-fsd over TLS (Wi-Fi): our ssh key ↔ the pinned tablet key.
    Tls(HostPort),
    /// re-fsd over the tablet's USB network (the tablet is always 10.11.99.1),
    /// tried only while a reMarkable USB link is up.
    Usb { port: u16 },
}

/// The tablet's address on its USB network.
pub const USB_TABLET: [u8; 4] = [10, 11, 99, 1];
/// re-fsd's port when none is given (the tablet service listens here).
pub const DEFAULT_PORT: u16 = 7745;

/// `HOST[:PORT]`: a hostname, IPv4 or IPv6 address (bracketed when a port follows),
/// with DEFAULT_PORT when the port is left out. Names are resolved on every
/// connect, so a changed address is picked up on reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl std::str::FromStr for HostPort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let bad_port = |p: &str| format!("bad port {p:?} in {s:?}");
        if let Some(rest) = s.strip_prefix('[') {
            // [v6] or [v6]:port
            let (host, after) = rest.split_once(']').ok_or_else(|| format!("missing ']' in {s:?}"))?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad_port(p))?,
                None if after.is_empty() => DEFAULT_PORT,
                None => return Err(format!("unexpected {after:?} after ']' in {s:?}")),
            };
            return Ok(HostPort { host: host.to_owned(), port });
        }
        if s.parse::<std::net::Ipv6Addr>().is_ok() {
            return Ok(HostPort { host: s.to_owned(), port: DEFAULT_PORT }); // bare IPv6, no port
        }
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().map_err(|_| bad_port(p))?),
            None => (s, DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err(format!("no host in {s:?}"));
        }
        Ok(HostPort { host: host.to_owned(), port })
    }
}

impl std::fmt::Display for HostPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// Resolve `hp` and connect to the first address that answers (each within
/// CONNECT_TIMEOUT).
async fn connect_tcp(hp: &HostPort) -> io::Result<tokio::net::TcpStream> {
    let addrs: Vec<SocketAddr> = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host((hp.host.as_str(), hp.port)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("resolving {} timed out", hp.host)))?
        .map_err(|e| io::Error::new(e.kind(), format!("resolving {}: {e}", hp.host)))?
        .collect();
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{} has no addresses", hp.host));
    for addr in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(s)) => {
                s.set_nodelay(true)?;
                return Ok(s);
            }
            Ok(Err(e)) => last = io::Error::new(e.kind(), format!("{addr}: {e}")),
            Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, format!("no answer from {addr}")),
        }
    }
    Err(last)
}
/// How long to wait for a TCP connection before trying the next transport.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a reMarkable is plugged in by USB: some USB network interface on this
/// machine has an address on the tablet's 10.11.99.x network (a /27 from its DHCP).
pub fn usb_present() -> bool {
    usb_interfaces().iter().any(|(_, ip)| is_usb_net(*ip))
}

fn is_usb_net(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0..3] == USB_TABLET[0..3] && o[3] != USB_TABLET[3]
}

/// (interface, IPv4 address) for interfaces backed by a USB device.
fn usb_interfaces() -> Vec<(String, std::net::Ipv4Addr)> {
    let mut out = Vec::new();
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a linked list we walk read-only and then free.
    unsafe {
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                let dev = std::fs::canonicalize(format!("/sys/class/net/{name}/device")).unwrap_or_default();
                if dev.to_string_lossy().contains("/usb") {
                    out.push((name, ip));
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out
}

impl Target {
    /// Worth trying right now? (USB only while the cable is in.)
    pub fn available(&self) -> bool {
        !matches!(self, Target::Usb { .. }) || usb_present()
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Ssh { host, .. } => write!(f, "ssh:{host}"),
            Target::Tcp(a) => write!(f, "tcp:{a}"),
            Target::Tls(a) => write!(f, "tls:{a}"),
            Target::Usb { port } => write!(f, "usb:{}:{port}", std::net::Ipv4Addr::from(USB_TABLET)),
        }
    }
}

pub struct Link {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
    /// Keeps the ssh process alive; killed when the link is dropped.
    pub child: Option<Child>,
}

pub async fn connect(target: &Target) -> io::Result<Link> {
    match target {
        Target::Ssh { host, agent } => {
            let mut child = Command::new("ssh")
                .args(["-T", "-e", "none", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=2"])
                .arg(host)
                .arg(format!("{agent} --stdio"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
            let reader = Box::new(child.stdout.take().expect("piped stdout"));
            let writer = Box::new(child.stdin.take().expect("piped stdin"));
            Ok(Link { reader, writer, child: Some(child) })
        }
        Target::Tls(hp) => {
            let cfg = crate::auth::client_config().map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, format!("{e:#}")))?;
            let s = connect_tcp(hp).await?;
            let name = rustls::pki_types::ServerName::try_from("re-fsd").expect("valid name");
            let tls = tokio::time::timeout(CONNECT_TIMEOUT * 5, tokio_rustls::TlsConnector::from(cfg).connect(name, s))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
            let (r, w) = tokio::io::split(tls);
            Ok(Link { reader: Box::new(r), writer: Box::new(w), child: None })
        }
        Target::Tcp(hp) => {
            let s = connect_tcp(hp).await?;
            let (r, w) = s.into_split();
            Ok(Link { reader: Box::new(r), writer: Box::new(w), child: None })
        }
        Target::Usb { port } => {
            let addr = SocketAddr::from((USB_TABLET, *port));
            let s = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("no answer from {addr}")))??;
            s.set_nodelay(true)?;
            let (r, w) = s.into_split();
            Ok(Link { reader: Box::new(r), writer: Box::new(w), child: None })
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_port_parsing() {
        use super::{HostPort, DEFAULT_PORT};
        let hp = |s: &str| s.parse::<HostPort>();
        let ok = |h: &str, p: u16| Ok(HostPort { host: h.into(), port: p });
        assert_eq!(hp("tablet.lan"), ok("tablet.lan", DEFAULT_PORT));
        assert_eq!(hp("tablet.lan:9000"), ok("tablet.lan", 9000));
        assert_eq!(hp("192.168.1.50"), ok("192.168.1.50", DEFAULT_PORT));
        assert_eq!(hp("192.168.1.50:7745"), ok("192.168.1.50", 7745));
        assert_eq!(hp("fe80::1"), ok("fe80::1", DEFAULT_PORT));
        assert_eq!(hp("[fe80::1]"), ok("fe80::1", DEFAULT_PORT));
        assert_eq!(hp("[fe80::1]:9000"), ok("fe80::1", 9000));
        assert!(hp("tablet:notaport").is_err());
        assert!(hp(":7745").is_err());
        assert!(hp("[fe80::1").is_err());
        assert_eq!(HostPort { host: "fe80::1".into(), port: 1 }.to_string(), "[fe80::1]:1");
        assert_eq!(HostPort { host: "tablet".into(), port: 1 }.to_string(), "tablet:1");
    }

    #[test]
    fn usb_network() {
        use std::net::Ipv4Addr;
        assert!(super::is_usb_net(Ipv4Addr::new(10, 11, 99, 2)));
        assert!(super::is_usb_net(Ipv4Addr::new(10, 11, 99, 7)), "seen on a real link (/27)");
        assert!(!super::is_usb_net(Ipv4Addr::new(10, 11, 99, 1)), "that's the tablet itself");
        assert!(!super::is_usb_net(Ipv4Addr::new(192, 168, 71, 15)));
    }
}
