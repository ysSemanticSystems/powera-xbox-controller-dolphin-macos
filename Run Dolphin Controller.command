#!/bin/zsh
set -euo pipefail

cd "$(dirname "$0")"

echo ""
echo "PowerA → Dolphin pipe bridge"
echo "----------------------------------------"
echo "This will build (if needed) and start the bridge."
echo "You will be prompted for your password (sudo) for USB access."
echo ""
echo "To stop: run \"Stop Dolphin Controller.command\" or press Ctrl+C in this Terminal."
echo ""

make run

