//! Shared by re-fs (client) and re-fsd (tablet agent): the reMarkable on-disk format,
//! the folder tree built from it, and the wire protocol. No I/O happens here.

pub mod index;
pub mod tlsid;
pub mod wire;

/// 3: page files in the index; ReadPage. 4: Wi-Fi uses TLS; the challenge login is gone.
/// 5: ReadPages, several page files in one round trip.
pub const PROTO_VERSION: u16 = 5;
