//! The re-fs wire protocol. See docs/protocol.md.
//!
//! Every frame is a 9-byte header followed by `len` payload bytes:
//!
//! ```text
//! len: u32 LE | kind: u8 | req_id: u32 LE | payload
//! ```
//!
//! `kind` 1 is a postcard-encoded [`Request`] or [`Response`]; `kind` 2 is raw file
//! bytes answering a [`Request::Read`], written by the agent with `sendfile`.
//! Replies carry the `req_id` of their request, so many requests can be in flight
//! and replies may arrive in any order. Events pushed after [`Request::Watch`]
//! carry the Watch request's id.

mod codec;
mod msg;

pub use codec::{encode_data_header, encode_msg, Frame, FrameDecoder, FrameKind, HEADER_LEN, MAX_FRAME};
pub use msg::{compress_entries, compress_pages, decompress_entries, decompress_pages, ErrorCode, Expect, Op, PageData, Plan, Request, Response, MAX_READ};

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("frame of {0} bytes exceeds the limit")]
    TooLarge(u32),
    #[error("unknown frame kind {0}")]
    BadKind(u8),
    #[error("bad message: {0}")]
    Decode(#[from] postcard::Error),
    #[error("bad compressed index: {0}")]
    Decompress(#[from] lz4_flex::block::DecompressError),
}
