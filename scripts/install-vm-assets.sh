#!/bin/bash
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*" >&2; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

# Determine script and project directories
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
ASSETS_DIR="$PROJECT_ROOT/assets"
KERNELS_DIR="$ASSETS_DIR/kernels"
ROOTFS_DIR="$ASSETS_DIR/rootfs"

# Asset URLs
KERNEL_URL="https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/x86_64/kernels/vmlinux.bin"
ROOTFS_URL="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64/ubuntu-22.04.ext4"

# Target file paths
KERNEL_PATH="$KERNELS_DIR/vmlinux"
ROOTFS_PATH="$ROOTFS_DIR/ubuntu.ext4"

check_dependencies() {
    info "Checking dependencies..."

    local missing_deps=()

    for cmd in curl file; do
        if ! command -v "$cmd" &> /dev/null; then
            missing_deps+=("$cmd")
        fi
    done

    if [ ${#missing_deps[@]} -gt 0 ]; then
        error "Missing required dependencies: ${missing_deps[*]}"
        error "Please install them and try again."
        exit 1
    fi

    info "All dependencies satisfied"
}

download_file() {
    local url="$1"
    local output_path="$2"
    local description="$3"

    info "Downloading $description..."
    info "  URL: $url"
    info "  Destination: $output_path"

    if curl -fsSL --progress-bar "$url" -o "$output_path"; then
        info "  Successfully downloaded $description"
        return 0
    else
        error "Failed to download $description from $url"
        return 1
    fi
}

verify_kernel() {
    local kernel_path="$1"

    info "Verifying kernel image..."

    # Check file exists and is not empty
    if [ ! -f "$kernel_path" ]; then
        error "Kernel file not found: $kernel_path"
        return 1
    fi

    local size=$(stat -f%z "$kernel_path" 2>/dev/null || stat -c%s "$kernel_path" 2>/dev/null || echo 0)
    if [ "$size" -lt 1000000 ]; then  # Less than 1MB is suspicious
        error "Kernel file seems too small (${size} bytes)"
        return 1
    fi

    # Check file type (should be ELF for x86_64)
    local file_type=$(file "$kernel_path")
    if [[ ! "$file_type" =~ "ELF" ]] && [[ ! "$file_type" =~ "executable" ]]; then
        warn "Kernel file type unexpected: $file_type"
        warn "Expected ELF executable, but will continue..."
    fi

    info "  Kernel size: $(numfmt --to=iec-i --suffix=B "$size" 2>/dev/null || echo "${size} bytes")"
    info "  Kernel verified successfully"
    chmod +x "$kernel_path"

    return 0
}

verify_rootfs() {
    local rootfs_path="$1"

    info "Verifying rootfs image..."

    # Check file exists and is not empty
    if [ ! -f "$rootfs_path" ]; then
        error "Rootfs file not found: $rootfs_path"
        return 1
    fi

    local size=$(stat -f%z "$rootfs_path" 2>/dev/null || stat -c%s "$rootfs_path" 2>/dev/null || echo 0)
    if [ "$size" -lt 10000000 ]; then  # Less than 10MB is suspicious
        error "Rootfs file seems too small (${size} bytes)"
        return 1
    fi

    # Check file type (should be ext4 filesystem)
    local file_type=$(file "$rootfs_path")
    if [[ ! "$file_type" =~ "ext" ]] && [[ ! "$file_type" =~ "filesystem" ]]; then
        warn "Rootfs file type unexpected: $file_type"
        warn "Expected ext filesystem, but will continue..."
    fi

    info "  Rootfs size: $(numfmt --to=iec-i --suffix=B "$size" 2>/dev/null || echo "${size} bytes")"
    info "  Rootfs verified successfully"

    return 0
}

download_kernel() {
    if [ -f "$KERNEL_PATH" ]; then
        info "Kernel already exists at $KERNEL_PATH"
        if verify_kernel "$KERNEL_PATH"; then
            info "Existing kernel is valid, skipping download"
            return 0
        else
            warn "Existing kernel failed verification, re-downloading..."
            rm -f "$KERNEL_PATH"
        fi
    fi

    mkdir -p "$KERNELS_DIR"

    if download_file "$KERNEL_URL" "$KERNEL_PATH" "kernel image"; then
        verify_kernel "$KERNEL_PATH"
    else
        error "Kernel download failed"
        return 1
    fi
}

download_rootfs() {
    if [ -f "$ROOTFS_PATH" ]; then
        info "Rootfs already exists at $ROOTFS_PATH"
        if verify_rootfs "$ROOTFS_PATH"; then
            info "Existing rootfs is valid, skipping download"
            return 0
        else
            warn "Existing rootfs failed verification, re-downloading..."
            rm -f "$ROOTFS_PATH"
        fi
    fi

    mkdir -p "$ROOTFS_DIR"

    if download_file "$ROOTFS_URL" "$ROOTFS_PATH" "rootfs image"; then
        verify_rootfs "$ROOTFS_PATH"
    else
        error "Rootfs download failed"
        return 1
    fi
}

print_summary() {
    echo ""
    info "================================================"
    info "  Firecracker VM Assets Installation Complete"
    info "================================================"
    echo ""
    info "Downloaded assets:"
    info "  Kernel:  $KERNEL_PATH"
    info "  Rootfs:  $ROOTFS_PATH"
    echo ""
    info "You can now build and run clawpot-driver:"
    echo "  cd clawpot-driver"
    echo "  cargo build --release"
    echo "  sudo ./target/release/clawpot-driver start \\"
    echo "    --kernel $KERNEL_PATH \\"
    echo "    --rootfs $ROOTFS_PATH"
    echo ""
}

main() {
    info "Starting Firecracker VM asset installation..."
    info "Project root: $PROJECT_ROOT"

    check_dependencies

    # Create assets directory if it doesn't exist
    mkdir -p "$ASSETS_DIR"

    # Download kernel and rootfs
    if ! download_kernel; then
        error "Failed to download kernel"
        exit 1
    fi

    if ! download_rootfs; then
        error "Failed to download rootfs"
        exit 1
    fi

    print_summary

    info "Installation completed successfully!"
}

# Run main function
main "$@"
