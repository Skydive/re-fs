# Connecting to the tablet

re-fs reaches the agent on the tablet over USB, Wi-Fi (TLS), a VPN or ssh. This
page explains how to pick a transport and set up Wi-Fi keys. The same transport
flags work for `mount`, `ls` and `push`.

## Transports

Every transport talks to `re-fsd`, the agent on the tablet; they differ only in the
pipe between your machine and it.

| Flag | Pipe | Encryption | Who may connect |
|---|---|---|---|
| `--usb` | TCP to `10.11.99.1:7745`, tried only while the cable is in | none (a cable) | anyone on the cable |
| `--tls HOST[:PORT]` | TCP + TLS 1.3, e.g. `--tls tablet.lan` on Wi-Fi | TLS 1.3 | ed25519 keys in the tablet's `authorized_keys` |
| `--tcp HOST[:PORT]` | plain TCP, e.g. `--tcp tablet.vpn` over WireGuard | none (WireGuard's) | trusted addresses only (USB, loopback/wireproxy) |
| `--ssh HOST` | `ssh HOST re-fsd --stdio` | SSH | whoever ssh lets in |

`--usb`, `--tls` and `--tcp` need the tablet service (`re-fs deploy --service`);
`--ssh` only needs the agent binary.

### Giving several

```sh
re-fs mount ~/rm --usb --tls tablet.lan --ssh root@tablet.vpn
```

Each time re-fs connects, it tries `--usb`, then each `--tls`, then each `--tcp`,
then `--ssh`. It stays on the first that answers until that link drops, then
reconnects the same way.

### Addresses

`--tls` and `--tcp` take `HOST[:PORT]`: a hostname, an IPv4 address, or an IPv6
address (in brackets when a port follows, e.g. `[fe80::1]:7745`). The port is
**7745** unless given. Names are resolved on every connect, so a changed address is
picked up when re-fs reconnects; if a name has several addresses, each is tried.
Plain TCP is refused from Wi-Fi addresses: use `--tls` there.

## TLS keys

Over `--tls` both sides prove who they are with ed25519 keys:

- **You:** re-fs presents `~/.ssh/id_ed25519` (or `--identity FILE`) as a client
  certificate. That key must be an unrestricted `ssh-ed25519` line in the tablet's
  `/home/root/.ssh/authorized_keys`; removing the line revokes access at once.
  Passphrase-protected keys and ssh-agent aren't supported yet.
- **The tablet:** re-fs accepts only the agent key pinned in
  `~/.config/re-fs/tablet.pub`. `deploy --service` pins it; to pin it again (a new
  machine, a reinstalled tablet), fetch it over ssh, which authenticates the tablet:

  ```sh
  re-fs pair root@10.11.99.1
  ```

## What `--ssh` does

`--ssh` still uses re-fsd; only the way the client reaches it changes. With
`--ssh root@10.11.99.1`, re-fs runs

```sh
ssh root@10.11.99.1 /home/root/re-fs/re-fsd --stdio
```

That starts a fresh agent on the tablet for this mount, speaking the normal re-fs
protocol over the ssh channel's stdin and stdout. The index, reads, change events
and writes all work as over TCP. When you unmount, ssh closes and that agent exits.
It's separate from the service on port 7745; both can run at once. `--agent PATH`
changes where re-fs looks for the binary.

Unlike sshfs, which uses the tablet's generic SFTP server and makes a round trip for
every stat, readdir and read, re-fs over ssh still gets the whole index in one go,
browses from memory, receives pushed change events and caches reads. ssh is just the
encrypted, authenticated pipe.

### `--ssh` versus TCP and TLS

| | `--ssh` | `--usb` / `--tcp` / `--tls` |
|---|---|---|
| On the tablet | the agent binary (`re-fs deploy`); no service | the socket-activated service on port 7745 |
| Encryption | ssh (Dropbear) | none over USB/WireGuard; TLS 1.3 for `--tls` |
| Login | your ssh key, checked by Dropbear | trusted address, or a TLS client certificate from your ssh key |
| Connect time | an ssh handshake per mount: 0.3–0.4 s on USB/LAN, ~3.5 s over WireGuard | ~0.25 s on USB |
| Reads | ~18 MiB/s over USB, 13 MiB/s on Wi-Fi | 23 MiB/s plain TCP over USB, 18–23 MiB/s with TLS |

`--ssh` is the zero-setup fallback: it works anywhere ssh works. TCP and TLS
connect faster and transfer slightly faster.

## Measured on a reMarkable 2

| | `ls -R` | Mount | 16 MB PDF |
|---|---|---|---|
| TCP over USB | 0.23 s | 0.24–0.30 s | 23 MiB/s |
| TLS over USB (OpenSSL / rustls) | 0.26 s | 0.30 s | 23 / 19 MiB/s |
| TLS over Wi-Fi | 0.40 s | 0.35 s | 10–17 MiB/s (radio-bound) |
| ssh over Wi-Fi LAN | 0.76 s | 0.70 s | 13 MiB/s |
| TCP over WireGuard | 3.1 s | 3.3 s | 2 MiB/s |

More in [Measurements](../development/measurements.md). How the service, the TLS
handshake and the trust rules work: [Transports and security](../internals/transports-and-security.md).
