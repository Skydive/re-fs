#!/bin/sh
# Mount a reMarkable with re-fs whenever it's plugged in by USB, and unmount it when
# it's unplugged. For systems without Nix (NixOS users: see the flake's modules).
#
#   ./install-usb-automount.sh                          # mount at /run/media/$USER/reMarkable
#   ./install-usb-automount.sh --mountpoint ~/tablet -- --rw -v
#   ./install-usb-automount.sh --uninstall
#
# Installs /etc/udev/rules.d/70-re-fs-remarkable.rules (with sudo), for a mount
# point under /run/media an /etc/tmpfiles.d entry that recreates it at boot, and the
# user service ~/.config/systemd/user/re-fs-usb.service, which runs
# `re-fs mount MOUNTPOINT --usb [ARGS…]` while the tablet is plugged in.
# Needs re-fs and fusermount3 on PATH, systemd, and the tablet service installed once
# (`re-fs deploy root@10.11.99.1 --service`).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
rules_src="$here/70-re-fs-remarkable.rules"
rules_dst=/etc/udev/rules.d/70-re-fs-remarkable.rules
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit="$unit_dir/re-fs-usb.service"
device=sys-subsystem-net-devices-remarkable.device
user=$(id -un)
mountpoint="/run/media/$user/reMarkable"
tmpfiles_dst="/etc/tmpfiles.d/re-fs-$user.conf"
uninstall=0

die() { echo "install-usb-automount: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --mountpoint) [ $# -ge 2 ] || die "--mountpoint needs a directory"; mountpoint=$2; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --) shift; break ;;
    *) die "unknown option $1 (re-fs options go after --)" ;;
  esac
done

if [ "$uninstall" = 1 ]; then
  systemctl --user disable --now re-fs-usb.service 2>/dev/null || true
  rm -f "$unit"
  systemctl --user daemon-reload
  sudo rm -f "$rules_dst" "$tmpfiles_dst"
  sudo udevadm control --reload-rules
  echo "Removed the re-fs USB auto-mount."
  exit 0
fi

re_fs=$(command -v re-fs) || die "re-fs isn't on PATH"
fusermount=$(command -v fusermount3 || command -v fusermount) || die "fusermount3 isn't on PATH (install fuse3)"
mkdir_bin=$(command -v mkdir)
[ -f "$rules_src" ] || die "missing $rules_src"

# Quote one argument for a systemd command line.
q() { printf '"%s"' "$(printf '%s' "$1" | sed 's/[\\"]/\\&/g')"; }
args="$(q "$mountpoint") --usb"
for a in "$@"; do args="$args $(q "$a")"; done

echo "Installing $rules_dst (needs sudo)…"
sudo install -m 0644 "$rules_src" "$rules_dst"
sudo udevadm control --reload-rules
# Apply it to a tablet that's already plugged in.
sudo udevadm trigger --subsystem-match=net --action=change

case "$mountpoint" in
  /run/media/*)
    # /run is cleared at boot and /run/media/$USER belongs to root: have
    # systemd-tmpfiles create the mount point (owned by you) now and at every boot.
    echo "Installing $tmpfiles_dst…"
    printf 'd %s 0700 %s - - -\n' "$mountpoint" "$user" | sudo tee "$tmpfiles_dst" >/dev/null
    sudo systemd-tmpfiles --create "$tmpfiles_dst"
    ;;
esac

mkdir -p "$unit_dir"
cat > "$unit" <<UNIT
# Installed by re-fs's install-usb-automount.sh.
[Unit]
Description=re-fs: the reMarkable, mounted while it's plugged in by USB
BindsTo=$device
After=$device

[Service]
Environment=PATH=$(dirname "$fusermount"):/usr/local/bin:/usr/bin:/bin
ExecStartPre=$(q "$mkdir_bin") -p $(q "$mountpoint")
ExecStart=$(q "$re_fs") mount $args
# If re-fs was killed rather than stopped, don't leave a dead mount behind.
ExecStopPost=-$(q "$fusermount") -uz $(q "$mountpoint")
# The interface appears a moment before the tablet's DHCP gives us an address:
# re-fs reconnects by itself, and a first start that finds nothing is retried.
Restart=on-failure
RestartSec=2

[Install]
WantedBy=$device
UNIT

systemctl --user daemon-reload
systemctl --user enable re-fs-usb.service
echo "Installed: plug the tablet in and it mounts at $mountpoint."
if systemctl --user is-active -q "$device" 2>/dev/null; then
  systemctl --user start re-fs-usb.service
  echo "The tablet is plugged in now; started re-fs-usb.service."
fi
echo "Check with: systemctl --user status re-fs-usb   /   journalctl --user -u re-fs-usb"
