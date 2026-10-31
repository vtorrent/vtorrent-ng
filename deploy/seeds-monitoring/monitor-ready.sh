#!/bin/sh
set -eu
curl -fsS --max-time 5 http://127.0.0.1:9090/-/ready >/dev/null
curl -fsS --max-time 5 http://127.0.0.1:9093/-/ready >/dev/null
systemctl is-active --quiet ntfy-relay
