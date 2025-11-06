#!/bin/sh
set -e
cargo build --release --
sudo pkill eventmgr || true
sudo systemctl start eventmgr.service
exec journalctl -f -u eventmgr



