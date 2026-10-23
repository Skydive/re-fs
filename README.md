<p align="center"><img src="docs/logo.svg" alt="re-fs" width="346"></p>

<p align="center">Your reMarkable 2's documents as an ordinary folder tree.</p>

re-fs mounts a reMarkable 2 with FUSE. A small agent on the tablet serves its
document store; the client keeps the whole index in memory, so browsing runs at
local speed, caches files, renders handwriting to PDF, and turns `mkdir`, `cp`, `mv`
and `rm` into safe edits on the tablet.

- The tablet's real folder tree, live: a change on the tablet shows up in about 0.3 s.
- PDFs and EPUBs as files, cached locally; notebooks rendered to PDF.
- `X.annotated.pdf` with your strokes and highlights drawn on, and `X.highlights.md`.
- Lossless `.rmdoc` copies, plus `.recent`, `.pinned` and `.tags/<tag>` views.
- Copy PDFs/EPUBs in, move, rename, trash (`--rw`), or `re-fs push` without mounting.
- USB, Wi-Fi (TLS 1.3 with your ssh key), WireGuard or ssh; works offline from the cache.

## Quick start

```sh
nix build .#re-fs                                   # client (embeds the matching agent)

./result/bin/re-fs deploy root@10.11.99.1 --service # once, over USB: agent, systemd
                                                    # socket, TLS key; pins the tablet key
./result/bin/re-fs mount ~/rm --usb --tls 192.168.1.50:7745 --ssh root@tablet.vpn
./result/bin/re-fs mount ~/rm --usb --rw            # with writes
./result/bin/re-fs ls --usb -l Books                # list without mounting
./result/bin/re-fs push paper.pdf --to Books --usb  # copy in without mounting
./result/bin/re-fs pair root@10.11.99.1             # re-pin the tablet's TLS key
fusermount3 -u ~/rm                                 # unmount (the client exits)
```

Transports are tried in this order on each connect, and re-fs stays on the one it got
until it drops: `--usb` (only while the cable is in), each `--tls ADDR`, each
`--tcp ADDR`, then `--ssh HOST`.

## What's in the mount

```
~/rm/Books/Paper.pdf               the PDF as imported
~/rm/Books/Paper.annotated.pdf     … with your strokes and highlights drawn on
~/rm/Books/Paper.highlights.md     … highlights as Markdown
~/rm/Books/Novel.epub              an EPUB
~/rm/Notes.pdf                     a notebook, rendered
~/rm/.trash/                       the tablet's trash
~/rm/.originals/…                  the folders again, original PDFs/EPUBs only
~/rm/.rmdoc/…                      every document as .rmdoc (lossless, with annotations)
~/rm/.recent/                      last 50 opened (`ls -lt`)
~/rm/.pinned/                      favourites
~/rm/.tags/<tag>/                  documents by tag
```

Generated files (`*.annotated.pdf`, notebooks, `.rmdoc`, `.highlights.md`) are made
the first time you open them and cached until the document changes; they show as 0
bytes until then. `getfattr -d FILE` shows `user.rm.pages`, `last_page`, `tags`,
`pinned` and `uuid`.

With `--rw`: `mkdir` makes a folder, copying a PDF/EPUB imports it, `mv` moves or
renames, `rm`/`rmdir` move to the tablet's trash. Existing documents can't be
overwritten, and nothing is deleted for good. xochitl restarts once, 3 s after the
last write, so the tablet shows the changes.

## Transports and security

Every transport talks to **re-fsd**, the agent on the tablet; they differ only in the
pipe. Give several and re-fs tries them in the order `--usb` (only while the cable is
in), each `--tls`, each `--tcp`, then `--ssh`, staying on the one it got until it drops.

