#!/bin/sh
# Install (or reinstall) the re-fsd service. reMarkable OS updates reset /etc,
# so run this again after an update, e.g. from /home/root/wireproxy/reinstall.sh:
#   sh /home/root/re-fs/install.sh
set -e
cp /home/root/re-fs/re-fsd.service /etc/systemd/system/re-fsd.service
systemctl daemon-reload
systemctl enable re-fsd
systemctl restart re-fsd
systemctl is-active re-fsd
