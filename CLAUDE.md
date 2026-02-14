# Clawpot

Firecracker microVM orchestration system.

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

### Integration tests (dev VM)

The dev VM is the best way to iterate on integration tests when building new features or debugging. It gives you a long-lived QEMU VM with `/dev/kvm`, Firecracker, Rust, and all test dependencies — you can sync code, rebuild, and re-run tests in seconds without waiting for CI.

First launch (one-time setup, downloads base image and builds golden image):

```bash
sudo utils/devvm launch
```

Then the inner loop is just sync and run:

```bash
utils/devvm sync
utils/devvm run "cd /work/clawpot && cargo build --workspace"
utils/devvm run "cd /work/clawpot && cargo build --release --target x86_64-unknown-linux-musl -p clawpot-agent"
utils/devvm run "cd /work/clawpot && sudo bash scripts/install-vm-assets.sh"
utils/devvm run "cd /work/clawpot && sudo bash scripts/setup-rootfs.sh"
utils/devvm run "cd /work/clawpot/tests/integration && sudo -E \$(which uv) run pytest -v -s --timeout=120"
```

The VM persists until you destroy it, so cargo caches and build artifacts accumulate across syncs — incremental rebuilds are fast. You can also SSH in interactively:

```bash
utils/devvm ssh
```

Other commands:

```bash
utils/devvm status                  # check if a dev VM is running
sudo utils/devvm destroy            # tear down the VM
sudo utils/devvm launch --rebuild   # rebuild the golden image (e.g. after changing deps)
```

### Integration tests (CI)

CI is the right choice when you're a Cloud agent that can't run VMs directly, or when you want to verify your changes in a clean environment before merging. Push your commit, then monitor the Buildkite build:

```bash
git push
python utils/monitor_build.py HEAD
```

The CI pipeline runs on a home lab VM with nested KVM. It:
1. Restores cargo target and VM asset caches from `/var/lib/clawpot-ci/cache/`
2. Builds the workspace and runs unit tests
3. Saves caches back for the next run
4. Packages binaries + assets + tests into a tarball
5. Launches an ephemeral inner VM (QEMU, from a golden image)
6. Runs integration tests inside the inner VM (which spawn Firecracker microVMs)
7. Collects artifacts and destroys the VM

On failure, `monitor_build.py` prints the full logs for failed jobs.

To monitor a specific commit:

```bash
python utils/monitor_build.py <commit-sha>
```

## Workflow

Direct commits to the main branch are disabled. All changes must be submitted via pull request.

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
- `utils/` — Developer utilities (build monitor, dev VM manager)
