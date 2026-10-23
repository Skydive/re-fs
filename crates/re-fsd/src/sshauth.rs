//! Which ssh keys may connect: the unrestricted ed25519 keys in the tablet's
//! `authorized_keys`. TLS client certificates are checked against these.

use std::fs;
use std::path::Path;

use base64::Engine;

pub const AUTHORIZED_KEYS: &str = "/home/root/.ssh/authorized_keys";

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

/// Whether `public_key` is an unrestricted ed25519 key in `authorized_keys`.
pub fn authorized(authorized_keys: &Path, public_key: &[u8; 32]) -> bool {
    fs::read_to_string(authorized_keys).is_ok_and(|text| ed25519_keys(&text).iter().any(|k| k == public_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_keys_parsing() {
        let pk = [5u8; 32];
        let mut blob = 11u32.to_be_bytes().to_vec();
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&pk);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);
        let text = format!(
            "ssh-rsa AAAAB3Nza bob@x\ncommand=\"/bin/true\" ssh-ed25519 {b64} restricted\nssh-ed25519 {b64} user@host\nssh-ed25519 !!!notbase64\n"
        );
        assert_eq!(ed25519_keys(&text), vec![pk]);
        let path = std::env::temp_dir().join(format!("re-fsd-ak-{}", std::process::id()));
        fs::write(&path, &text).unwrap();
        assert!(authorized(&path, &pk));
        assert!(!authorized(&path, &[6; 32]));
        fs::remove_file(path).unwrap();
    }
}
