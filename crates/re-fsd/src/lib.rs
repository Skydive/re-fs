//! re-fsd as a library: the agent's event loop and parts, so tests can run a real
//! agent in-process. The binary (main.rs) only parses arguments.

#[cfg(all(feature = "rustls", feature = "openssl"))]
compile_error!("choose one TLS backend: feature \"openssl\" (default) or \"rustls\" (with --no-default-features)");

pub mod conn;
pub mod server;
pub mod sshauth;
pub mod store;
pub mod tls;
