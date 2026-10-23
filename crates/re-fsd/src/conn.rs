//! One client connection: decoded requests in, a queue of bytes and file ranges out.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

use mio::net::TcpStream;
use re_proto::wire::{encode_data_header, encode_msg, FrameDecoder, Response};

pub enum Io {
    Tcp(TcpStream),
    Stdio { r: File, w: File },
}

impl Io {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Io::Tcp(s) => s.read(buf),
            Io::Stdio { r, .. } => r.read(buf),
        }
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Io::Tcp(s) => s.write(buf),
            Io::Stdio { w, .. } => w.write(buf),
        }
    }

    fn out_fd(&self) -> BorrowedFd<'_> {
        match self {
            // SAFETY: the fd is owned by `s`, which outlives the borrow.
            Io::Tcp(s) => unsafe { BorrowedFd::borrow_raw(s.as_raw_fd()) },
            Io::Stdio { w, .. } => w.as_fd(),
        }
    }
}

enum Out {
    Bytes { buf: Vec<u8>, pos: usize },
    File { file: File, offset: u64, remaining: u64 },
}

pub struct Conn {
    pub io: Io,
    pub decoder: FrameDecoder,
    out: VecDeque<Out>,
    /// The request id of this connection's Watch, if it sent one.
    pub watch_id: Option<u32>,
}

pub enum ReadState {
    Open,
    Eof,
}

impl Conn {
    pub fn new(io: Io) -> Self {
        Conn { io, decoder: FrameDecoder::new(), out: VecDeque::new(), watch_id: None }
    }

    /// Read everything available into the decoder (edge-triggered: until WouldBlock).
    pub fn fill(&mut self) -> io::Result<ReadState> {
        let mut buf = [0u8; 64 * 1024];
        loop {
            match self.io.read(&mut buf) {
                Ok(0) => return Ok(ReadState::Eof),
                Ok(n) => self.decoder.feed(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadState::Open),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    pub fn send(&mut self, req_id: u32, msg: &Response) {
        let mut buf = Vec::new();
        encode_msg(&mut buf, req_id, msg);
        self.out.push_back(Out::Bytes { buf, pos: 0 });
    }

    /// A Data frame of exactly `len` bytes of `file` from `offset`.
    pub fn send_file(&mut self, req_id: u32, file: File, offset: u64, len: u32) {
        self.out.push_back(Out::Bytes { buf: encode_data_header(req_id, len).to_vec(), pos: 0 });
        if len > 0 {
            self.out.push_back(Out::File { file, offset, remaining: u64::from(len) });
        }
    }

    /// Write as much as the socket takes. Ok(true) when the queue is empty.
    pub fn flush(&mut self) -> io::Result<bool> {
        while let Some(front) = self.out.front_mut() {
            let done = match front {
                Out::Bytes { buf, pos } => match self.io.write(&buf[*pos..]) {
                    Ok(n) => {
                        *pos += n;
                        *pos == buf.len()
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => false,
                    Err(e) => return Err(e),
                },
                Out::File { file, offset, remaining } => {
                    let want = (*remaining).min(1 << 20) as usize;
                    match rustix::fs::sendfile(self.io.out_fd(), &*file, Some(offset), want) {
                        Ok(0) => {
                            // The file shrank after we promised `remaining` more bytes.
                            // Pad with zeros to keep the stream framed; the client
                            // gets an Event for the change and fetches again.
                            let pad = vec![0u8; *remaining as usize];
                            *front = Out::Bytes { buf: pad, pos: 0 };
                            false
                        }
                        Ok(n) => {
                            *remaining -= n as u64;
                            *remaining == 0
                        }
                        Err(rustix::io::Errno::AGAIN) => return Ok(false),
                        Err(rustix::io::Errno::INTR) => false,
                        Err(e) => return Err(e.into()),
                    }
                }
            };
            if done {
                self.out.pop_front();
            }
        }
        Ok(true)
    }
}
