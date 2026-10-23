//! Wi-Fi logins: the client signs this connection's challenge with an ed25519
//! key; we accept it if that key is in the tablet's `authorized_keys`. Same keys
//! and same revocation as ssh, without running ssh.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

pub const AUTHORIZED_KEYS: &str = "/home/root/.ssh/authorized_keys";

pub fn challenge() -> io::Result<[u8; 32]> {
    let mut c = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut c)?;
    Ok(c)
}

/// The unrestricted ssh-ed25519 keys in an authorized_keys file. Lines with
/// options (`command=…`, `from=…`) are skipped: those keys are meant to do less.
pub fn ed25519_keys(text: &str) -> Vec<[u8; 32]> {
    let mut keys = Vec::new();
    for line in text.lines().map(str::trim) {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("ssh-ed25519") {
            continue;
        }
        let Some(Ok(blob)) = parts.next().map(|b| base64::engine::general_purpose::STANDARD.decode(b)) else { continue };
        // SSH wire format: string "ssh-ed25519", string key(32).
        let name = b"ssh-ed25519";
        let mut want = (name.len() as u32).to_be_bytes().to_vec();
        want.extend_from_slice(name);
        want.extend_from_slice(&32u32.to_be_bytes());
        if blob.len() == want.len() + 32 && blob.starts_with(&want) {
            keys.push(blob[want.len()..].try_into().unwrap());
        }
    }
    keys
}

/// Whether `signature` over `message` is by `public_key`, and that key is authorized.
pub fn verify(authorized_keys: &Path, public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    let Ok(text) = fs::read_to_string(authorized_keys) else { return false };
    if !ed25519_keys(&text).iter().any(|k| k == public_key) {
        return false;
    }
    let (Ok(key), Ok(sig)) = (VerifyingKey::from_bytes(public_key), Signature::from_slice(signature)) else { return false };
    key.verify(message, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn authorized_keys_and_signatures() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let pk = sk.verifying_key().to_bytes();
        let mut blob = 11u32.to_be_bytes().to_vec();
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&pk);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);
        let other = SigningKey::from_bytes(&[8; 32]);
        let text = format!(
            "ssh-rsa AAAAB3Nza bob@x\ncommand=\"/bin/true\" ssh-ed25519 {b64} restricted\nssh-ed25519 {b64} user@host\nssh-ed25519 !!!notbase64\n"
        );
        assert_eq!(ed25519_keys(&text), vec![pk]);

        let path = std::env::temp_dir().join(format!("re-fsd-ak-{}", std::process::id()));
        fs::write(&path, &text).unwrap();
        let msg = re_proto::wire::auth_message(&[1; 32], 42);
        assert!(verify(&path, &pk, &msg, &sk.sign(&msg).to_bytes()));
        assert!(!verify(&path, &pk, &re_proto::wire::auth_message(&[2; 32], 42), &sk.sign(&msg).to_bytes()), "other challenge");
        assert!(!verify(&path, &other.verifying_key().to_bytes(), &msg, &other.sign(&msg).to_bytes()), "key not authorized");
        assert!(!verify(&path, &pk, &msg, &[0; 64]), "bad signature");
        fs::remove_file(path).unwrap();
    }
}
