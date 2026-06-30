#!/usr/bin/env bash
# Creates and configures a immich LXC on Proxmox VE. Run on the host as root.
set -euo pipefail
VMID="${1:?Usage: $0 <vmid> [options]}"
# TODO: pct create / config / install immich. Mirror jellyfin/lxc/provision.sh.
echo "[provision] immich LXC $VMID — not yet implemented"
