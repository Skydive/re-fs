# Wire protocol (version 7)

One byte stream per connection: plain TCP, TLS 1.3, or `ssh HOST re-fsd --stdio`. The
protocol inside is the same on all of them.

## Frames

```
len: u32 LE | kind: u8 | req_id: u32 LE | payload (len bytes)
```

- kind 1, **message**: postcard-encoded `Request` or `Response`.
- kind 2, **data**: raw file bytes answering a read. The agent writes the 9-byte
  header and then `sendfile`s the range (plain links) or streams it through the TLS session.

Payloads are capped at 64 MiB; one read returns at most 8 MiB. Replies carry the
request's id, so requests are pipelined and may be answered out of order. Events
after `Watch` carry the Watch's id. `FrameDecoder` is sans-io: feed bytes in, get
whole frames out; tests feed it one byte at a time.

## Requests

| Request | Reply |
|---|---|
| `Hello { proto }` | `Hello { proto, agent, instance, generation }` |
| `GetIndex { since: Option<(instance, generation)> }` | `Index { instance, generation, full, removed, entries }` |
| `Read { uuid, file, offset, len, expect_mtime_ms }` | data frame (shorter at EOF), or `Err(Changed / NotFound)` |
| `ReadPage { uuid, name, offset, len, expect_mtime_ms }` | the same, for a file in `<uuid>/` |
| `Watch` | `Event { generation }` now, then on every change |
| `PutChunk { token, offset, data }` | `Ok`; stored in the staging area beside `xochitl/` |
| `Apply { plan }` | `Applied { generation }`, or `Err(Conflict)` with nothing written |

Errors are `Err { code, message }` with code BadRequest, NotFound, Changed, Io,
Unsupported, Conflict or Unauthorized.

## The index

`entries` is lz4 (size-prepended) over postcard `Vec<Entry>`. The agent sends the
raw JSON bytes; it never parses them. The full index of 380 items is ~10 MB of JSON
and ~1.3 MB on the wire.

`instance` changes every time the agent starts; `generation` goes up with every
change. A `GetIndex` with a `since` from the same instance gets a delta: entries
changed after that generation plus removed UUIDs. A rename on the tablet arrived as a
365-byte delta.

```mermaid
sequenceDiagram
  participant C as re-fs
  participant A as re-fsd
  C->>A: Hello{proto: 7}
  A-->>C: Hello{instance, generation}
  C->>A: Watch
  A-->>C: Event{gen 7}
  C->>A: GetIndex{since: None}
  A-->>C: Index{full, entries (lz4)}
  Note over A: inotify: a .metadata changed<br/>200 ms later: rescan, gen 8
  A-->>C: Event{gen 8}
  C->>A: GetIndex{since: (instance, 7)}
  A-->>C: Index{delta: 1 entry}
  Note over C: rebuild tree, diff,<br/>invalidate kernel caches
```

## Reading a PDF

```mermaid
sequenceDiagram
  participant P as cat
  participant K as kernel
  participant F as re-fs (fuser)
  participant T as re-fs (tokio)
  participant A as re-fsd
  P->>K: open Books/x.pdf
  K->>F: lookup ×3 (tree, no network)
  K->>F: open
  F->>T: prefetch whole file
  T->>A: Read{0, 4 MiB} … ×4 in flight
  P->>K: read
  K->>F: read(offset, size)
  F->>T: wait until range is on disk
  A-->>T: data frames (sendfile)
  T-->>K: reply.data
  Note over T: done: partial → objects/<uuid>.pdf@<mtime>
  P->>K: read again later
  K->>F: read
  F-->>K: pread from cache (fuser thread)
```

## Versions

| Version | Change |
|---|---|
| 1 | first protocol |
| 2 | Hello carried a login challenge; `Auth` (removed in 4) |
| 3 | page files in the index; `ReadPage`; `.pagedata` |
| 4 | Wi-Fi uses TLS; the challenge login is gone |
| 5 | `ReadPages`: several page files in one round trip |
| 6 | the client picks the codec (none, lz4, zstd) per request; replies are chunked |
| 7 | `PutStagedPage` and `.pagedata` in `PutJson`: importing whole documents (`.rmdoc`, or a PDF re-fs made) |
