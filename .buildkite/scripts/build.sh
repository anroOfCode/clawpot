#!/bin/bash
set -euo pipefail

# Buildkite build step: compile, unit test, and package a tarball
# for the ephemeral inner VM.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$PROJECT_ROOT"

echo "--- :rust: Build workspace (debug)"
cargo build --workspace

echo "--- :rust: Build agent (musl, release)"
cargo build --release --target x86_64-unknown-linux-musl -p clawpot-agent

echo "--- :rust: Unit tests"
cargo test --workspace

echo "--- :package: Download VM assets"
./scripts/install-vm-assets.sh

echo "--- :package: Package build tarball"

TARBALL="$PROJECT_ROOT/build.tar.gz"
STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT

DEST="$STAGING/clawpot"
mkdir -p "$DEST/target/debug" \
         "$DEST/target/x86_64-unknown-linux-musl/release" \
         "$DEST/assets/kernels" \
         "$DEST/assets/rootfs" \
         "$DEST/scripts" \
         "$DEST/tests/integration" \
         "$DEST/proto"

# Binaries
cp target/debug/clawpot-server   "$DEST/target/debug/"
cp target/debug/clawpot          "$DEST/target/debug/"
cp target/x86_64-unknown-linux-musl/release/clawpot-agent \
                                 "$DEST/target/x86_64-unknown-linux-musl/release/"

# VM assets (pristine — setup-rootfs.sh will modify the rootfs inside the inner VM)
cp assets/kernels/vmlinux        "$DEST/assets/kernels/"
cp assets/rootfs/ubuntu.ext4     "$DEST/assets/rootfs/"

# Scripts and test code
cp scripts/setup-rootfs.sh       "$DEST/scripts/"
cp -r tests/integration/.        "$DEST/tests/integration/"
cp -r proto/.                    "$DEST/proto/"

# The inner VM entry point
cp ci/install_and_test.sh        "$DEST/"

# Create tarball
tar -czf "$TARBALL" -C "$STAGING" clawpot

TARBALL_SIZE=$(stat -c%s "$TARBALL" 2>/dev/null || stat -f%z "$TARBALL" 2>/dev/null)
echo "Tarball: $TARBALL ($(numfmt --to=iec-i --suffix=B "$TARBALL_SIZE" 2>/dev/null || echo "${TARBALL_SIZE} bytes"))"
