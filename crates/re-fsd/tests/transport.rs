//! End-to-end transmission through a real agent (server::run in a thread) over
//! plain TCP and TLS, with whichever TLS backend this build has. The client side
//! is always rustls, like re-fs, so the openssl build also checks interop.
//!
//!   cargo test -p re-fsd                                         # openssl backend
//!   cargo test -p re-fsd --no-default-features --features rustls # rustls backend

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use re_proto::index::FileKind;
use re_proto::wire::{decompress_entries, encode_msg, Frame, FrameDecoder, FrameKind, Request, Response};
use re_proto::PROTO_VERSION;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use uuid::Uuid;

const DOC: Uuid = Uuid::from_bytes([0xab; 16]);
const PAGE: &str = "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f";
const PAGE_DATA: &[u8] = b"reMarkable .lines file, version=6          (strokes)";

struct Agent {
    port: u16,
    dir: PathBuf,
    tablet_key: [u8; 32],
    pdf: Vec<u8>,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The ssh wire-format authorized_keys line for an ed25519 public key.
fn authorized_line(pk: &[u8]) -> String {
    let mut blob = 11u32.to_be_bytes().to_vec();
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&32u32.to_be_bytes());
    blob.extend_from_slice(pk);
    format!("ssh-ed25519 {} test", base64::engine::general_purpose::STANDARD.encode(blob))
}

