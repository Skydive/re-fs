#!/bin/sh
# Install (or reinstall) the re-fsd socket and service. reMarkable OS updates reset
# /etc, so run this again after an update, e.g. from /home/root/wireproxy/reinstall.sh:
#   sh /home/root/re-fs/install.sh
set -e
# Before socket activation the service was enabled on its own.
systemctl disable --now re-fsd.service 2>/dev/null || true
cp /home/root/re-fs/re-fsd.socket /home/root/re-fs/re-fsd.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable re-fsd.socket
systemctl restart re-fsd.socket
systemctl is-active re-fsd.socket
