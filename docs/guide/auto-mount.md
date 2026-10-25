# Auto-mount on USB

re-fs can mount the tablet whenever it's plugged in by USB, and unmount it when it's
unplugged. The mount appears at `/run/media/<user>/reMarkable`, where file managers
built on GIO (Nautilus and friends) list removable media, with an eject button.

It needs the tablet service, installed once with `re-fs deploy root@10.11.99.1
--service` (see [Installation](installation.md)).

## NixOS and home-manager

The flake has a NixOS module, which names the tablet's USB network device for systemd
and creates the mount point at boot. A home-manager module adds the mount itself:

```nix
# flake inputs
inputs.re-fs.url = "github:Skydive/re-fs";

# NixOS configuration
imports = [ inputs.re-fs.nixosModules.default ];
services.re-fs.usbDevice = { enable = true; users = [ "you" ]; };

# home-manager configuration
imports = [ inputs.re-fs.homeManagerModules.default ];
services.re-fs.usbMount = {
  enable = true;
  # mountPoint = "/run/media/you/reMarkable";   # the default
  extraArgs = [ "-v" ];                         # any other `re-fs mount` options
};
```

## Other distributions

Any systemd distribution works with the install script. It ships in the release
tarball under `host/` and in the packages under `/usr/share/re-fs/host/`, and it's
also in the repository as `deploy/host/`. Run it as yourself, not as root; it asks
for sudo when it needs it:

```sh
/usr/share/re-fs/host/install-usb-automount.sh                     # /run/media/$USER/reMarkable
/usr/share/re-fs/host/install-usb-automount.sh --mountpoint ~/tablet -- --overwrite -v
/usr/share/re-fs/host/install-usb-automount.sh --uninstall
```

It needs `re-fs` and `fusermount3` on your PATH. It installs:

- `/etc/udev/rules.d/70-re-fs-remarkable.rules`, the same rule as the NixOS module,
  and reloads udev so an already-plugged-in tablet is picked up;
- `/etc/tmpfiles.d/re-fs-$USER.conf`, which creates the mount point under `/run/media`
  at every boot;
- `~/.config/systemd/user/re-fs-usb.service`, with the paths it found, and enables it.

Everything after `--` is passed to `re-fs mount`.

## How it works

1. Plugged in, the tablet appears as a USB network card. The udev rule matches its
   USB id `04b3:4010` and gives the interface the systemd name
   `sys-subsystem-net-devices-remarkable.device`.
2. The user service `re-fs-usb.service` is `WantedBy` and `BindsTo` that device, so
   systemd starts it on plug-in and stops it on unplug.
3. The service runs `re-fs mount /run/media/<user>/reMarkable --usb`. The interface
   appears a moment before the tablet's DHCP gives your machine its 10.11.99.x
   address; re-fs reconnects (or systemd restarts it) until the link is up. On
   unplug, re-fs unmounts on SIGTERM; a leftover mount is cleared with
   `fusermount3 -uz` if it was killed instead.

`/run` is cleared at boot and `/run/media/<user>` belongs to root, which is why the
NixOS module (`users`) or the script (`/etc/tmpfiles.d`) recreates the mount point,
owned by you, at every boot. Any other directory you own works too (`mountPoint`, or
`--mountpoint` for the script).

## Checking

```sh
systemctl --user status re-fs-usb
journalctl --user -u re-fs-usb
```

Don't mount the same mount point by hand at the same time.

If your tablet reports a different USB id, find it with
`udevadm info /sys/class/net/<iface> | grep -E 'ID_(VENDOR|MODEL)_ID'`. Then set
`vendorId`/`productId` in the NixOS module, or edit the rule file before running the
script.
