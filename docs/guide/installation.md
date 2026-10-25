# Installation

Installing re-fs takes two steps: install the client `re-fs` on your computer, then
let it put the agent `re-fsd` on the tablet. Both come from one build; the client
carries the agents for every tablet model.

## 1. Install the client

### From a release (Debian, Ubuntu, Fedora, openSUSE, Arch, …)

Download the files from the
[latest release](https://github.com/Skydive/re-fs/releases/latest) and install the
one for your distribution:

```sh
sudo apt install ./re-fs_*_amd64.deb                # Debian, Ubuntu
sudo dnf install ./re-fs-*.x86_64.rpm               # Fedora (openSUSE: sudo zypper in)
sudo pacman -U ./re-fs-*-x86_64.pkg.tar.zst         # Arch
```

The packages install `/usr/bin/re-fs` and pull in `fuse3` (for `fusermount3`) and an
ssh client. On any other x86_64 Linux, unpack the tarball. It holds the same
client as one static binary, with no dependencies besides `fusermount3` and `ssh`:

```sh
tar xzf re-fs-*-x86_64-linux.tar.gz
sudo install re-fs-*-x86_64-linux/re-fs /usr/local/bin/
```

| Release file | What it is |
|---|---|
| `re-fs_*_amd64.deb`, `re-fs-*.x86_64.rpm`, `re-fs-*-x86_64.pkg.tar.zst` | the client, packaged |
| `re-fs-*-x86_64-linux.tar.gz` | the client as one static binary, plus the USB auto-mount files |
| `re-fs-windows-x86_64.exe` | the Windows client: `ls`, `push`, `pair`, `deploy`; no `mount` yet |
| `re-fsd-armv7`, `re-fsd-aarch64` | the default agents on their own (the client already contains them) |
| `re-fsd-armv7-rustls`, `re-fsd-aarch64-rustls` | static agents, for `deploy --binary` |
| `SHA256SUMS` | checksums of all of the above |

### With Nix

```sh
nix build github:Skydive/re-fs#re-fs     # or, in a checkout: nix build .#re-fs
./result/bin/re-fs --version
```

As a flake input (`inputs.re-fs.url = "github:Skydive/re-fs";`), the package is
`re-fs.packages.x86_64-linux.re-fs`. The flake also has NixOS and home-manager
modules for [auto-mounting on USB](auto-mount.md).

## 2. Put the agent on the tablet

Connect the tablet by USB, where it's always `10.11.99.1`. The tablet must accept
your ssh key for `root`. On a reMarkable 1 or 2, add your public key to
`/home/root/.ssh/authorized_keys`. The password is under *Settings → Help → Copyright
and licenses*. Then run:

```sh
re-fs deploy root@10.11.99.1 --service
```

`deploy` asks the tablet for its CPU over ssh (`uname -m` and its model name) and
sends the matching agent:

| Model | CPU | Agent | Status |
|---|---|---|---|
| reMarkable 1 | armv7 (Cortex-A9) | `re-fsd-armv7` | untested |
| reMarkable 2 | armv7 (Cortex-A7) | `re-fsd-armv7` | in daily use |
| Paper Pro, Paper Pro Move | aarch64 (Cortex-A53/A55) | `re-fsd-aarch64` | builds; not tried on a device |

With `--service` it also:

- installs a socket-activated systemd service on port 7745 (systemd starts the agent
  on the first connection; it exits after 5 idle minutes);
- creates the agent's TLS key (`/home/root/re-fs/tls.key`) and pins it on your machine
  in `~/.config/re-fs/tablet.pub`, for [Wi-Fi with TLS](connecting.md#tls-keys).

Without `--service`, `deploy` only copies the agent to `/home/root/re-fs/re-fsd`.
That's enough for `--ssh`. It also stops a running agent so the next connection
starts the new one. Run `deploy` again after updating re-fs: client and agent must
come from the same build.

### Paper Pro

The Paper Pro gives root over ssh only in developer mode, and turning that on wipes
the tablet. Its agent uses the tablet's own OpenSSL 3, libzstd and glibc (2.39 or
newer), like the reMarkable 2's. That hasn't been checked on a Paper Pro. If the agent
doesn't start there, install the static one, which needs nothing on the tablet:

```sh
re-fs deploy root@10.11.99.1 --service --binary re-fsd-aarch64-rustls
```

### After a reMarkable OS update

OS updates reset `/etc`, which removes the service. The agent itself survives in
`/home/root/re-fs/`. Reinstall the service on the tablet:

```sh
sh /home/root/re-fs/install.sh
```

## Next

[Mount the tablet](mounting.md), and [choose how to connect](connecting.md) if you
want Wi-Fi or a VPN as well as USB.
