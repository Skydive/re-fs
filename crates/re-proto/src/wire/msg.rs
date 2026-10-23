use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ProtoError;
use crate::index::{Entry, FileKind};

/// Largest `len` a single Read may ask for.
pub const MAX_READ: u32 = 8 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Hello {
        proto: u16,
    },
    /// The whole index, or only what changed after generation `since` of agent
    /// instance `instance` (from Hello). Anything else gets the whole index.
    GetIndex {
        since: Option<(u64, u64)>,
    },
    /// Bytes of one file. Answered by a Data frame (possibly shorter at EOF), or by
    /// `Err(Changed)` when `expect_mtime_ms` is set and the file's mtime differs.
    Read {
        uuid: Uuid,
        file: FileKind,
        offset: u64,
        len: u32,
        expect_mtime_ms: Option<i64>,
    },
    /// Subscribe to `Event`s on this request id.
    Watch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Hello {
        proto: u16,
        agent: String,
        /// Changes on every agent start; generations are only comparable within one.
        instance: u64,
        generation: u64,
    },
    Index {
        instance: u64,
        generation: u64,
        /// `true`: `entries` is everything. `false`: apply `entries` and `removed`
        /// on top of the generation the client asked from.
        full: bool,
        removed: Vec<Uuid>,
        /// lz4-compressed postcard `Vec<Entry>`; see [`compress_entries`].
        entries: Vec<u8>,
    },
    /// Something changed; the index is now at `generation`.
    Event {
        generation: u64,
    },
    Err {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    BadRequest,
    NotFound,
    /// The file changed since the client's index (Read with `expect_mtime_ms`).
    Changed,
    Io,
    Unsupported,
}

pub fn compress_entries(entries: &[&Entry]) -> Vec<u8> {
    // postcard into a Vec cannot fail for these types.
    let raw = postcard::to_stdvec(entries).expect("serialising entries");
    lz4_flex::compress_prepend_size(&raw)
}

pub fn decompress_entries(blob: &[u8]) -> Result<Vec<Entry>, ProtoError> {
    let raw = lz4_flex::decompress_size_prepended(blob)?;
    Ok(postcard::from_bytes(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileStat;

    #[test]
    fn entries_roundtrip_and_compress() {
        let e = Entry {
            uuid: Uuid::from_bytes([7; 16]),
            metadata: br#"{"type":"DocumentType","visibleName":"x","parent":""}"#.repeat(20),
            content: Some(b"{}".to_vec()),
            files: vec![FileStat { kind: FileKind::Pdf, size: 123, mtime_ms: 456 }],
        };
        let blob = compress_entries(&[&e, &e]);
        assert!(blob.len() < 2 * e.metadata.len());
        assert_eq!(decompress_entries(&blob).unwrap(), vec![e.clone(), e]);
    }
}
