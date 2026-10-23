//! Shared by re-fs (client) and re-fsd (tablet agent): the reMarkable on-disk format,
//! the folder tree built from it, and the wire protocol. No I/O happens here.

pub mod index;
pub mod wire;

/// 2: Hello carries a login challenge; Auth. 3: page files in the index; ReadPage.
pub const PROTO_VERSION: u16 = 3;
