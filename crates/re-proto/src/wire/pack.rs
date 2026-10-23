//! Compressed payloads (the index, page batches): a codec byte, then the data in
//! 1 MiB chunks, each `raw_len: u32, packed_len: u32, bytes`.
//!
//! The client picks the codec per request; replies say which they used, so
//! unpacking needs no context. Chunking keeps memory and time per step small on
//! the tablet, and avoids an lz4_flex bug on 32-bit ARM with single inputs of
//! ~9 MB and more. Chunks larger than 1 MiB don't compress noticeably better
//! (measured: < 0.3% on real index and stroke data).

use serde::{Deserialize, Serialize};

use super::ProtoError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Codec {
    /// Fast links: the tablet can't compress faster than USB sends.
    None,
    /// Cheap on the tablet's CPU; good for the index on fast links.
    #[default]
    Lz4,
    /// zstd level 1: about half lz4's size for the index, 2.5x the CPU; worth it
    /// on slow links (WireGuard).
    Zstd,
}

impl Codec {
    fn tag(self) -> u8 {
        match self {
            Codec::None => 0,
            Codec::Lz4 => 1,
            Codec::Zstd => 2,
        }
    }

    fn from_tag(t: u8) -> Option<Self> {
        [Codec::None, Codec::Lz4, Codec::Zstd].into_iter().find(|c| c.tag() == t)
    }
}

impl std::str::FromStr for Codec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Codec::None),
            "lz4" => Ok(Codec::Lz4),
            "zstd" => Ok(Codec::Zstd),
            _ => Err(format!("unknown codec {s:?} (none, lz4, zstd)")),
        }
    }
}

pub const CHUNK: usize = 1 << 20;
const ZSTD_LEVEL: i32 = 1;

pub fn pack(codec: Codec, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 16);
    out.push(codec.tag());
    for chunk in data.chunks(CHUNK) {
        let packed: std::borrow::Cow<[u8]> = match codec {
            Codec::None => chunk.into(),
            Codec::Lz4 => lz4_flex::compress(chunk).into(),
            Codec::Zstd => zstd::bulk::compress(chunk, ZSTD_LEVEL).expect("zstd compress").into(),
        };
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&packed);
    }
    out
}

pub fn unpack(blob: &[u8]) -> Result<Vec<u8>, ProtoError> {
    let bad = || ProtoError::Packed;
    let (&tag, mut rest) = blob.split_first().ok_or_else(bad)?;
    let codec = Codec::from_tag(tag).ok_or_else(bad)?;
    let mut out = Vec::new();
    while !rest.is_empty() {
        if rest.len() < 8 {
            return Err(bad());
        }
        let raw = u32::from_le_bytes(rest[0..4].try_into().unwrap()) as usize;
        let n = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let body = rest.get(8..8 + n).ok_or_else(bad)?;
        if raw > CHUNK {
            return Err(bad());
        }
        match codec {
            Codec::None if n == raw => out.extend_from_slice(body),
            Codec::None => return Err(bad()),
            Codec::Lz4 => out.extend_from_slice(&lz4_flex::decompress(body, raw).map_err(|_| bad())?),
            Codec::Zstd => out.extend_from_slice(&zstd::bulk::decompress(body, raw).map_err(|_| bad())?),
        }
        rest = &rest[8 + n..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_codec_across_chunks() {
        // 2.5 chunks of compressible but not trivial data.
        let data: Vec<u8> = (0..CHUNK * 5 / 2).map(|i| ((i * 31) % 251) as u8 ^ (i / 4096) as u8).collect();
        for codec in [Codec::None, Codec::Lz4, Codec::Zstd] {
            let p = pack(codec, &data);
            assert_eq!(unpack(&p).unwrap(), data, "{codec:?}");
            if codec != Codec::None {
                assert!(p.len() < data.len(), "{codec:?} should shrink this");
            }
        }
        assert_eq!(unpack(&pack(Codec::Zstd, b"")).unwrap(), b"");
    }

    #[test]
    fn rejects_garbage() {
        assert!(unpack(&[]).is_err());
        assert!(unpack(&[9]).is_err());
        assert!(unpack(&[1, 5, 0, 0, 0, 200, 0, 0, 0]).is_err());
        let mut p = pack(Codec::Lz4, b"hello hello hello");
        p.truncate(p.len() - 2);
        assert!(unpack(&p).is_err());
    }
}
