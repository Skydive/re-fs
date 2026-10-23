//! TLS for Wi-Fi clients: TLS 1.3, mutual authentication by ed25519 keys (ours in
//! /home/root/re-fs, pinned by the client; the client's in authorized_keys).
//!
//! Two interchangeable backends, chosen at build time:
//! - `openssl` (default): the system's OpenSSL 3 (the reMarkable ships
//!   libssl.so.3), for a small agent.
//! - `rustls`: rustls + ring, statically linked, no system libraries needed.
//!
//! Both expose the same API: `load_or_create_key`, `public_hex`, `server_config`,
//! and a `Session` used by the connection code in a buffer-oriented way (bytes
//! from the socket in, plaintext out, and back), so the non-blocking event loop
//! doesn't care which one is underneath.

#[cfg(feature = "openssl")]
mod openssl;
#[cfg(feature = "openssl")]
pub use self::openssl::*;

#[cfg(feature = "rustls")]
mod rustls;
#[cfg(feature = "rustls")]
pub use self::rustls::*;

#[cfg(not(any(feature = "rustls", feature = "openssl")))]
mod none {
    //! Built without TLS: Wi-Fi clients are refused.
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    pub struct Key;
    pub struct Config;
    pub struct Session;

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "built without TLS support"))
    }
    pub fn load_or_create_key(_: &Path) -> io::Result<Key> {
        unsupported()
    }
    pub fn public_hex(_: &Key) -> String {
        String::new()
    }
    pub fn server_config(_: &Key, _: PathBuf) -> io::Result<Arc<Config>> {
        unsupported()
    }
    impl Session {
        pub fn new(_: &Arc<Config>) -> io::Result<Self> {
            unsupported()
        }
        pub fn read_tls(&mut self, _: &mut dyn io::Read) -> io::Result<usize> {
            unsupported()
        }
        pub fn process(&mut self) -> io::Result<()> {
            unsupported()
        }
        pub fn read_plain(&mut self, _: &mut [u8]) -> io::Result<usize> {
            unsupported()
        }
        pub fn write_plain(&mut self, _: &[u8]) -> io::Result<usize> {
            unsupported()
        }
        pub fn wants_write(&self) -> bool {
            false
        }
        pub fn write_tls(&mut self, _: &mut dyn io::Write) -> io::Result<usize> {
            unsupported()
        }
    }
}
#[cfg(not(any(feature = "rustls", feature = "openssl")))]
pub use none::*;
