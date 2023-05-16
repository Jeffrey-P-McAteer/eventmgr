#!/bin/sh
set -e
cargo build --release --
pkill eventmgr || true
exec journalctl -f -u eventmgr



