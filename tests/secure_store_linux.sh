#!/usr/bin/env bash
# Isolate the D-Bus session and keyring files; never unlock the user's store.
set -euo pipefail
mushy_store_tmp=$(mktemp -d)
trap 'rm -rf "$mushy_store_tmp"' EXIT
export XDG_RUNTIME_DIR="$mushy_store_tmp/runtime"
export XDG_DATA_HOME="$mushy_store_tmp/data"
export XDG_CONFIG_HOME="$mushy_store_tmp/config"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
dbus-run-session -- bash -euo pipefail -c '
  printf "%s" "synthetic-ci-keyring-password" | gnome-keyring-daemon --unlock --components=secrets
  cargo test -p mushy-links --test credentials --locked -- --ignored
'
