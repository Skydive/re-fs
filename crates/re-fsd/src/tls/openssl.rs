//! The OpenSSL backend: the system's libssl/libcrypto 3 (on the reMarkable,
//! /usr/lib/libssl.so.3, which xochitl uses too), so the agent needn't carry its
//! own TLS stack.
//!
//! OpenSSL is stream-oriented; the event loop wants buffers. So the SslStream runs
//! over an in-memory pipe: `read_tls` pours socket bytes in, `write_tls` drains
//! the records OpenSSL produced. Reads that would need more input surface as
//! WouldBlock, exactly like the rustls backend.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openssl::asn1::Asn1Time;
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::pkey::{Id, PKey, Private};
use openssl::ssl::{ErrorCode, Ssl, SslAcceptor, SslContext, SslMethod, SslOptions, SslStream, SslVerifyMode, SslVersion};
use openssl::x509::{X509Name, X509};

use crate::sshauth;

pub type Key = PKey<Private>;
pub type Config = SslContext;

/// TLS 1.3 suites, ChaCha20 first: the tablet's Cortex-A7 has no AES instructions.
const SUITES: &str = "TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384";

/// Our ed25519 key: created on first use, kept as PKCS#8 DER (0600). Same file
/// format as the rustls backend, so switching backends keeps the client's pin.
pub fn load_or_create_key(path: &Path) -> io::Result<Key> {
    match fs::read(path) {
        Ok(der) => PKey::private_key_from_pkcs8(&der).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let key = PKey::generate_ed25519().map_err(io::Error::other)?;
            let der = key.private_key_to_pkcs8().map_err(io::Error::other)?;
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(&der)?;
            Ok(key)
        }
        Err(e) => Err(e),
    }
}

pub fn public_hex(key: &Key) -> String {
    re_proto::tlsid::hex(&key.raw_public_key().unwrap_or_default())
}

fn self_signed(key: &Key) -> Result<X509, openssl::error::ErrorStack> {
    let mut name = X509Name::builder()?;
    name.append_entry_by_text("CN", "re-fsd")?;
    let name = name.build();
    let mut b = X509::builder()?;
    b.set_version(2)?;
    let serial = BigNum::from_u32(1)?.to_asn1_integer()?;
    b.set_serial_number(&serial)?;
    b.set_subject_name(&name)?;
    b.set_issuer_name(&name)?;
    let (from, until) = (Asn1Time::days_from_now(0)?, Asn1Time::days_from_now(3650)?);
    b.set_not_before(&from)?;
    b.set_not_after(&until)?;
    b.set_pubkey(key)?;
    b.sign(key, MessageDigest::null())?; // ed25519 signs without a separate digest
    Ok(b.build())
}

pub fn server_config(key: &Key, authorized_keys: PathBuf) -> io::Result<Arc<Config>> {
    let ctx = (|| -> Result<SslContext, openssl::error::ErrorStack> {
        let mut b = SslAcceptor::mozilla_modern_v5(SslMethod::tls_server())?;
        b.set_min_proto_version(Some(SslVersion::TLS1_3))?;
        b.set_ciphersuites(SUITES)?;
        b.set_options(SslOptions::CIPHER_SERVER_PREFERENCE);
        b.set_private_key(key)?;
        let cert = self_signed(key)?;
        b.set_certificate(&cert)?;
        // Clients present self-signed certificates for ssh keys: accept the
        // certificate if its ed25519 key is in authorized_keys (re-read per
        // handshake). OpenSSL still checks the client proves it holds the key.
        b.set_verify_callback(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT, move |_chain_ok, store| {
            if store.error_depth() != 0 {
                return false;
            }
            let Some(cert) = store.current_cert() else { return false };
            let Ok(pk) = cert.public_key() else { return false };
            if pk.id() != Id::ED25519 {
                return false;
            }
            let Ok(raw) = pk.raw_public_key() else { return false };
            let Ok(raw): Result<[u8; 32], _> = raw.try_into() else { return false };
            sshauth::authorized(&authorized_keys, &raw)
        });
        Ok(b.build().into_context())
    })()
    .map_err(io::Error::other)?;
    Ok(Arc::new(ctx))
}

/// The in-memory transport the SslStream reads from and writes to.
#[derive(Default)]
struct Pipe {
    incoming: VecDeque<u8>,
    outgoing: Vec<u8>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.incoming.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min(self.incoming.len());
        for (b, v) in buf.iter_mut().zip(self.incoming.drain(..n)) {
            *b = v;
        }
        Ok(n)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.outgoing.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Plaintext is taken this much at a time, so little is ever buffered.
const PLAIN_CHUNK: usize = 64 * 1024;

pub struct Session {
    stream: SslStream<Pipe>,
    handshaken: bool,
}

fn map_err(e: openssl::ssl::Error) -> io::Error {
    match e.code() {
        ErrorCode::WANT_READ | ErrorCode::WANT_WRITE => io::ErrorKind::WouldBlock.into(),
        _ => io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()),
    }
}

impl Session {
    pub fn new(cfg: &Arc<Config>) -> io::Result<Self> {
        let ssl = Ssl::new(cfg).map_err(io::Error::other)?;
        let stream = SslStream::new(ssl, Pipe::default()).map_err(io::Error::other)?;
        Ok(Session { stream, handshaken: false })
    }

    pub fn read_tls(&mut self, r: &mut dyn Read) -> io::Result<usize> {
        let mut buf = [0u8; 16 * 1024];
        let n = r.read(&mut buf)?;
        self.stream.get_mut().incoming.extend(&buf[..n]);
        Ok(n)
    }

    /// Advance the handshake. Errors (a refused client certificate) are fatal; the
    /// alert OpenSSL queued goes out with the next write_tls.
    pub fn process(&mut self) -> io::Result<()> {
        if !self.handshaken {
            match self.stream.accept() {
                Ok(()) => self.handshaken = true,
                Err(e) => match map_err(e) {
                    e if e.kind() == io::ErrorKind::WouldBlock => {}
                    e => return Err(e),
                },
            }
        }
        Ok(())
    }

    pub fn read_plain(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.handshaken {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        match self.stream.ssl_read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.code() == ErrorCode::ZERO_RETURN => Ok(0),
            Err(e) => Err(map_err(e)),
        }
    }

    pub fn write_plain(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.handshaken {
            return Ok(0);
        }
        match self.stream.ssl_write(&buf[..buf.len().min(PLAIN_CHUNK)]) {
            Ok(n) => Ok(n),
            Err(e) => match map_err(e) {
                e if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
                e => Err(e),
            },
        }
    }

    pub fn wants_write(&self) -> bool {
        !self.stream.get_ref().outgoing.is_empty()
    }

    pub fn write_tls(&mut self, w: &mut dyn Write) -> io::Result<usize> {
        let out = &mut self.stream.get_mut().outgoing;
        let n = w.write(out)?;
        out.drain(..n);
        Ok(n)
    }
}
