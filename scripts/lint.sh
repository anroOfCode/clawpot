#!/bin/bash
set -euo pipefail

# Run all linters and formatters locally.
# Use --fix to auto-fix issues instead of just checking.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

FIX=false
if [[ "${1:-}" == "--fix" ]]; then
    FIX=true
fi

echo "=== rustfmt ==="
if $FIX; then
    cargo fmt
    echo "Formatted."
else
    cargo fmt --check
fi

echo "=== clippy ==="
if $FIX; then
    cargo clippy --workspace --fix --allow-dirty --allow-staged -- -D warnings
else
    cargo clippy --workspace -- -D warnings
fi

echo "=== ruff ==="
cd "$PROJECT_ROOT/tests/integration"
if $FIX; then
    uvx ruff check --fix .
    uvx ruff format .
else
    uvx ruff check .
    uvx ruff format --check .
fi

echo "All checks passed."
