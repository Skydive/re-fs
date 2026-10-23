//! TLS for Wi-Fi clients: TLS 1.3 (rustls + ring) with mutual authentication by
//! ssh-style ed25519 keys. Our identity is a key kept in /home/root/re-fs (the
//! client pins it, fetched once over ssh); the client's is its ssh key, which must
//! be in authorized_keys.

use std::fs;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};

use crate::sshauth;

/// Our key pair: created on first use, kept as PKCS#8 DER (0600).
pub fn load_or_create_key(path: &Path) -> io::Result<rcgen::KeyPair> {
    let der = match fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).map_err(io::Error::other)?;
            let der = key.serialize_der();
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
            io::Write::write_all(&mut f, &der)?;
            der
        }
        Err(e) => return Err(e),
    };
    let pkcs8 = PrivatePkcs8KeyDer::from(der);
    rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8, &rcgen::PKCS_ED25519).map_err(io::Error::other)
}

/// The public key clients pin, as hex.
pub fn public_hex(key: &rcgen::KeyPair) -> String {
    re_proto::tlsid::hex(key.public_key_raw())
}

pub fn server_config(key: &rcgen::KeyPair, authorized_keys: PathBuf) -> io::Result<Arc<ServerConfig>> {
    let cert = rcgen::CertificateParams::new(vec!["re-fsd".to_string()])
        .and_then(|p| p.self_signed(key))
        .map_err(io::Error::other)?;
    // ChaCha20-Poly1305 first: the tablet's Cortex-A7 has no AES instructions, and
    // the client (with them) would otherwise pick AES-GCM.
    let mut provider = ring::default_provider();
    provider.cipher_suites.sort_by_key(|cs| cs.suite() != rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256);
    let provider = Arc::new(provider);
    let verifier = Arc::new(AuthorizedKeys { path: authorized_keys, algs: provider.signature_verification_algorithms });
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert.der().clone()], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))
        .map_err(io::Error::other)?;
    let mut cfg = cfg;
    cfg.ignore_client_order = true;
    Ok(Arc::new(cfg))
}

/// Accept client certificates whose ed25519 key is in authorized_keys (re-read on
/// every handshake, so removing a key there takes effect at once).
#[derive(Debug)]
struct AuthorizedKeys {
    path: PathBuf,
    algs: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for AuthorizedKeys {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>], _now: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        let key = re_proto::tlsid::ed25519_key_in_cert(end_entity).ok_or_else(|| rustls::Error::General("client key isn't ed25519".into()))?;
        if sshauth::authorized(&self.path, &key) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General("client key isn't in authorized_keys".into()))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}
