
# Eventmgr

Simple async event handler for common desktop i/o events.

## Oneliner graveyard

```bash

# Watch load for process
htop -p $(pgrep eventmgr)

# Build new, kill running, attach to systemctl logs from new process
cargo build --release -- ; pkill eventmgr ; journalctl -f -u eventmgr


```

