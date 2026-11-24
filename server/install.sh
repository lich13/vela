#!/bin/sh
# Installs or upgrades the Vela server from the directory this script is in.
# Leaves /etc/vela/server.env and ~/.vela as they are.
set -eu

[ "$(id -u)" -eq 0 ] || { echo "install.sh: run as root" >&2; exit 1; }
dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

"$dir/vela-server" --version >/dev/null

install -m 0755 "$dir/vela-server" /usr/local/bin/vela-server.new
mv -f /usr/local/bin/vela-server.new /usr/local/bin/vela-server
install -m 0644 "$dir/vela-server.service" /etc/systemd/system/vela-server.service

systemctl daemon-reload
systemctl enable vela-server.service >/dev/null
systemctl restart vela-server.service
