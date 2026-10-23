//! Logging in over Wi-Fi with the same ed25519 key ssh uses: we sign the agent's
//! challenge; it checks the key against the tablet's authorized_keys.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey};

static IDENTITY: OnceLock<PathBuf> = OnceLock::new();

/// Use this private key instead of ~/.ssh/id_ed25519.
pub fn set_identity(path: PathBuf) {
    let _ = IDENTITY.set(path);
}

fn identity_path() -> PathBuf {
    IDENTITY.get().cloned().unwrap_or_else(|| {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".ssh/id_ed25519")
    })
}

/// (public key, signature over `message`).
pub fn sign(message: &[u8]) -> Result<([u8; 32], Vec<u8>)> {
    let path = identity_path();
    let key = ssh_key::PrivateKey::read_openssh_file(&path).with_context(|| format!("reading {}", path.display()))?;
    if key.is_encrypted() {
        bail!("{} has a passphrase; re-fs can't use it yet (give --identity with an unencrypted key)", path.display());
    }
    let Some(kp) = key.key_data().ed25519() else { bail!("{} isn't an ed25519 key", path.display()) };
    let sk = SigningKey::from_bytes(&kp.private.to_bytes());
    Ok((sk.verifying_key().to_bytes(), sk.sign(message).to_bytes().to_vec()))
}
