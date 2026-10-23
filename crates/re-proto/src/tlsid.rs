//! Ed25519 keys as TLS identities: the encodings both ends need to use ssh-style
//! keys in self-signed TLS certificates. Pure byte handling, no crypto.

/// SubjectPublicKeyInfo of an Ed25519 key, up to the 32 key bytes:
/// SEQUENCE { SEQUENCE { OID 1.3.101.112 }, BIT STRING (0 unused bits) ... }
const ED25519_SPKI_PREFIX: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

/// The Ed25519 public key of a DER certificate (the first Ed25519
/// SubjectPublicKeyInfo in it), or None if it isn't an Ed25519 certificate.
pub fn ed25519_key_in_cert(der: &[u8]) -> Option<[u8; 32]> {
    let at = der.windows(ED25519_SPKI_PREFIX.len()).position(|w| w == ED25519_SPKI_PREFIX)?;
    der.get(at + ED25519_SPKI_PREFIX.len()..at + ED25519_SPKI_PREFIX.len() + 32)?.try_into().ok()
}

/// A PKCS#8 v2 (RFC 8410 OneAsymmetricKey) DER encoding of an Ed25519 key pair.
pub fn ed25519_pkcs8(seed: &[u8; 32], public: &[u8; 32]) -> Vec<u8> {
    let mut d = vec![0x30, 0x53, 0x02, 0x01, 0x01, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];
    d.extend_from_slice(seed);
    d.extend_from_slice(&[0xa1, 0x23, 0x03, 0x21, 0x00]);
    d.extend_from_slice(public);
    d
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_key_and_round_trips_hex() {
        let mut cert = vec![0x30, 0x82, 0x01, 0x00, 0xde, 0xad];
        cert.extend_from_slice(&ED25519_SPKI_PREFIX);
        cert.extend_from_slice(&[7; 32]);
        cert.extend_from_slice(&[0xff; 10]);
        assert_eq!(ed25519_key_in_cert(&cert), Some([7; 32]));
        assert_eq!(ed25519_key_in_cert(&cert[..20]), None);
        assert_eq!(unhex32(&hex(&[9; 32])), Some([9; 32]));
        assert_eq!(unhex32("zz"), None);
        assert_eq!(ed25519_pkcs8(&[1; 32], &[2; 32]).len(), 85);
    }
}
