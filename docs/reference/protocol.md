# re-fs wire protocol (version 7)

A one-page summary for working on the code; the full description is
[Wire protocol](../internals/wire-protocol.md). One byte stream per connection: plain
TCP, TLS 1.3, or `ssh HOST re-fsd --stdio`; the protocol inside is the same on all.

## Frames

```
len: u32 LE | kind: u8 | req_id: u32 LE | payload (len bytes)
```

- kind 1, **message**: a postcard-encoded `Request` (client → agent) or `Response`.
- kind 2, **data**: raw file bytes answering a `Read`. The agent writes the header and
  then `sendfile`s the range.

Payloads are capped at 64 MiB; a single `Read` returns at most 8 MiB. Replies carry the
request's `req_id`, so requests can be pipelined and answered out of order. Events
after a `Watch` carry the Watch's `req_id`.

## Requests

| Request | Reply |
|---|---|
| `Hello { proto }` | `Hello { proto, agent, instance, generation }` |
| `ReadPage { uuid, name, offset, len, expect_mtime_ms }` | like `Read`, for files in `<uuid>/` |
| `GetIndex { since: Option<(instance, generation)> }` | `Index { instance, generation, full, removed, entries }` |
| `Read { uuid, file, offset, len, expect_mtime_ms }` | a data frame (shorter at EOF), or `Err(Changed / NotFound)` |
| `Watch` | `Event { generation }` now, then on every change |
| `PutChunk { token, offset, data }` | `Ok`. The bytes go to `../.re-fs-staging/<token>` beside xochitl/ |
| `Apply { plan }` | `Applied { generation }`, or `Err(Conflict)` with nothing written |

## Plans

`Plan { expect, ops, restart }`, built by `re_proto::index::plan`:

- `expect`: for each item touched, its `.metadata` (mtime, size) as the client last
  saw it, or `None` for "must not exist". Any mismatch → `Conflict`.
- `ops`: `PutJson { uuid, file: .metadata|.content|.pagedata, json }`,
  `PutStaged { uuid, file: .pdf|.epub, token }` and
  `PutStagedPage { uuid, name, token }` (into the page folder `<uuid>/`, which is
  created; `name` must be a page file, `<page-uuid>.rm` or `<page-uuid>-metadata.json`,
  or the whole plan is refused). They are applied data and page files first, then
  `.content` and `.pagedata`, then `.metadata`, each via a temporary file and rename,
  so an item appears only once it's complete.
- `restart`: restart xochitl 3 s after the last such plan, only when serving the
  real xochitl directory.

Errors are `Err { code, message }`, where `code` is one of BadRequest, NotFound,
Changed, Io, Unsupported, Conflict or Unauthorized.

## Compatibility

Client and agent must speak the same version: `Hello` with any other `proto` is
answered `Err(Unsupported)`, so upgrade both together (`re-fs deploy`). Version 7
added `PutStagedPage` and `.pagedata` in `PutJson`, for importing whole documents;
a version-6 agent can't decode these plans, which is why the version changed.

## Transport security

Plain TCP is accepted only from addresses the agent trusts (`--allow-local`: USB,
loopback). Other clients must speak TLS 1.3 on the same port (the agent tells them
apart by the first byte, 0x16 for a ClientHello). Both sides authenticate with
self-signed certificates for ed25519 keys: the client's key must be in the tablet's
`authorized_keys`; the agent's key must match the one the client pinned. The
protocol inside is unchanged.

## Index

`entries` is lz4 (size-prepended) over postcard `Vec<Entry>`. Each `Entry` holds the
raw `.metadata` and `.content` bytes plus size and mtime for `.metadata`, `.content`,
`.pdf` and `.epub`. The agent never parses the JSON; the client builds the tree.

`instance` is new each time the agent starts, and `generation` goes up on every change
it sees. With `since` from the same instance the reply is a delta (`full: false`):
entries changed after that generation, plus `removed` UUIDs. Otherwise it's the
whole index.

## Change detection

re-fsd watches the xochitl directory with inotify (close-write, moves, create,
delete, attrib). It rescans the touched UUIDs 200 ms after the last event of a
burst, and at most 1 s after the first. A queue overflow triggers a full rescan.
