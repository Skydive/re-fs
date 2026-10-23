//! The single-threaded event loop: listeners, connections and inotify on one `mio::Poll`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::time::{Duration, Instant};

use inotify::{EventMask, Inotify, WatchMask};
use mio::net::TcpListener;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use re_proto::index::FileKind;
use re_proto::wire::{ErrorCode, Frame, FrameKind, Request, Response, MAX_READ};
use re_proto::PROTO_VERSION;
use uuid::Uuid;

use crate::conn::{Conn, Io, ReadState};
use crate::store::{uuid_of, Store};

const INOTIFY: Token = Token(0);
const STDIN: Token = Token(1);
const STDOUT: Token = Token(2);
const FIRST_LISTENER: usize = 10;
const FIRST_CONN: usize = 1000;
/// The stdio connection's key in `conns`.
const STDIO_CONN: usize = 1;

/// Wait this long after the last change before rescanning (xochitl writes in bursts),
/// but never longer than DEBOUNCE_MAX after the first.
const DEBOUNCE: Duration = Duration::from_millis(200);
const DEBOUNCE_MAX: Duration = Duration::from_secs(1);

pub struct Options<'a> {
    pub dir: &'a Path,
    pub stdio: bool,
    pub listen: &'a [SocketAddr],
    pub verbose: bool,
}

struct Dirty {
    uuids: HashSet<Uuid>,
    all: bool,
    first: Instant,
    last: Instant,
}

pub fn run(opts: Options) -> io::Result<()> {
    let mut poll = Poll::new()?;
    let mut inotify = Inotify::init()?;
    inotify.watches().add(
        opts.dir,
        WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::MOVED_FROM | WatchMask::DELETE | WatchMask::CREATE | WatchMask::ATTRIB,
    )?;
    let ino_fd = inotify.as_raw_fd();
    poll.registry().register(&mut SourceFd(&ino_fd), INOTIFY, Interest::READABLE)?;

    let started = Instant::now();
    let mut store = Store::open(opts.dir)?;
    log(opts.verbose, format_args!("indexed {} items in {:?}", store.len(), started.elapsed()));

    let mut listeners = Vec::new();
    for (i, addr) in opts.listen.iter().enumerate() {
        let mut l = TcpListener::bind(*addr)?;
        poll.registry().register(&mut l, Token(FIRST_LISTENER + i), Interest::READABLE)?;
        log(opts.verbose, format_args!("listening on {addr}"));
        listeners.push(l);
    }

    let mut conns: HashMap<usize, Conn> = HashMap::new();
    if opts.stdio {
        // SAFETY: fds 0 and 1 are ours for the life of the process; nothing else uses them.
        let (r, w) = unsafe { (File::from_raw_fd(0), File::from_raw_fd(1)) };
        for f in [&r, &w] {
            let fl = rustix::fs::fcntl_getfl(f)?;
            rustix::fs::fcntl_setfl(f, fl | rustix::fs::OFlags::NONBLOCK)?;
        }
        poll.registry().register(&mut SourceFd(&r.as_raw_fd()), STDIN, Interest::READABLE)?;
        poll.registry().register(&mut SourceFd(&w.as_raw_fd()), STDOUT, Interest::WRITABLE)?;
        conns.insert(STDIO_CONN, Conn::new(Io::Stdio { r, w }));
    }

    let mut next_conn = FIRST_CONN;
    let mut dirty: Option<Dirty> = None;
    let mut events = Events::with_capacity(64);
    let mut ino_buf = [0u8; 16 * 1024];

    loop {
        let timeout = dirty.as_ref().map(|d| {
            let due = (d.last + DEBOUNCE).min(d.first + DEBOUNCE_MAX);
            due.saturating_duration_since(Instant::now())
        });
        match poll.poll(&mut events, timeout) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }

        let mut touched: HashSet<usize> = HashSet::new();
        for ev in events.iter() {
            match ev.token() {
                INOTIFY => loop {
                    let evs = match inotify.read_events(&mut ino_buf) {
                        Ok(evs) => evs,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    };
                    let now = Instant::now();
                    let d = dirty.get_or_insert_with(|| Dirty { uuids: HashSet::new(), all: false, first: now, last: now });
                    d.last = now;
                    for e in evs {
                        if e.mask.contains(EventMask::Q_OVERFLOW) {
                            d.all = true;
                        } else if let Some(u) = e.name.and_then(|n| n.to_str()).and_then(uuid_of) {
                            d.uuids.insert(u);
                        }
                    }
                },
                STDIN | STDOUT => {
                    touched.insert(STDIO_CONN);
                }
                Token(t) if (FIRST_LISTENER..FIRST_CONN).contains(&t) => {
                    let l = &listeners[t - FIRST_LISTENER];
                    loop {
                        match l.accept() {
                            Ok((mut s, peer)) => {
                                let _ = s.set_nodelay(true);
                                let tok = next_conn;
                                next_conn += 1;
                                poll.registry().register(&mut s, Token(tok), Interest::READABLE | Interest::WRITABLE)?;
                                log(opts.verbose, format_args!("connection from {peer}"));
                                conns.insert(tok, Conn::new(Io::Tcp(s)));
                                touched.insert(tok);
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) => {
                                log(true, format_args!("accept: {e}"));
                                break;
                            }
                        }
                    }
                }
                Token(t) => {
                    touched.insert(t);
                }
            }
        }

        // Rescan once the burst has settled, then tell watchers.
        if let Some(d) = &dirty {
            let now = Instant::now();
            if now >= d.last + DEBOUNCE || now >= d.first + DEBOUNCE_MAX {
                let d = dirty.take().unwrap();
                let changed = if d.all { store.rescan_all()? } else { store.refresh(d.uuids)? };
                if changed {
                    log(opts.verbose, format_args!("generation {}", store.generation()));
                    let ev = Response::Event { generation: store.generation() };
                    for (&k, c) in conns.iter_mut() {
                        if let Some(id) = c.watch_id {
                            c.send(id, &ev);
                            touched.insert(k);
                        }
                    }
                }
            }
        }

        for k in touched {
            let Some(conn) = conns.get_mut(&k) else { continue };
            let closed = match service(conn, &store) {
                Ok(open) => !open,
                Err(e) => {
                    log(true, format_args!("connection error: {e}"));
                    true
                }
            };
            if closed {
                let mut conn = conns.remove(&k).unwrap();
                if let Io::Tcp(s) = &mut conn.io {
                    let _ = poll.registry().deregister(s);
                }
                if k == STDIO_CONN {
                    return Ok(()); // the ssh session ended
                }
            }
        }
    }
}