| Flag | Pipe | Encryption | Who may connect |
|---|---|---|---|
| `--usb` | TCP to `10.11.99.1:7745` | none (a cable) | anyone on the cable |
| `--tls ADDR` | TCP + TLS 1.3 (Wi-Fi) | TLS 1.3 | ed25519 keys in the tablet's `authorized_keys` |
| `--tcp ADDR` | plain TCP (e.g. `10.0.0.5:7745` over WireGuard) | none (WireGuard's) | trusted addresses only (USB, loopback/wireproxy) |
| `--ssh HOST` | `ssh HOST re-fsd --stdio` | SSH | whoever SSH lets in |

Over TLS both identities are ed25519 keys: re-fs presents a certificate for
`~/.ssh/id_ed25519` (or `--identity`), which the tablet checks against
`/home/root/.ssh/authorized_keys`; re-fs accepts only the tablet key pinned in
`~/.config/re-fs/tablet.pub`. Plain TCP is refused from Wi-Fi. On the tablet, systemd
starts the agent on the first connection and it exits after 5 idle minutes. After a
reMarkable OS update, run `sh /home/root/re-fs/install.sh`.

### What `--ssh` does

re-fsd is still required with `--ssh`; only the way the client reaches it changes.
With `--ssh root@10.11.99.1`, re-fs runs

```sh
ssh root@10.11.99.1 /home/root/re-fs/re-fsd --stdio
```

That starts a fresh re-fsd on the tablet for this mount, speaking the normal re-fs
protocol over the SSH channel's stdin and stdout. The index, file reads, change events
and `--rw` writes all go through the same messages as over TCP. When you unmount, SSH
closes and that re-fsd exits. It's separate from the socket-activated service on port
7745, and both can run at once.

Unlike sshfs, which talks to the tablet's generic SFTP server and makes a round trip
for every stat, readdir and read, re-fs over SSH still uses its own agent: the whole
index in one go, browsing from memory, pushed change events, cached reads and the
reMarkable-aware folder tree. SSH is just the encrypted, authenticated pipe.

| | `--ssh` | `--usb` / `--tcp` / `--tls` |
|---|---|---|
| On the tablet | the re-fsd binary at `/home/root/re-fs/re-fsd` (`re-fs deploy` puts it there); no service needed | the socket-activated `re-fsd.service` on port 7745 |
| Encryption | SSH (Dropbear) | none over USB/WireGuard; TLS 1.3 for `--tls` |
| Login | your SSH key, checked by Dropbear | trusted address, or a TLS client certificate from your SSH key |
| Connect cost | an SSH handshake each mount: about 0.3–0.4 s on USB/LAN, about 3.5 s over WireGuard | about 0.25 s on USB |
| Reads (measured) | about 18 MiB/s over USB, 13 MiB/s on Wi-Fi | 23 MiB/s plain TCP over USB, 18–23 MiB/s with TLS over USB |

So `--ssh` is the zero-setup fallback: it works anywhere SSH works, even without the
service installed. TCP and TLS are faster to connect and slightly faster to transfer.
The full usage guide (setup, every option, troubleshooting) is
[docs/wiki/Usage.md](docs/wiki/Usage.md).

## Building

```sh
nix build .#re-fs                  # client; `deploy` installs the OpenSSL agent
nix build .#re-fs-rustls-agent     # client; `deploy` installs the static rustls agent
nix build .#re-fsd-armv7           # agent with the tablet's OpenSSL (default): 788 KB, dynamic glibc
nix build .#re-fsd-armv7-rustls    # agent with rustls + ring: 1.79 MB, static musl
nix flake check                    # tests (both TLS backends), clippy, both agents
nix develop                        # cargo, rust-analyzer
```

The agent's TLS backend is a cargo feature: `openssl` (default; links the tablet's
`/usr/lib/libssl.so.3`, OpenSSL 3.5.6) or `rustls` (`--no-default-features
--features rustls`). The rustls build cross-compiles ring's C with nixpkgs' cached
ARMv7 GCC against musl headers and links everything with rust-lld.

## Measured on a reMarkable 2

| | `ls -R` | Mount | 16 MB PDF |
|---|---|---|---|
| TCP over USB | 0.23 s | 0.24–0.30 s | 23 MiB/s |
| TLS over USB (OpenSSL / rustls) | 0.26 s | 0.30 s | 23 / 19 MiB/s |
| TLS over Wi-Fi | 0.40 s | 0.35 s | 10–17 MiB/s (radio-bound) |
| ssh over Wi-Fi LAN | 0.76 s | 0.70 s | 13 MiB/s |
| TCP over WireGuard | 3.1 s | 3.3 s | 2 MiB/s |

Browsing the mounted tree takes 7 ms; a cached 16 MB PDF re-reads in 16 ms; a write
returns in 40–100 ms over the LAN. More in [Measurements](docs/wiki/Measurements.md).

## Status

| | |
|---|---|
| Folder tree, live updates, PDF/EPUB reads, cache, offline mount | done |
| Writes (mkdir, copy in, move, trash), `push` | done |
| TCP over USB, TLS over Wi-Fi, WireGuard, socket-activated service | done |
| Notebooks, annotated PDFs, highlights, `.rmdoc`, views, xattrs | done |
| OpenSSL / rustls TLS backends | done |
| Page templates, typed text | not yet |
| ssh-agent / passphrase-protected keys | not yet |
| `.rmdoc` import, trash restore/purge, home-manager module | not yet |

## Documentation

- [Usage guide](docs/wiki/Usage.md): setup, transports (what `--ssh` does), every option, troubleshooting
- [reMarkable storage format](docs/wiki/Remarkable-Storage-Format.md): how the tablet stores everything
- [Architecture](docs/wiki/Architecture.md) · [Wire protocol](docs/wiki/Wire-Protocol.md) · [Transports and security](docs/wiki/Transports-and-Security.md)
- [Rendering](docs/wiki/Rendering.md) · [Writes and plans](docs/wiki/Writes-and-Plans.md) · [Caching and offline](docs/wiki/Caching-and-Offline.md)
- [Build and cross-compilation](docs/wiki/Build-and-Cross-Compilation.md) · [Testing](docs/wiki/Testing.md)
- [Design decisions](docs/wiki/Design-Decisions.md): every choice and why
- Short references: [docs/format.md](docs/format.md), [docs/protocol.md](docs/protocol.md)

### Working without the tablet

`mirror/` (gitignored) can hold a copy of the tablet's `.metadata`/`.content` files
plus sparse files with each PDF/EPUB's real size and mtime:

```sh
mkdir mirror
ssh root@10.11.99.1 'cd /home/root/.local/share/remarkable/xochitl && tar cf - *.metadata *.content' | tar xf - -C mirror
ssh root@10.11.99.1 'cd /home/root/.local/share/remarkable/xochitl && stat -c "%n %s %Y" *.pdf *.epub' |
  while read -r n s t; do truncate -s "$s" "mirror/$n"; touch -d "@$t" "mirror/$n"; done

cargo run -p re-fs -- ls --mirror mirror -R
tools/check-mirror.sh                     # compare with an independent Python model
```

A native agent serves a copy of it for full mount tests:
`re-fsd --dir <copy> --listen 127.0.0.1:7746`, then `re-fs mount ~/rm --tcp 127.0.0.1:7746`.
