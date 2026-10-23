# Usage

How to set re-fs up, pick a transport, and use the mount day to day. For how it
works underneath, see [Architecture](Architecture) and [Transports and security](Transports-and-Security).

## One-time setup

```sh
nix build .#re-fs                               # the client; it embeds the matching agent
./result/bin/re-fs deploy root@10.11.99.1 --service
```

`deploy --service`, over SSH (here via the USB cable, where the tablet is always
`10.11.99.1`):

- copies the agent to `/home/root/re-fs/re-fsd` on the tablet,
- installs the socket-activated service on port 7745 (systemd starts the agent on the
  first connection; it exits after 5 idle minutes),
- creates the agent's TLS key (`/home/root/re-fs/tls.key`) and pins it on your machine
  in `~/.config/re-fs/tablet.pub`.

Without `--service`, `deploy` only copies the binary (enough for `--ssh`) and stops
a running agent so the next connection starts the new one.

After a reMarkable OS update (which resets `/etc`), run on the tablet:

```sh
sh /home/root/re-fs/install.sh
```

## Choosing a transport

Every transport talks to **re-fsd**, the agent on the tablet; they differ only in the
pipe between your machine and it.

| Flag | Pipe | Encryption | Who may connect |
|---|---|---|---|
| `--usb` | TCP to `10.11.99.1:7745` (only tried while the cable is in) | none (a cable) | anyone on the cable |
| `--tls HOST[:PORT]` | TCP + TLS 1.3, e.g. `--tls tablet.lan` on Wi-Fi | TLS 1.3 | ed25519 keys in the tablet's `authorized_keys` |
| `--tcp HOST[:PORT]` | plain TCP, e.g. `--tcp tablet.vpn` over WireGuard | none (WireGuard's) | trusted addresses only (USB, loopback/wireproxy) |
| `--ssh HOST` | `ssh HOST re-fsd --stdio` | SSH | whoever SSH lets in |

You can give several. Each time re-fs connects it tries them in the order `--usb`,
each `--tls`, each `--tcp`, then `--ssh`, and stays on the one it got until that link
drops; then it reconnects the same way.

`--tls` and `--tcp` take `HOST[:PORT]`: a hostname, IPv4 address, or IPv6 address
(in brackets when a port follows, e.g. `[fe80::1]:7745`). The port is **7745** unless
given. Names are resolved on every connect, so a changed address is picked up when re-fs
reconnects; if a name has several addresses, each is tried in turn.

```sh
re-fs mount ~/rm --usb --tls tablet.lan --ssh root@tablet.vpn
```

### What `--ssh` does

re-fsd is still required with `--ssh`; only the way the client reaches it changes.
With `--ssh root@10.11.99.1`, re-fs runs

```sh
ssh root@10.11.99.1 /home/root/re-fs/re-fsd --stdio
```

That starts a fresh re-fsd on the tablet for this mount, speaking the normal re-fs
protocol over the SSH channel's stdin and stdout. The index, file reads, change events
and `--rw` writes all go through the same messages as over TCP; only the pipe
underneath differs. When you unmount, SSH closes and that re-fsd exits. It's separate
from the socket-activated service on port 7745, and both can run at once.
(`--agent PATH` changes where re-fs looks for the binary.)

### How that compares with sshfs

sshfs talks to the tablet's generic SFTP server and makes a round trip for every file
operation (stat, readdir, read). re-fs over SSH still uses its own agent, so you get
the whole index in one go, browsing from memory, pushed change events, cached reads
and the reMarkable-aware folder tree. SSH is just the encrypted, authenticated pipe.

### `--ssh` versus TCP / TLS

| | `--ssh` | `--usb` / `--tcp` / `--tls` |
|---|---|---|
| On the tablet | the re-fsd binary at `/home/root/re-fs/re-fsd` (`re-fs deploy` puts it there); no service needed | the socket-activated `re-fsd.service` on port 7745 |
| Encryption | SSH (Dropbear) | none over USB/WireGuard; TLS 1.3 for `--tls` |
| Login | your SSH key, checked by Dropbear | trusted address, or a TLS client certificate from your SSH key |
| Connect cost | an SSH handshake each mount: about 0.3–0.4 s on USB/LAN, about 3.5 s over WireGuard | about 0.25 s on USB |
| Reads (measured) | about 18 MiB/s over USB, 13 MiB/s on Wi-Fi | 23 MiB/s plain TCP over USB, 18–23 MiB/s with TLS over USB |

So `--ssh` is the zero-setup fallback: it works anywhere SSH works, even without the
service installed. TCP and TLS are faster to connect and slightly faster to transfer.
Full numbers: [Measurements](Measurements).

### TLS keys

- Your side: `~/.ssh/id_ed25519` (or `--identity FILE`), presented as a client
  certificate. It must be an unrestricted `ssh-ed25519` line in the tablet's
  `/home/root/.ssh/authorized_keys`; removing it there revokes access at once.
  Passphrase-protected keys and ssh-agent aren't supported yet.
- The tablet's side: pinned by `deploy --service`, or again with
  `re-fs pair root@10.11.99.1` (fetched over SSH, which authenticates the tablet).

## Mounting

```sh
re-fs mount MOUNTPOINT [transports…] [options]
```

| Option | Effect |
|---|---|
| `--rw` | allow `mkdir`, copying PDFs/EPUBs in, `mv`, and `rm`/`rmdir` (to the tablet's trash); see [Writes and plans](Writes-and-Plans) |
| `--hide-notebooks` | leave notebooks out |
| `--no-views` | leave out `.originals`, `.rmdoc`, `.recent`, `.pinned`, `.tags` |
| `--cache-dir DIR` | where downloaded and generated files and the saved index live (default `~/.cache/re-fs`) |
| `--cache-mib N` | cache size cap (default 5120) |
| `--identity FILE` | the ed25519 key for `--tls` |
| `--port N` | the agent's port for `--usb` (default 7745) |
| `--agent PATH` | the agent's path on the tablet for `--ssh` |
| `-v` | log connections, index updates, generated files and warnings |

Unmount with `fusermount3 -u ~/rm` or Ctrl-C; the client exits either way. If no
transport answers but a saved index exists, the mount comes up offline: cached files
open, others fail with "Host is down", and re-fs keeps reconnecting in the background.
See [Caching and offline](Caching-and-Offline).

What appears in the mount (each document once, plus generated files and views) is
described in the repository's README and in [Rendering](Rendering).

## Without mounting

```sh
re-fs ls --usb -R                       # the tree as re-fs shows it
re-fs ls --usb -l Books                 # sizes, dates, kinds
re-fs ls --usb --paths -R               # full paths, for scripts
re-fs push a.pdf b.epub --to Books --usb   # copy in; xochitl restarts once at the end
re-fs ls --mirror ./mirror -R           # a local copy of the tablet's files, no tablet needed
```

`ls` and `push` take the same transport flags as `mount`.

## Troubleshooting

| Message | Meaning / fix |
|---|---|
| `no tablet key pinned … run re-fs pair` | `--tls` without a pin: `re-fs pair root@10.11.99.1` |
| `the tablet's TLS key doesn't match the pinned one` | the tablet's key changed (or it's another device); re-pair if expected |
| `connection to the tablet closed` on `--tls` | your key isn't in the tablet's `authorized_keys` (the agent logs why: `journalctl -u re-fsd`) |
| `connection to the tablet closed` on `--tcp` over Wi-Fi | plain TCP is refused from untrusted addresses; use `--tls` |
| `agent speaks protocol X, client Y` | client and agent from different builds: run `re-fs deploy` again |
| `Host is down` when opening a file | offline and that file isn't cached |
| `Invalid argument` copying a file in | only real PDFs and EPUBs can go on the tablet |