/// Start an agent on a temp xochitl dir with one PDF; trusts only 127.0.0.1,
/// accepts TLS for `client_pk`.
fn start(name: &str, client_pk: &[u8]) -> Agent {
    let dir = std::env::temp_dir().join(format!("re-fsd-transport-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let x = dir.join("xochitl");
    std::fs::create_dir_all(&x).unwrap();
    // Bigger than one TLS chunk (64 KiB) so multi-record transfers are exercised.
    let pdf: Vec<u8> = b"%PDF-1.4\n".iter().copied().chain((0..300_000u32).map(|i| (i * 7 % 251) as u8)).collect();
    std::fs::write(x.join(format!("{DOC}.pdf")), &pdf).unwrap();
    std::fs::write(x.join(format!("{DOC}.metadata")), br#"{"type":"DocumentType","visibleName":"Doc","parent":""}"#).unwrap();
    std::fs::write(x.join(format!("{DOC}.content")), br#"{"fileType":"pdf"}"#).unwrap();
    std::fs::create_dir_all(x.join(DOC.to_string())).unwrap();
    std::fs::write(x.join(format!("{DOC}/{PAGE}.rm")), PAGE_DATA).unwrap();
    std::fs::write(dir.join("authorized_keys"), authorized_line(client_pk) + "\n").unwrap();

    let key = re_fsd::tls::load_or_create_key(&dir.join("tls.key")).unwrap();
    let tablet_key = re_proto::tlsid::unhex32(&re_fsd::tls::public_hex(&key)).unwrap();
    let tls = re_fsd::tls::server_config(&key, dir.join("authorized_keys")).unwrap();
    let port = free_port();
    let (xd, listen) = (x.clone(), vec![SocketAddr::from(([0, 0, 0, 0], port))]);
    std::thread::spawn(move || {
        let allow = [IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let opts = re_fsd::server::Options {
            dir: &xd,
            stdio: false,
            listen: &listen,
            allow_local: &allow,
            tls: Some(tls),
            idle_exit: None,
            verbose: false,
            restart_xochitl: false,
        };
        re_fsd::server::run(opts).unwrap();
    });
    // Wait for the listener.
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Agent { port, dir, tablet_key, pdf }
}

/// A blocking request/response client over any byte stream.
struct Client<S: Read + Write> {
    s: S,
    dec: FrameDecoder,
    next: u32,
}

impl<S: Read + Write> Client<S> {
    fn new(s: S) -> Self {
        Client { s, dec: FrameDecoder::new(), next: 1 }
    }

    fn call(&mut self, req: &Request) -> std::io::Result<Frame> {
        let id = self.next;
        self.next += 1;
        let mut buf = Vec::new();
        encode_msg(&mut buf, id, req);
        self.s.write_all(&buf)?;
        self.s.flush()?;
        let mut rd = [0u8; 64 * 1024];
        loop {
            if let Some(f) = self.dec.next_frame().unwrap() {
                assert_eq!(f.req_id, id);
                return Ok(f);
            }
            let n = self.s.read(&mut rd)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.dec.feed(&rd[..n]);
        }
    }

    /// Hello, the index, and the whole PDF; returns the PDF's bytes.
    fn exercise(&mut self) -> std::io::Result<Vec<u8>> {
        let hello: Response = self.call(&Request::Hello { proto: PROTO_VERSION })?.decode().unwrap();
        assert!(matches!(hello, Response::Hello { proto: PROTO_VERSION, .. }), "{hello:?}");
        let Response::Index { full: true, entries, .. } = self.call(&Request::GetIndex { since: None })?.decode().unwrap() else { panic!("no index") };
        let entries = decompress_entries(&entries).unwrap();
        assert_eq!(entries.len(), 1);
        let size = entries[0].stat(FileKind::Pdf).unwrap().size;
        // Page files in one batch: the existing one comes back, the missing one is left out.
        let names = vec![format!("{PAGE}.rm"), "11111111-1111-4111-8111-111111111111.rm".to_string()];
        let Response::Pages { pages } = self.call(&Request::ReadPages { uuid: DOC, names })?.decode().unwrap() else { panic!("no pages") };
        let pages = re_proto::wire::decompress_pages(&pages).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!((pages[0].name.as_str(), pages[0].data.as_slice()), (format!("{PAGE}.rm").as_str(), PAGE_DATA));
        let mut pdf = Vec::new();
        while (pdf.len() as u64) < size {
            let f = self.call(&Request::Read { uuid: DOC, file: FileKind::Pdf, offset: pdf.len() as u64, len: 100_000, expect_mtime_ms: None })?;
            assert_eq!(f.kind, FrameKind::Data);
            pdf.extend_from_slice(&f.payload);
        }
        Ok(pdf)
    }
}

/// Accept only the pinned tablet key (as re-fs does).
#[derive(Debug)]
struct Pinned([u8; 32]);

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, ee: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        (re_proto::tlsid::ed25519_key_in_cert(ee) == Some(self.0)).then(ServerCertVerified::assertion).ok_or(rustls::Error::General("pin".into()))
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// A client key pair (public key, cert, PKCS#8).
fn client_identity() -> (Vec<u8>, CertificateDer<'static>, Vec<u8>) {
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let cert = rcgen::CertificateParams::new(vec!["re-fs".into()]).unwrap().self_signed(&kp).unwrap();
    (kp.public_key_raw().to_vec(), cert.der().clone(), kp.serialize_der())
}

fn tls_connect(addr: (&str, u16), tablet_key: [u8; 32], cert: CertificateDer<'static>, pkcs8: Vec<u8>) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned(tablet_key)))
        .with_client_auth_cert(vec![cert], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)))
        .unwrap();
    let conn = rustls::ClientConnection::new(Arc::new(cfg), ServerName::try_from("re-fsd").unwrap()).unwrap();
    let sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    rustls::StreamOwned::new(conn, sock)
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn plain_tcp_from_a_trusted_address() {
    let (pk, _, _) = client_identity();
    let a = start("plain", &pk);
    let sock = TcpStream::connect(("127.0.0.1", a.port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert_eq!(Client::new(sock).exercise().unwrap(), a.pdf);
    cleanup(&a.dir);
}

#[test]
fn plain_tcp_from_an_untrusted_address_is_refused() {
    let (pk, _, _) = client_identity();
    let a = start("refused", &pk);
    // 127.0.0.2 isn't on the allow-list, standing in for Wi-Fi.
    let sock = TcpStream::connect(("127.0.0.2", a.port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    assert!(Client::new(sock).exercise().is_err());
    cleanup(&a.dir);
}

#[test]
fn tls_with_an_authorized_key() {
    let (pk, cert, pkcs8) = client_identity();
    let a = start("tls", &pk);
    let s = tls_connect(("127.0.0.2", a.port), a.tablet_key, cert, pkcs8);
    assert_eq!(Client::new(s).exercise().unwrap(), a.pdf);
    cleanup(&a.dir);
}

#[test]
fn tls_also_works_from_a_trusted_address() {
    let (pk, cert, pkcs8) = client_identity();
    let a = start("tls-trusted", &pk);
    let s = tls_connect(("127.0.0.1", a.port), a.tablet_key, cert, pkcs8);
    assert_eq!(Client::new(s).exercise().unwrap(), a.pdf);
    cleanup(&a.dir);
}

#[test]
fn tls_with_an_unknown_key_is_refused() {
    let (pk, _, _) = client_identity();
    let a = start("tls-stranger", &pk);
    let (_, cert, pkcs8) = client_identity(); // a different key
    let s = tls_connect(("127.0.0.2", a.port), a.tablet_key, cert, pkcs8);
    assert!(Client::new(s).exercise().is_err());
    cleanup(&a.dir);
}

#[test]
fn tls_to_the_wrong_tablet_is_refused() {
    let (pk, cert, pkcs8) = client_identity();
    let a = start("tls-pin", &pk);
    let s = tls_connect(("127.0.0.2", a.port), [1; 32], cert, pkcs8);
    assert!(Client::new(s).exercise().is_err());
    cleanup(&a.dir);
}
