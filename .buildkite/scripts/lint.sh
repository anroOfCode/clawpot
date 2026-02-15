#!/bin/bash
set -euo pipefail

# Buildkite lint step: check formatting and run static analysis.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$PROJECT_ROOT"

echo "--- :art: rustfmt"
cargo fmt --check

echo "--- :mag: clippy"
cargo clippy --workspace -- -D warnings

echo "--- :snake: ruff (lint + format)"
cd "$PROJECT_ROOT/tests/integration"
uvx ruff check .
uvx ruff format --check .
