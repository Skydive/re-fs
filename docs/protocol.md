# re-fs wire protocol (version 1)

One byte stream per connection: TCP, or `ssh HOST re-fsd --stdio`. No encryption of
its own; use WireGuard, USB or ssh for that.

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
| `GetIndex { since: Option<(instance, generation)> }` | `Index { instance, generation, full, removed, entries }` |
| `Read { uuid, file, offset, len, expect_mtime_ms }` | a data frame (shorter at EOF), or `Err(Changed / NotFound)` |
| `Watch` | `Event { generation }` now, then on every change |
| `PutChunk { token, offset, data }` | `Ok`. The bytes go to `../.re-fs-staging/<token>` beside xochitl/ |
| `Apply { plan }` | `Applied { generation }`, or `Err(Conflict)` with nothing written |

## Plans

`Plan { expect, ops, restart }`, built by `re_proto::index::plan`:

- `expect`: for each item touched, its `.metadata` (mtime, size) as the client last
  saw it, or `None` for "must not exist". Any mismatch → `Conflict`.
- `ops`: `PutJson { uuid, file: .metadata|.content, json }` and
  `PutStaged { uuid, file: .pdf|.epub, token }`. They are applied data files first,
  then `.content`, then `.metadata`, each via a temporary file and rename, so an item
  appears only once it's complete.
- `restart`: restart xochitl 3 s after the last such plan, only when serving the
  real xochitl directory.

Errors are `Err { code, message }`, where `code` is one of BadRequest, NotFound,
Changed, Io, Unsupported or Conflict.

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
