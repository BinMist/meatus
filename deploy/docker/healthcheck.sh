#!/bin/sh
set -eu
URL="${MEATUS_HEALTH_URL:-http://127.0.0.1:7400/healthz}"
wget -qO- "$URL" >/dev/null
