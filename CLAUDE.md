# Clawpot

Firecracker microVM orchestration system. Rust workspace with 4 crates.

## Build

```bash
cargo build --workspace                                                    # debug build (server + CLI)
cargo build --release --target x86_64-unknown-linux-musl -p clawpot-agent  # static agent binary
```

## Test

### Unit tests

```bash
cargo test --workspace
```

### Integration tests (local)

Requires root and `/dev/kvm`. Runs Firecracker microVMs.

```bash
cd tests/integration
sudo -E $(which uv) run pytest -v -s --timeout=120
```

### Integration tests (CI)

Push your commit, then monitor the Buildkite build:

```bash
git push
BUILDKITE_API_TOKEN=<token> python utils/monitor_build.py HEAD
```

The CI pipeline runs on a home lab VM with nested KVM. It:
1. Builds the workspace and runs unit tests
2. Packages binaries + assets + tests into a tarball
3. Launches an ephemeral inner VM (QEMU, from a golden image)
4. Runs integration tests inside the inner VM (which spawn Firecracker microVMs)
5. Collects artifacts and destroys the VM

On failure, `monitor_build.py` prints the full logs for failed jobs.

To monitor a specific commit:

```bash
BUILDKITE_API_TOKEN=<token> python utils/monitor_build.py <commit-sha>
```

## Project structure

- `clawpot-server/` — gRPC server that manages Firecracker microVMs
- `clawpot-cli/` — CLI client (`clawpot`)
- `clawpot-agent/` — Guest agent (runs inside microVMs, built as static musl binary)
- `clawpot-common/` — Shared types and protobuf definitions
- `proto/` — Protobuf service definitions
- `assets/` — VM kernel and rootfs
- `scripts/` — Asset installation and rootfs setup
- `tests/integration/` — End-to-end pytest suite
- `ci/` — CI infrastructure (VM provisioning, golden image, test runner)
- `.buildkite/` — Pipeline definition and build/test scripts
- `utils/` — Developer utilities (build monitor)
