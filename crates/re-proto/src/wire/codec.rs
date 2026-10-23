use serde::{de::DeserializeOwned, Serialize};

use super::ProtoError;

pub const HEADER_LEN: usize = 9;
/// Largest payload accepted. The full index is ~2 MB compressed; reads are ≤ 8 MiB.
pub const MAX_FRAME: u32 = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Msg = 1,
    Data = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub req_id: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, ProtoError> {
        Ok(postcard::from_bytes(&self.payload)?)
    }
}

fn header(out: &mut Vec<u8>, kind: FrameKind, req_id: u32, len: u32) {
    out.extend_from_slice(&len.to_le_bytes());
    out.push(kind as u8);
    out.extend_from_slice(&req_id.to_le_bytes());
}

/// Append a message frame to `out`.
pub fn encode_msg(out: &mut Vec<u8>, req_id: u32, msg: &impl Serialize) {
    let start = out.len();
    header(out, FrameKind::Msg, req_id, 0);
    let out_taken = std::mem::take(out);
    *out = postcard::to_extend(msg, out_taken).expect("serialising a message");
    let len = (out.len() - start - HEADER_LEN) as u32;
    out[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

/// The header of a Data frame; the caller sends `len` raw bytes after it.
pub fn encode_data_header(req_id: u32, len: u32) -> [u8; HEADER_LEN] {
    let mut v = Vec::with_capacity(HEADER_LEN);
    header(&mut v, FrameKind::Data, req_id, len);
    v.try_into().expect("header is HEADER_LEN bytes")
}

/// Feed it bytes as they arrive; it yields whole frames. No I/O of its own, so it
/// works under mio, tokio or a test alike.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    start: usize,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // Drop consumed bytes before growing, so the buffer doesn't creep.
        if self.start > 0 && self.start * 2 >= self.buf.len() {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtoError> {
        let avail = &self.buf[self.start..];
        if avail.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = u32::from_le_bytes(avail[0..4].try_into().unwrap());
        if len > MAX_FRAME {
            return Err(ProtoError::TooLarge(len));
        }
        let kind = match avail[4] {
            1 => FrameKind::Msg,
            2 => FrameKind::Data,
            k => return Err(ProtoError::BadKind(k)),
        };
        let req_id = u32::from_le_bytes(avail[5..9].try_into().unwrap());
        let total = HEADER_LEN + len as usize;
        if avail.len() < total {
            return Ok(None);
        }
        let payload = avail[HEADER_LEN..total].to_vec();
        self.start += total;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        Ok(Some(Frame { kind, req_id, payload }))
    }

    /// Bytes buffered but not yet returned as frames.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Request, Response};

    fn sample() -> Vec<u8> {
        let mut out = Vec::new();
        encode_msg(&mut out, 1, &Request::Hello { proto: 1 });
        out.extend_from_slice(&encode_data_header(2, 5));
        out.extend_from_slice(b"hello");
        encode_msg(&mut out, 3, &Response::Event { generation: 9 });
        out
    }

    fn check(frames: &[Frame]) {
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].decode::<Request>().unwrap(), Request::Hello { proto: 1 });
        assert_eq!((frames[1].kind, frames[1].req_id, &frames[1].payload[..]), (FrameKind::Data, 2, &b"hello"[..]));
        assert_eq!(frames[2].decode::<Response>().unwrap(), Response::Event { generation: 9 });
    }

    #[test]
    fn whole_buffer() {
        let mut d = FrameDecoder::new();
        d.feed(&sample());
        let frames: Vec<_> = std::iter::from_fn(|| d.next_frame().unwrap()).collect();
        check(&frames);
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn one_byte_at_a_time() {
        let mut d = FrameDecoder::new();
        let mut frames = Vec::new();
        for b in sample() {
            d.feed(&[b]);
            while let Some(f) = d.next_frame().unwrap() {
                frames.push(f);
            }
        }
        check(&frames);
    }

    #[test]
    fn rejects_oversized_and_unknown() {
        let mut d = FrameDecoder::new();
        d.feed(&[0xff, 0xff, 0xff, 0xff, 1, 0, 0, 0, 0]);
        assert!(matches!(d.next_frame(), Err(ProtoError::TooLarge(_))));
        let mut d = FrameDecoder::new();
        d.feed(&[0, 0, 0, 0, 9, 0, 0, 0, 0]);
        assert!(matches!(d.next_frame(), Err(ProtoError::BadKind(9))));
    }
}
