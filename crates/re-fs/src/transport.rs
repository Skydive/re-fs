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
    /// A listening re-fsd (USB, LAN, or through WireGuard).
    Tcp(SocketAddr),
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Ssh { host, .. } => write!(f, "ssh:{host}"),
            Target::Tcp(a) => write!(f, "tcp:{a}"),
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
        Target::Tcp(addr) => {
            let s = tokio::net::TcpStream::connect(addr).await?;
            s.set_nodelay(true)?;
            let (r, w) = s.into_split();
            Ok(Link { reader: Box::new(r), writer: Box::new(w), child: None })
        }
    }
}
