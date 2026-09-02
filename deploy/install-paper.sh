#!/usr/bin/env bash
set -euo pipefail
# Run as root after copying the release binary to /usr/local/bin.
install -d -m 0750 /etc/tradinghive /opt/tradinghive/data
id tradinghive >/dev/null 2>&1 || useradd --system --home /opt/tradinghive --shell /usr/sbin/nologin tradinghive
chown -R tradinghive:tradinghive /opt/tradinghive
install -m 0644 deploy/systemd/trading-hive.service /etc/systemd/system/trading-hive.service
systemctl daemon-reload
systemctl enable trading-hive.service
printf '%s\n' 'Create /etc/tradinghive/trading-hive.env before starting the service.'
printf '%s\n' 'Live mode is intentionally not enabled by this installer.'
