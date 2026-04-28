#!/bin/zsh
set -euo pipefail

# Best-effort stop: kill the running bridge process.
# (We avoid extra dependencies and keep this simple.)

APP="gipbridge"

echo ""
echo "Stopping PowerA → Dolphin pipe bridge…"

if pkill -f "/target/release/${APP}" 2>/dev/null; then
  echo "Stopped."
  exit 0
fi

if pkill -x "${APP}" 2>/dev/null; then
  echo "Stopped."
  exit 0
fi

echo "No running process found."

