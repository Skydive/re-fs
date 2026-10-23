//! A pipelined protocol client: any number of requests in flight on one link,
//! replies routed back by request id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use re_proto::wire::{encode_msg, ErrorCode, FrameDecoder, FrameKind, Plan, Request, Response};
use re_proto::index::FileKind;
use re_proto::PROTO_VERSION;
use uuid::Uuid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};

use crate::transport::Link;

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("connection to the tablet closed")]
    Closed,
    #[error("tablet: {code:?}: {message}")]
    Remote { code: ErrorCode, message: String },
    #[error("unexpected reply from the tablet")]
    Unexpected,
}

pub enum Reply {
    Msg(Response),
    Data(Vec<u8>),
}

enum Sink {
    Once(oneshot::Sender<Reply>),
    Stream(mpsc::UnboundedSender<Response>),
}

struct Inner {
    next_id: AtomicU32,
    pending: Mutex<HashMap<u32, Sink>>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    closed: watch::Receiver<bool>,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    /// Start the reader and writer tasks for `link`.
    pub fn start(link: Link) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (closed_tx, closed) = watch::channel(false);
        let inner = Arc::new(Inner { next_id: AtomicU32::new(1), pending: Mutex::new(HashMap::new()), tx, closed });
        let Link { mut reader, mut writer, child } = link;

        let closed_w = closed_tx.clone();
        tokio::spawn(async move {
            while let Some(buf) = rx.recv().await {
                if writer.write_all(&buf).await.is_err() {
                    break;
                }
                // Coalesce whatever else is queued before flushing.
                let mut ok = true;
                while let Ok(more) = rx.try_recv() {
                    if writer.write_all(&more).await.is_err() {
                        ok = false;
                        break;
                    }
                }
                if !ok || writer.flush().await.is_err() {
                    break;
                }
            }
            let _ = closed_w.send(true);
        });

        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let _child = child; // dropping it kills ssh
            let mut dec = FrameDecoder::new();
            let mut buf = vec![0u8; 256 * 1024];
            'read: loop {
                let n = match reader.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                dec.feed(&buf[..n]);
                loop {
                    let frame = match dec.next_frame() {
                        Ok(Some(f)) => f,
                        Ok(None) => break,
                        Err(e) => {
                            eprintln!("re-fs: protocol error: {e}");
                            break 'read;
                        }
                    };
                    let Some(inner) = weak.upgrade() else { break 'read };
                    let mut pending = inner.pending.lock().unwrap();
                    let reply = match frame.kind {
                        FrameKind::Data => Reply::Data(frame.payload),
                        FrameKind::Msg => match frame.decode::<Response>() {
                            Ok(r) => Reply::Msg(r),
                            Err(e) => {
                                eprintln!("re-fs: bad message: {e}");
                                break 'read;
                            }
                        },
                    };
                    match (pending.get(&frame.req_id), reply) {
                        (Some(Sink::Stream(s)), Reply::Msg(r)) => {
                            let _ = s.send(r);
                        }
                        (Some(Sink::Once(_)), reply) => {
                            if let Some(Sink::Once(s)) = pending.remove(&frame.req_id) {
                                let _ = s.send(reply);
                            }
                        }
                        _ => {} // reply to something nobody waits for any more
                    }
                }
            }
            if let Some(inner) = weak.upgrade() {
                inner.pending.lock().unwrap().clear(); // wakes every waiter with Closed
            }
            let _ = closed_tx.send(true);
        });

        Client { inner }
    }

    fn submit(&self, req: &Request, sink: Sink) -> Result<u32, CallError> {
        if *self.inner.closed.borrow() {
            return Err(CallError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.pending.lock().unwrap().insert(id, sink);
        let mut buf = Vec::new();
        encode_msg(&mut buf, id, req);
        self.inner.tx.send(buf).map_err(|_| CallError::Closed)?;
        Ok(id)
    }

    pub async fn call(&self, req: &Request) -> Result<Reply, CallError> {
        let (tx, rx) = oneshot::channel();
        self.submit(req, Sink::Once(tx))?;
        match rx.await.map_err(|_| CallError::Closed)? {
            Reply::Msg(Response::Err { code, message }) => Err(CallError::Remote { code, message }),
            r => Ok(r),
        }
    }

    pub async fn call_msg(&self, req: &Request) -> Result<Response, CallError> {
        match self.call(req).await? {
            Reply::Msg(r) => Ok(r),
            Reply::Data(_) => Err(CallError::Unexpected),
        }
    }

    /// Bytes of a tablet file; shorter than `len` only at EOF.
    pub async fn read(&self, uuid: Uuid, file: FileKind, offset: u64, len: u32, expect_mtime_ms: Option<i64>) -> Result<Vec<u8>, CallError> {
        match self.call(&Request::Read { uuid, file, offset, len, expect_mtime_ms }).await? {
            Reply::Data(d) => Ok(d),
            Reply::Msg(_) => Err(CallError::Unexpected),
        }
    }

    /// Bytes of a page-folder file.
    pub async fn read_page(&self, uuid: Uuid, name: &str, offset: u64, len: u32) -> Result<Vec<u8>, CallError> {
        match self.call(&Request::ReadPage { uuid, name: name.to_owned(), offset, len, expect_mtime_ms: None }).await? {
            Reply::Data(d) => Ok(d),
            Reply::Msg(_) => Err(CallError::Unexpected),
        }
    }

    pub async fn put_chunk(&self, token: u64, offset: u64, data: Vec<u8>) -> Result<(), CallError> {
        match self.call_msg(&Request::PutChunk { token, offset, data }).await? {
            Response::Ok => Ok(()),
            _ => Err(CallError::Unexpected),
        }
    }

    /// Apply a plan; returns the generation that includes it.
    pub async fn apply(&self, plan: Plan) -> Result<u64, CallError> {
        match self.call_msg(&Request::Apply { plan }).await? {
            Response::Applied { generation } => Ok(generation),
            _ => Err(CallError::Unexpected),
        }
    }

    /// Subscribe to change events.
    pub fn watch(&self) -> Result<mpsc::UnboundedReceiver<Response>, CallError> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.submit(&Request::Watch, Sink::Stream(tx))?;
        Ok(rx)
    }

    pub async fn closed(&self) {
        let mut c = self.inner.closed.clone();
        let _ = c.wait_for(|&closed| closed).await;
    }

    /// Returns (agent name, instance, generation).
    pub async fn hello(&self) -> anyhow::Result<(String, u64, u64)> {
        let Response::Hello { agent, instance, generation, .. } = self.call_msg(&Request::Hello { proto: PROTO_VERSION }).await? else {
            anyhow::bail!("unexpected reply to Hello");
        };
        Ok((agent, instance, generation))
    }
}
