#!/bin/bash
set -euo pipefail

# Buildkite lint step: delegates to scripts/lint.sh (check mode).

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

echo "--- :lint-roller: Lint"
"$PROJECT_ROOT/scripts/lint.sh"
