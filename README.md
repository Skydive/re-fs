<p align="center"><img src="docs/logo.svg" alt="re-fs" width="346"></p>

<p align="center">Your reMarkable's documents as an ordinary folder tree.</p>

## Quick start

**1. Install** the package for your distribution from the
[latest release](https://github.com/Skydive/re-fs/releases/latest), or build it with Nix:

```sh
sudo apt install ./re-fs_*_amd64.deb            # Debian, Ubuntu
sudo dnf install ./re-fs-*.x86_64.rpm           # Fedora (openSUSE: sudo zypper in)
sudo pacman -U ./re-fs-*-x86_64.pkg.tar.zst     # Arch
nix build github:Skydive/re-fs#re-fs            # Nix: ./result/bin/re-fs
```

**2. Put the agent on the tablet**, once, with the tablet plugged in by USB and your
ssh key in its `/home/root/.ssh/authorized_keys`:

```sh
re-fs deploy root@10.11.99.1 --service
```

**3. Mount it:**

```sh
mkdir -p ~/rm
re-fs mount ~/rm --usb        # Ctrl-C, or fusermount3 -u ~/rm, unmounts
```

Your folders, PDFs and EPUBs appear under `~/rm`, with notebooks rendered to PDF and
`X.annotated.pdf` files showing your handwriting. Copying, moving and deleting change
the tablet (deleted files go to its trash).

## Documentation

[The user guide](docs/README.md) covers Wi-Fi and VPN connections, mounting on USB
plug-in, every option, and how re-fs works.

MIT licensed; see [LICENSE](LICENSE).
