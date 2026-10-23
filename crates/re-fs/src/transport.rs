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
    Tcp(SocketAddr),
    /// A listening re-fsd over TLS (Wi-Fi): our ssh key ↔ the pinned tablet key.
    Tls(SocketAddr),
    /// re-fsd over the tablet's USB network (the tablet is always 10.11.99.1),
    /// tried only while a reMarkable USB link is up.
    Usb { port: u16 },
}

/// The tablet's address on its USB network.
pub const USB_TABLET: [u8; 4] = [10, 11, 99, 1];
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
        Target::Tls(addr) => {
            let cfg = crate::auth::client_config().map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, format!("{e:#}")))?;
            let s = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("no answer from {addr}")))??;
            s.set_nodelay(true)?;
            let name = rustls::pki_types::ServerName::try_from("re-fsd").expect("valid name");
            let tls = tokio::time::timeout(CONNECT_TIMEOUT * 5, tokio_rustls::TlsConnector::from(cfg).connect(name, s))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
            let (r, w) = tokio::io::split(tls);
            Ok(Link { reader: Box::new(r), writer: Box::new(w), child: None })
        }
        Target::Tcp(_) | Target::Usb { .. } => {
            let addr = match target {
                Target::Usb { port } => SocketAddr::from((USB_TABLET, *port)),
                Target::Tcp(a) => *a,
                Target::Ssh { .. } | Target::Tls(_) => unreachable!(),
            };
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
    fn usb_network() {
        use std::net::Ipv4Addr;
        assert!(super::is_usb_net(Ipv4Addr::new(10, 11, 99, 2)));
        assert!(super::is_usb_net(Ipv4Addr::new(10, 11, 99, 7)), "seen on a real link (/27)");
        assert!(!super::is_usb_net(Ipv4Addr::new(10, 11, 99, 1)), "that's the tablet itself");
        assert!(!super::is_usb_net(Ipv4Addr::new(192, 168, 71, 15)));
    }
}
