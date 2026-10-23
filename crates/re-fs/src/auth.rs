//! TLS for Wi-Fi: TLS 1.3 (rustls + ring) where both identities are ed25519 keys.
//! Ours is the ssh key (~/.ssh/id_ed25519), presented as a self-signed client
//! certificate; the tablet accepts it if the key is in its authorized_keys. The
//! tablet's is its own key, pinned here once (fetched over ssh by `re-fs pair`).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{bail, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};

static IDENTITY: OnceLock<PathBuf> = OnceLock::new();

/// Use this private key instead of ~/.ssh/id_ed25519.
pub fn set_identity(path: PathBuf) {
    let _ = IDENTITY.set(path);
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn identity_path() -> PathBuf {
    IDENTITY.get().cloned().unwrap_or_else(|| home().join(".ssh/id_ed25519"))
}

/// Where the tablet's pinned TLS key (hex) is kept.
pub fn pin_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("re-fs/tablet.pub")
}

pub fn save_pin(hex: &str) -> Result<()> {
    if re_proto::tlsid::unhex32(hex).is_none() {
        bail!("not a tablet key: {hex:?}");
    }
    let p = pin_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, format!("{}\n", hex.trim()))?;
    Ok(())
}

fn load_pin() -> Result<[u8; 32]> {
    let p = pin_path();
    let text = std::fs::read_to_string(&p)
        .with_context(|| format!("no tablet key pinned ({}): run `re-fs pair root@TABLET` or `re-fs deploy TABLET --service`", p.display()))?;
    re_proto::tlsid::unhex32(&text).with_context(|| format!("{}: not a tablet key", p.display()))
}

/// (seed, public) of our ssh key.
fn ssh_key() -> Result<([u8; 32], [u8; 32])> {
    let path = identity_path();
    let key = ssh_key::PrivateKey::read_openssh_file(&path).with_context(|| format!("reading {}", path.display()))?;
    if key.is_encrypted() {
        bail!("{} has a passphrase; re-fs can't use it yet (give --identity with an unencrypted key)", path.display());
    }
    let Some(kp) = key.key_data().ed25519() else { bail!("{} isn't an ed25519 key", path.display()) };
    Ok((kp.private.to_bytes(), kp.public.0))
}

/// The TLS client configuration, built once.
pub fn client_config() -> Result<Arc<ClientConfig>> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(c) = CFG.get() {
        return Ok(c.clone());
    }
    let (seed, public) = ssh_key()?;
    let pkcs8 = re_proto::tlsid::ed25519_pkcs8(&seed, &public);
    let kp = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(pkcs8.clone()), &rcgen::PKCS_ED25519)?;
    let cert = rcgen::CertificateParams::new(vec!["re-fs".to_string()])?.self_signed(&kp)?;
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(PinnedTablet { key: load_pin()?, algs: provider.signature_verification_algorithms });
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert.der().clone()], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)))?;
    Ok(CFG.get_or_init(|| Arc::new(cfg)).clone())
}

/// Accept only the tablet whose key we pinned.
#[derive(Debug)]
struct PinnedTablet {
    key: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedTablet {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match re_proto::tlsid::ed25519_key_in_cert(end_entity) {
            Some(k) if k == self.key => Ok(ServerCertVerified::assertion()),
            _ => Err(rustls::Error::General("the tablet's TLS key doesn't match the pinned one (re-run `re-fs pair`?)".into())),
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
