# Clawpot

Firecracker microVM orchestration system. A gRPC-based server manages VM lifecycle, networking, and guest command execution, with a CLI client and an in-VM agent.

## Prerequisites

- Linux host with KVM support (`/dev/kvm`)
- Firecracker v1.9.1+ installed
- Rust toolchain
- Root access (for TAP devices, bridge, iptables)

## Quick Start

```bash
# Build everything
cargo build --workspace

# Build the guest agent (static musl binary)
cargo build --release --target x86_64-unknown-linux-musl -p clawpot-agent

# Download kernel and rootfs
./scripts/install-vm-assets.sh

# Inject the agent into the rootfs
sudo ./scripts/setup-rootfs.sh

# Start the server (requires root)
sudo target/debug/clawpot-server

# In another terminal — create a VM
clawpot create --vcpus 2 --memory 512

# List VMs
clawpot list

# Run a command inside a VM
clawpot exec <vm_id> -- uname -a

# Delete a VM
clawpot delete <vm_id>
```

## Architecture

```
┌──────────┐  gRPC   ┌─────────────────┐  vsock   ┌───────────────┐
│  clawpot  │───────>│  clawpot-server  │────────>│  clawpot-agent │
│   (CLI)   │ :50051 │  (VM manager)    │  :10051 │  (in-guest)    │
└──────────┘        └─────────────────┘          └───────────────┘
```

- **clawpot-server** — gRPC server that manages Firecracker microVMs. Handles VM creation/deletion, TAP networking (bridge `clawpot-br0`, subnet `192.168.100.0/24`), and proxies command execution to guest agents over vsock.
- **clawpot-cli** — CLI client (`clawpot`). Connects to the server at `127.0.0.1:50051` (configurable via `--server`).
- **clawpot-agent** — Guest agent that runs inside each microVM. Listens on vsock port 10051 and executes commands on behalf of the server. Built as a static musl binary.
- **clawpot-common** — Shared library: Firecracker HTTP client, VM manager, protobuf types.

## CLI Reference

```
clawpot [--server <URL>] <command>
```

| Command  | Description | Arguments |
|----------|-------------|-----------|
| `create` | Create a new VM | `--vcpus <N>` (default: 1), `--memory <MiB>` (default: 256) |
| `delete` | Delete a VM | `<vm_id>` |
| `list`   | List all VMs | — |
| `exec`   | Run a command in a VM | `<vm_id> -- <command> [args...]` |

## Testing

### Unit tests

```bash
cargo test --workspace
```

### Integration tests (local)

Requires root and `/dev/kvm`. Spins up Firecracker microVMs end-to-end.

```bash
cd tests/integration
sudo -E $(which uv) run pytest -v -s --timeout=120
```

### Integration tests (CI)

CI runs on Buildkite with nested KVM. Push and monitor:

```bash
git push
python utils/monitor_build.py HEAD
```

The pipeline builds the workspace, packages binaries and assets, launches an ephemeral QEMU inner VM, and runs the integration tests inside it. On failure, `monitor_build.py` prints the full logs for failed jobs.

## Project Structure

```
clawpot/
├── clawpot-server/        # gRPC server — VM lifecycle, networking
├── clawpot-cli/           # CLI client (binary: clawpot)
├── clawpot-agent/         # Guest agent (static musl binary)
├── clawpot-common/        # Shared library — Firecracker client, proto types
├── proto/                 # Protobuf service definitions
├── assets/                # VM kernel and rootfs
├── scripts/               # Asset download and rootfs setup
├── tests/integration/     # End-to-end pytest suite
├── ci/                    # CI infrastructure (VM provisioning, golden image)
├── .buildkite/            # Pipeline definition and build/test scripts
└── utils/                 # Developer utilities (build monitor)
```

## Resources

- [Firecracker Documentation](https://github.com/firecracker-microvm/firecracker/tree/main/docs)
- [Firecracker Getting Started](https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md)
