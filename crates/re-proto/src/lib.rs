//! Shared by re-fs (client) and re-fsd (tablet agent): the reMarkable on-disk format,
//! the folder tree built from it, and (later) the wire protocol. No I/O happens here.

pub mod index;

pub const PROTO_VERSION: u16 = 1;
