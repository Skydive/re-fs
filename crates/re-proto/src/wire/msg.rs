use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::pack::{pack, unpack, Codec};
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
        /// How to compress the reply.
        codec: Codec,
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
    /// Bytes of a file in a document's page folder (`name` must pass
    /// `is_page_file`). Answered like `Read`.
    ReadPage {
        uuid: Uuid,
        name: String,
        offset: u64,
        len: u32,
        expect_mtime_ms: Option<i64>,
    },
    /// Several page-folder files of one document in one round trip (`names` must
    /// pass `is_page_file`). Answered by `Pages`; files that don't exist are left out.
    ReadPages {
        uuid: Uuid,
        names: Vec<String>,
        /// How to compress the reply.
        codec: Codec,
    },
    /// Subscribe to `Event`s on this request id.
    Watch,
    /// Part of a file being uploaded, kept in the agent's staging area under `token`
    /// until an `Apply` moves it into place. Answered by `Ok`.
    PutChunk {
        token: u64,
        offset: u64,
        data: Vec<u8>,
    },
    /// Check the plan's preconditions, then write its files atomically.
    /// Answered by `Applied`, or `Err(Conflict)` with nothing written.
    Apply {
        plan: Plan,
    },
}

/// A set of file writes on the tablet, made by the client's planner. The agent
/// doesn't parse JSON; it only checks preconditions and moves bytes into place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub expect: Vec<Expect>,
    pub ops: Vec<Op>,
    /// Restart xochitl afterwards (debounced), so the tablet's UI shows the change.
    pub restart: bool,
}

/// The state of an item's `.metadata` the plan was made against: `None` means it
/// must not exist yet; `Some((mtime_ms, len))` means it must be unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expect {
    pub uuid: Uuid,
    pub metadata: Option<(i64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// Write `json` as `<uuid><file>` (`.metadata`, `.content` or `.pagedata`).
    PutJson { uuid: Uuid, file: FileKind, json: Vec<u8> },
    /// Move staged upload `token` to `<uuid><file>` (`.pdf` or `.epub`).
    PutStaged { uuid: Uuid, file: FileKind, token: u64 },
    /// Move staged upload `token` into the page folder as `<uuid>/<name>`
    /// (`name` must pass `is_page_file`; the folder is created).
    PutStagedPage { uuid: Uuid, name: String, token: u64 },
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
        /// postcard `Vec<Entry>`, packed with the requested codec; see [`compress_entries`].
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
    Ok,
    /// Answer to `ReadPages`: postcard `Vec<PageData>`, packed with the requested
    /// codec; see [`compress_pages`].
    Pages {
        pages: Vec<u8>,
    },
    /// The plan is on disk; the index is at `generation` and includes it.
    Applied {
        generation: u64,
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
    /// An Apply precondition failed: the tablet changed the item first.
    Conflict,
    /// Not allowed on this connection (plain TCP from an untrusted address).
    Unauthorized,
}

/// One file from a `ReadPages` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageData {
    pub name: String,
    pub mtime_ms: i64,
    pub data: Vec<u8>,
}

pub fn compress_pages(pages: &[PageData], codec: Codec) -> Vec<u8> {
    pack(codec, &postcard::to_stdvec(pages).expect("serialising pages"))
}

pub fn decompress_pages(blob: &[u8]) -> Result<Vec<PageData>, ProtoError> {
    Ok(postcard::from_bytes(&unpack(blob)?)?)
}

pub fn compress_entries(entries: &[&Entry], codec: Codec) -> Vec<u8> {
    // postcard into a Vec cannot fail for these types.
    pack(codec, &postcard::to_stdvec(entries).expect("serialising entries"))
}

pub fn decompress_entries(blob: &[u8]) -> Result<Vec<Entry>, ProtoError> {
    Ok(postcard::from_bytes(&unpack(blob)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileStat;

    #[test]
    fn pages_roundtrip() {
        let p = vec![
            PageData { name: "a.rm".into(), mtime_ms: 1, data: [1, 2, 3].repeat(100) },
            PageData { name: "b-metadata.json".into(), mtime_ms: 2, data: vec![] },
        ];
        let blob = compress_pages(&p, Codec::Lz4);
        assert!(blob.len() < 300);
        assert_eq!(decompress_pages(&blob).unwrap(), p);
    }

    #[test]
    fn entries_roundtrip_and_compress() {
        let e = Entry {
            uuid: Uuid::from_bytes([7; 16]),
            metadata: br#"{"type":"DocumentType","visibleName":"x","parent":""}"#.repeat(20),
            content: Some(b"{}".to_vec()),
            files: vec![FileStat { kind: FileKind::Pdf, size: 123, mtime_ms: 456 }],
            pages: vec![],
        };
        let blob = compress_entries(&[&e, &e], Codec::Zstd);
        assert!(blob.len() < 2 * e.metadata.len());
        assert_eq!(decompress_entries(&blob).unwrap(), vec![e.clone(), e]);
    }
}
