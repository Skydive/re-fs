# re-fs

A FUSE filesystem that shows a reMarkable 2's documents as their real folder tree.

```
re-fs deploy root@10.11.99.1 --service               # once: agent + tablet service (TCP 7745)
re-fs mount ~/rm --usb --ssh root@tablet.vpn      # USB if plugged in, else ssh
re-fs deploy root@tablet.vpn                      # just copy re-fsd (ssh-only use)
re-fs mount ~/rm --ssh root@tablet.vpn -v         # folder tree; PDFs and EPUBs readable
re-fs mount ~/rm --ssh root@tablet.vpn --rw       # also mkdir, cp in, mv, rm
re-fs ls --ssh root@tablet.vpn -l Books           # same tree, no mount
re-fs push paper.pdf --to Books --usb                # copy in without mounting
```

Four crates:

- **re-proto**: the reMarkable format, the folder tree, naming rules and (soon) the wire
  protocol. No I/O.
- **re-fsd**: the tablet agent. A static armv7 binary that serves the index and file
  bytes over TCP or `ssh … re-fsd --stdio` and pushes change events.
- **re-fs**: the client. FUSE mount, local cache, transports, CLI.
- **re-render**: `.rm` strokes (v5, v6) to PDF and Markdown. Pure.

Format notes: [docs/format.md](docs/format.md). Protocol: [docs/protocol.md](docs/protocol.md).

## Status

| Milestone | State |
|---|---|
| M0 workspace, flake, armv7 cross-build | done |
| M1 index, tree and naming; `re-fs ls` on a mirror | done |
| M2 re-fsd agent + read-only FUSE folder tree | done |
| M3 PDF/EPUB reads, cache, offline | done |
| M4 writes (mkdir, copy in, move, trash) | done |
| M5 TCP over USB / Wi-Fi (ssh-key login) / WireGuard, tablet service | done |
| M6/M7 notebooks, annotated PDFs, highlights, .rmdoc, views, xattrs, push | done |
| M9 TLS for Wi-Fi (ssh keys as TLS identities) | done |

## Transports

`--usb`, `--tcp ADDR` (repeatable) and `--ssh HOST` can be combined. Each time
re-fs connects it tries them in that order, skipping `--usb` unless a reMarkable USB
network interface is up (a USB NIC with a 10.11.99.x address). It stays on whichever
link it got until that link drops.

The tablet service is socket-activated (`deploy/re-fsd.socket` and
`re-fsd.service`, installed by `re-fs deploy --service`): systemd holds port 7745,
starts re-fsd on the first connection, and re-fsd exits after 5 minutes with no
clients, so nothing runs on the tablet while you're not using it. Clients that reach it on 10.11.99.1 (USB) or 127.0.0.1 (where
wireproxy's tunnels end) may use plain TCP. Anyone else (Wi-Fi) must use TLS 1.3
(`--tls 192.168.1.50:7745`; rustls with ring), where both identities are ed25519
keys:

- re-fs presents a self-signed client certificate for `~/.ssh/id_ed25519` (or
  `--identity`); the agent accepts it only if that key is an unrestricted
  ssh-ed25519 line in `/home/root/.ssh/authorized_keys` (re-read per handshake).
- The agent's TLS key lives in `/home/root/re-fs/tls.key`. re-fs accepts only the
  key pinned in `~/.config/re-fs/tablet.pub`, fetched over ssh by `re-fs pair HOST`
  (and by `re-fs deploy HOST --service`).

The agent tells TLS from plain TCP by the first byte, so both share port 7745.
Passphrase-protected keys and ssh-agent aren't supported yet. After a reMarkable OS update, run
`sh /home/root/re-fs/install.sh` (or add it to `/home/root/wireproxy/reinstall.sh`).

For WireGuard, add to `/home/root/wireproxy/wireproxy.conf` and restart wireproxy:

```
# re-fs agent: re-fs mount ~/rm --tcp 10.0.0.5:7745
[TCPServerTunnel]
ListenPort = 7745
Target = 127.0.0.1:7745
```

## What's in the mount

```
~/rm/Books/Paper.pdf               the PDF as imported
~/rm/Books/Paper.annotated.pdf     … with your strokes and highlights drawn on (if annotated)
~/rm/Books/Paper.highlights.md     … highlights as Markdown (PDF/EPUB with annotations)
~/rm/Books/Novel.epub              an EPUB
~/rm/Notes.pdf                     a notebook, rendered
~/rm/.trash/                       the tablet's trash
~/rm/.originals/…                  the folders again, original PDFs/EPUBs only
~/rm/.rmdoc/…                      the folders again, every document as .rmdoc (lossless, with annotations)
~/rm/.recent/                      last 50 opened (`ls -lt`: mtime = when last opened)
~/rm/.pinned/                      favourites
~/rm/.tags/<tag>/                  documents by tag
```

Strokes on PDF pages are placed as the tablet stores them: 226 dpi, x from the
page's centre, y from its top (checked against xochitl's thumbnails). Notebook and
inserted pages grow to hold strokes drawn beyond the screen. PDFs that viewers
repair silently (a BOM before `%PDF`, a truncated `%%EOF`, a broken xref table) are
repaired the same way before annotating. Not drawn yet: page templates, typed text.

Files marked as made by re-fs (`*.annotated.pdf`, notebooks, `.rmdoc`, `.highlights.md`)
are made the first time they're opened and cached until the document changes. Their
size shows as 0 until then. `getfattr -d FILE` shows `user.rm.pages`, `user.rm.last_page`,
`user.rm.tags`, `user.rm.pinned` and `user.rm.uuid`. With `--rw`, only the documents
themselves (`X.pdf`, `X.epub`, a notebook's `X.pdf`) and the main folders can be changed.

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
- A document's extension shows its type and isn't stored: `mv x.pdf y.pdf` names it
  "y". Renaming to another extension keeps that text in the name (`mv x.pdf notes.epub`
  shows as `notes.epub.pdf`), since the file is still a PDF.
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
nix build .#re-fsd-armv7     # tablet agent: static armv7 musl, linked with rust-lld;
                             # ring's C/asm built by nixpkgs' cached ARMv7 GCC (musl headers)
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
| first read of a PDF/EPUB | | 12–13 MiB/s (ssh); 10–15 MiB/s (TCP) |

| TCP transport (2026-10-23) | `ls -R` | mount | 16 MB PDF |
|---|---|---|---|
| USB | 0.23 s | 0.24 s | 23 MiB/s |
| USB, TLS | 0.26 s | 0.30 s | 18 MiB/s |
| Wi-Fi LAN, TLS | 0.40 s | 0.36 s | 10 MiB/s (radio-bound) |
| WireGuard (via wireproxy and the hub) | 3.1 s | 3.1 s | 2–3 MiB/s |
| re-read from cache (16 MB PDF) | 16 ms | 16 ms |
