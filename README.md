# re-fs

A FUSE filesystem that shows a reMarkable 2's documents as their real folder tree.

```
re-fs deploy root@tablet.vpn                      # once: copy re-fsd to the tablet
re-fs mount ~/rm --ssh root@tablet.vpn -v         # folder tree; PDFs and EPUBs readable
re-fs mount ~/rm --ssh root@tablet.vpn --rw       # also mkdir, cp in, mv, rm
re-fs ls --ssh root@tablet.vpn -l Books           # same tree, no mount
```

Three crates:

- **re-proto**: the reMarkable format, the folder tree, naming rules and (soon) the wire
  protocol. No I/O.
- **re-fsd**: the tablet agent. A static armv7 binary that serves the index and file
  bytes over TCP or `ssh … re-fsd --stdio` and pushes change events.
- **re-fs**: the client. FUSE mount, local cache, transports, CLI.

Format notes: [docs/format.md](docs/format.md). Protocol: [docs/protocol.md](docs/protocol.md).

## Status

| Milestone | State |
|---|---|
| M0 workspace, flake, armv7 cross-build | done |
| M1 index, tree and naming; `re-fs ls` on a mirror | done |
| M2 re-fsd agent + read-only FUSE folder tree | done |
| M3 PDF/EPUB reads, cache, offline | done |
| M4 writes (mkdir, copy in, move, trash) | done |
| M5 USB / LAN / WireGuard transports, tablet service | next |

## Reading files

The first open of a PDF or EPUB starts a background download of the whole file into
`~/.cache/re-fs/objects/` (`--cache-dir`, capped by `--cache-mib`, default 5 GiB, least
recently used first). Reads wait only for their own range; a read far past the
download front is fetched on its own. Later opens read the local copy, and the kernel
keeps its page cache until the file changes on the tablet.

The last index is kept in `~/.cache/re-fs/index.zstd`. If the tablet can't be reached,
`re-fs mount` mounts that index read-only: cached files open, others fail with
"Host is down". It reconnects in the background (backoff up to 30 s).

## Writing (`--rw`)

| On your machine | On the tablet |
|---|---|
| `mkdir A` | new folder (`.metadata` + `.content`) |
| `cp x.pdf A/`, `cp y.epub A/` | new document: the file, a `.content`, then the `.metadata` |
| `mv A/x.pdf B/y.pdf` | `.metadata`: new `parent` and `visibleName` |
| `rm A/x.pdf`, `rmdir A` | `.metadata`: `parent = "trash"` (restore from the tablet's trash) |

- Only PDFs and EPUBs can be copied in. The file's contents are checked, not just
  its name. Anything else fails with "Invalid argument".
- Existing documents can't be overwritten or edited in place (that would misalign
  their annotations). Copy in a new one instead.
- A document's extension is its type: `mv x.pdf x.epub` is refused.
- `rmdir` refuses folders that still hold anything, including notebooks that
  aren't shown yet.
- Items already in `.trash` can't be deleted for good from here.
- Each write is applied atomically by the agent and refused if the tablet changed
  the item first. A write returns once the mount shows its result: about 10 ms
  locally, 40–100 ms over ssh on the LAN.
- xochitl restarts once, 3 s after the last write, so the tablet's UI picks up the
  changes.

## Building

```
nix build .#re-fs            # client (its `deploy` installs the matching agent)
nix build .#re-fsd-armv7     # tablet agent: static armv7 musl, linked with rust-lld
nix flake check              # tests, clippy, armv7 build
nix develop                  # cargo, rust-analyzer
```

## Trying the tree without the tablet

`mirror/` (gitignored) holds a copy of the tablet's `.metadata`/`.content` files, plus
sparse files with the real sizes and mtimes of each `.pdf`/`.epub`:

```
mkdir mirror
ssh root@tablet.vpn 'cd /home/root/.local/share/remarkable/xochitl && tar cf - *.metadata *.content' | tar xf - -C mirror
ssh root@tablet.vpn 'cd /home/root/.local/share/remarkable/xochitl && stat -c "%n %s %Y" *.pdf *.epub' |
  while read -r n s t; do truncate -s "$s" "mirror/$n"; touch -d "@$t" "mirror/$n"; done

cargo run -p re-fs -- ls --mirror mirror -R          # tree
cargo run -p re-fs -- ls --mirror mirror -l Books    # sizes, dates, kinds
tools/check-mirror.sh                                # compare with tools/paths.py
```

`tools/` also has the Python scripts used to survey the format.

## Measured (2026-10-23, reMarkable 2)

| | over `.jc` WireGuard | over LAN |
|---|---|---|
| ssh connect | 3.5 s | 0.4 s |
| agent indexes 380 items | 0.1 s | 0.1 s |
| full index transfer (1.3 MB lz4) | 2.1 s | ~0.3 s |
| `find` over the mounted tree | 7 ms | 7 ms |
| a change on the tablet shows in the mount | ~0.3 s (200 ms is debounce) | |
| first read of a PDF/EPUB | | 12–13 MiB/s (ssh) |
| re-read from cache (16 MB PDF) | 16 ms | 16 ms |
