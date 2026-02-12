#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if [ "$(id -u)" -ne 0 ]; then
    echo "Integration tests require root. Re-running with sudo..."
    exec sudo -E "$(which uv)" run pytest -v -s --timeout=120 "$@"
fi

exec uv run pytest -v -s --timeout=120 "$@"