/// Read, answer and write for one connection. Ok(false) once it's closed.
fn service(conn: &mut Conn, store: &Store) -> io::Result<bool> {
    let state = conn.fill()?;
    while let Some(frame) = conn.decoder.next_frame().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))? {
        handle(conn, store, frame);
    }
    let drained = conn.flush()?;
    Ok(!matches!(state, ReadState::Eof) || !drained)
}

fn handle(conn: &mut Conn, store: &Store, frame: Frame) {
    let id = frame.req_id;
    let err = |code, message: String| Response::Err { code, message };
    if frame.kind != FrameKind::Msg {
        return conn.send(id, &err(ErrorCode::BadRequest, "expected a message frame".into()));
    }
    let req: Request = match frame.decode() {
        Ok(r) => r,
        Err(e) => return conn.send(id, &err(ErrorCode::BadRequest, e.to_string())),
    };
    match req {
        Request::Hello { proto } => {
            if proto != PROTO_VERSION {
                return conn.send(id, &err(ErrorCode::Unsupported, format!("agent speaks protocol {PROTO_VERSION}, client {proto}")));
            }
            conn.send(
                id,
                &Response::Hello {
                    proto: PROTO_VERSION,
                    agent: format!("re-fsd {}", env!("CARGO_PKG_VERSION")),
                    instance: store.instance(),
                    generation: store.generation(),
                },
            );
        }
        Request::GetIndex { since } => conn.send(id, &store.index(since)),
        Request::Read { uuid, file, offset, len, expect_mtime_ms } => {
            if !matches!(file, FileKind::Pdf | FileKind::Epub | FileKind::Metadata | FileKind::Content) {
                return conn.send(id, &err(ErrorCode::BadRequest, "unreadable file kind".into()));
            }
            match store.open_file(&uuid, file) {
                Ok((f, size, mtime)) => {
                    if expect_mtime_ms.is_some_and(|m| m != mtime) {
                        return conn.send(id, &err(ErrorCode::Changed, format!("mtime is {mtime}")));
                    }
                    let n = size.saturating_sub(offset).min(u64::from(len.min(MAX_READ))) as u32;
                    conn.send_file(id, f, offset, n);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => conn.send(id, &err(ErrorCode::NotFound, format!("{uuid}{}", file.suffix()))),
                Err(e) => conn.send(id, &err(ErrorCode::Io, e.to_string())),
            }
        }
        Request::Watch => {
            conn.watch_id = Some(id);
            conn.send(id, &Response::Event { generation: store.generation() });
        }
    }
}

fn log(on: bool, args: std::fmt::Arguments) {
    if on {
        eprintln!("re-fsd: {args}");
    }
}
