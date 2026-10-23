//! One client connection: decoded requests in, a queue of bytes and file ranges out.
//! TCP connections start by sniffing their first byte: a TLS ClientHello (0x16)
//! makes it a TLS connection; anything else is the plain protocol, which only
//! trusted addresses (USB, loopback) may speak.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use mio::net::TcpStream;
use re_proto::wire::{encode_data_header, encode_msg, FrameDecoder, Response};

/// TLS records start with content type 22 (handshake).
const TLS_HANDSHAKE: u8 = 0x16;
/// File bytes go through TLS this much at a time.
const TLS_CHUNK: u64 = 64 * 1024;

pub enum Io {
    Tcp(TcpStream),
    Stdio { r: File, w: File },
    /// A new TCP connection, before its first bytes say whether it speaks TLS.
    Sniff { sock: Option<TcpStream>, trusted: bool },
    Tls { sock: TcpStream, tls: Box<crate::tls::Session> },
}

impl Io {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Io::Tcp(s) => s.read(buf),
            Io::Stdio { r, .. } => r.read(buf),
            Io::Sniff { .. } | Io::Tls { .. } => unreachable!("read through fill()"),
        }
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Io::Tcp(s) => s.write(buf),
            Io::Stdio { w, .. } => w.write(buf),
            Io::Sniff { .. } | Io::Tls { .. } => unreachable!("written through flush()"),
        }
    }

    fn out_fd(&self) -> BorrowedFd<'_> {
        match self {
            // SAFETY: the fd is owned by `s`, which outlives the borrow.
            Io::Tcp(s) => unsafe { BorrowedFd::borrow_raw(s.as_raw_fd()) },
            Io::Stdio { w, .. } => w.as_fd(),
            Io::Sniff { .. } | Io::Tls { .. } => unreachable!("no sendfile here"),
        }
    }

    /// The TCP stream, for (de)registering with the poller.
    pub fn tcp(&mut self) -> Option<&mut TcpStream> {
        match self {
            Io::Tcp(s) | Io::Tls { sock: s, .. } => Some(s),
            Io::Sniff { sock, .. } => sock.as_mut(),
            Io::Stdio { .. } => None,
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
    pub fn fill(&mut self, tls_config: Option<&Arc<crate::tls::Config>>) -> io::Result<ReadState> {
        let mut buf = vec![0u8; 64 * 1024];
        if let Io::Sniff { sock, trusted } = &mut self.io {
            let s = sock.as_mut().expect("sniffing connections have a socket");
            let n = match s.read(&mut buf) {
                Ok(0) => return Ok(ReadState::Eof),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadState::Open),
                Err(e) => return Err(e),
            };
            let sock = sock.take().unwrap();
            if buf[0] == TLS_HANDSHAKE {
                let cfg = tls_config.ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "TLS isn't enabled (--tls-key)"))?;
                let mut tls = crate::tls::Session::new(cfg)?;
                let mut first = &buf[..n];
                while !first.is_empty() {
                    tls.read_tls(&mut first)?;
                }
                self.io = Io::Tls { sock, tls: Box::new(tls) };
                if let Io::Tls { tls, .. } = &mut self.io {
                    let state = Self::process_tls(tls, &mut self.decoder, &mut buf)?;
                    if matches!(state, ReadState::Eof) {
                        return Ok(state);
                    }
                }
            } else if *trusted {
                self.decoder.feed(&buf[..n]);
                self.io = Io::Tcp(sock);
            } else {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "plain TCP from an untrusted address; use TLS"));
            }
        }
        if let Io::Tls { sock, tls } = &mut self.io {
            loop {
                match tls.read_tls(sock) {
                    Ok(0) => return Ok(ReadState::Eof),
                    Ok(_) => {
                        if let ReadState::Eof = Self::process_tls(tls, &mut self.decoder, &mut buf)? {
                            return Ok(ReadState::Eof);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadState::Open),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
        }
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

    /// Decrypt what the TLS session has buffered into the decoder. A failed handshake (bad
    /// client certificate) is an error; its alert goes out with the next flush.
    fn process_tls(tls: &mut crate::tls::Session, decoder: &mut FrameDecoder, buf: &mut [u8]) -> io::Result<ReadState> {
        tls.process()?;
        loop {
            match tls.read_plain(buf) {
                Ok(0) => return Ok(ReadState::Eof), // close_notify
                Ok(n) => decoder.feed(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadState::Open),
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
        match &mut self.io {
            Io::Sniff { .. } => return Ok(true),
            Io::Tls { sock, tls } => return Self::flush_tls(sock, tls, &mut self.out),
            _ => {}
        }
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

    /// TLS: encrypt queued output a chunk at a time, writing records as the socket
    /// takes them. The TLS session buffers a limited amount of plaintext (and none past
    /// the limit before the handshake completes), so writes may be partial; when
    /// it takes nothing and has nothing to send, wait for the next event.
    fn flush_tls(sock: &mut TcpStream, tls: &mut crate::tls::Session, out: &mut VecDeque<Out>) -> io::Result<bool> {
        loop {
            while tls.wants_write() {
                match tls.write_tls(sock) {
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            let Some(front) = out.front_mut() else { return Ok(true) };
            let (taken, done) = match front {
                Out::Bytes { buf, pos } => {
                    let n = tls.write_plain(&buf[*pos..])?;
                    *pos += n;
                    (n, *pos == buf.len())
                }
                Out::File { file, offset, remaining } => {
                    let mut chunk = vec![0u8; (*remaining).min(TLS_CHUNK) as usize];
                    let got = file.read_at(&mut chunk, *offset)?;
                    // A file that shrank is padded with zeros, as with sendfile.
                    let got = if got == 0 { chunk.len() } else { got };
                    let n = tls.write_plain(&chunk[..got])?;
                    *offset += n as u64;
                    *remaining -= n as u64;
                    (n, *remaining == 0)
                }
            };
            if done {
                out.pop_front();
            } else if taken == 0 && !tls.wants_write() {
                return Ok(false);
            }
        }
    }
}
