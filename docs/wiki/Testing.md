# Testing

```
nix flake check                                   # tests, tests-rustls, clippy, all three tablet agents
cargo test --workspace                            # unit and integration tests (OpenSSL agent)
cargo test -p re-fsd --no-default-features --features rustls    # transport tests with rustls
cargo test -p re-proto --features json --release -- --ignored --nocapture   # tree build speed on mirror/
tools/check-mirror.sh                             # tree vs an independent Python model
```

## What's covered

| Crate | Tests |
|---|---|
| re-proto | naming (slashes, extensions, clashes, long names), both `.content` formats, tags, the tree (nesting, trash, orphans, cycles, reserved names, stable inodes, views, annotated variants), `diff`, plans, frame codec (whole buffer and one byte at a time, oversized/unknown frames), index compression, Ed25519 cert helpers |
| re-fsd | store scan/refresh/deltas, plan application (order, preconditions, refused kinds), authorized_keys parsing |
| re-render | v5 parsing and truncation, v6 parsing of a real page (Sample), notebook and annotated PDF output, inserted pages, PDFs with a BOM and a truncated `%%EOF`, highlights Markdown |
| re-fs | dates, USB address detection |

## Transmission tests

`crates/re-fsd/tests/transport.rs` runs a real agent in-process (re-fsd is a library
plus a binary) on a temporary document directory and talks to it over real sockets,
once per TLS backend. The tests' TLS client is always rustls (as in re-fs), so the
OpenSSL run also checks that the two interoperate:

| Test | Expectation |
|---|---|
| plain TCP from a trusted address | Hello, index, and a 300 KB file read byte-exact |
| plain TCP from an untrusted address (127.0.0.2) | refused |
| TLS with an authorized key, from untrusted and trusted addresses | works |
| TLS with an unknown client key | refused |
| TLS against a wrong pinned tablet key | refused by the client |

Loopback addresses other than 127.0.0.1 stand in for Wi-Fi peers.

## Golden test against real data

`tools/paths.py` computes the expected paths from a mirror independently of the Rust
code; `tools/check-mirror.sh` diffs it against `re-fs ls`. On the real tablet's data
both gave 172 paths without notebooks and 349 with them, identical.

## Manual checks on the tablet

Done during development, with results recorded in [Measurements](Measurements) and
[Rendering](Rendering):

- every transport, with checksums against the tablet's files;
- refusals: plain TCP from Wi-Fi, unknown client key, wrong tablet pin;
- writes with new items only, then moving an existing annotated notebook out and
  back (stroke file byte-identical);
- renders compared with xochitl's thumbnails (v6 notebook, annotated PDF, inserted
  page, v5 notebook).
