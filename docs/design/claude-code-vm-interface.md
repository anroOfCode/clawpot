# Design: Claude Code VM Interface

## Problem

We want to run an AI coding agent (with a Claude Code-like experience) inside
Clawpot's Firecracker microVMs. The agent should be able to read/write files,
run commands, and interact with the network — all sandboxed within a VM. A user
should be able to start an agent session, give it a task, and observe its work.

## Design Dimensions

Before choosing an architecture, there are five independent decisions:

| Dimension | Options |
|---|---|
| **Where does the agent loop run?** | Host, inside VM, or separate service |
| **How are tools executed?** | Directly in VM, proxied via RPC, or mixed |
| **Where does the API key live?** | Host only, inside VM, or proxied |
| **What framework drives the loop?** | Agent SDK, raw API, or Claude Code CLI |
| **How does the user interact?** | Host CLI, interactive VM session, or streaming RPC |

These combine into a large space, but in practice there are four viable
architectures worth considering.

---

## Architecture A: Host-Side Agent, VM-Side Tools

```
 User
  │
  ▼
┌──────────────────────────────────────────────────┐
│  Host                                            │
│                                                  │
│  ┌──────────────┐     ┌───────────────────────┐  │
│  │  CLI / TUI   │────▶│  Agent Harness        │  │
│  │  (clawpot    │     │  (Agent SDK or raw    │  │
│  │   agent cmd) │◀────│   API loop)           │  │
│  └──────────────┘     └───────┬───────────────┘  │
│                               │                  │
│                    ┌──────────┼──────────┐       │
│                    │ Tool dispatch layer │       │
│                    │  Bash  → ExecVM     │       │
│                    │  Read  → ExecVM cat │       │
│                    │  Write → ExecVM tee │       │
│                    │  Edit  → ExecVM sed │       │
│                    │  Glob  → ExecVM find│       │
│                    │  Grep  → ExecVM rg  │       │
│                    └──────────┼──────────┘       │
│                               │ gRPC/vsock       │
└───────────────────────────────┼──────────────────┘
                                │
                    ┌───────────▼──────────────┐
                    │  Firecracker VM          │
                    │  ┌────────────────────┐  │
                    │  │  clawpot-agent     │  │
                    │  │  (existing, as-is) │  │
                    │  └────────────────────┘  │
                    │  filesystem, processes   │
                    └──────────────────────────┘
```

**How it works:** The agent loop runs on the host. Each tool call (Bash, Read,
Write, etc.) is translated into one or more `ExecVM` RPCs that run commands
inside the VM. The API key never enters the VM.

**Tool mapping:**

| Claude Tool | VM Execution |
|---|---|
| `Bash(command)` | `ExecVM(vm, "bash", ["-c", command])` |
| `Read(path)` | `ExecVM(vm, "cat", ["-n", path])` |
| `Write(path, content)` | `ExecVM(vm, "bash", ["-c", "cat > path <<'CLAWPOT_EOF'\n..."])` |
| `Edit(path, old, new)` | Custom: Read file, apply replacement on host, Write back |
| `Glob(pattern)` | `ExecVM(vm, "find", ...) or ExecVM(vm, "bash", ["-c", "ls ..."])` |
| `Grep(pattern, path)` | `ExecVM(vm, "grep", ["-rn", pattern, path])` |

**Framework options:**
- **Claude Agent SDK (Python/TypeScript):** Use SDK MCP servers to define custom
  tools that proxy to ExecVM. The SDK handles the agentic loop, context
  management, and tool dispatch. This is the least code to write.
- **Raw Anthropic API:** Build the tool loop manually in Rust (matching the
  existing codebase language). More control, more code.

**Pros:**
- API key stays on the host — no credential leakage risk
- VM stays lightweight — no Node.js/Python runtime needed for the agent
- Host has full visibility into every tool call (can log, filter, rate-limit)
- Works with existing `ExecVM` RPC — no agent changes needed
- Network authorization naturally applies (agent's HTTP calls go through host)
- Event logging integrates cleanly (host sees all operations)

**Cons:**
- Tool fidelity gap: mapping Claude Code's tools to shell commands is imperfect
  (especially `Edit` with its exact-match replacement semantics)
- Latency: each tool call is a gRPC round-trip through vsock (though vsock is
  fast — sub-millisecond)
- File content must be serialized through gRPC (large files = large messages)
- Need to handle encoding, binary files, file permissions carefully
- Two-language split if using Agent SDK (Python on host, Rust server)

---

## Architecture B: Claude Code Installed in the VM

```
 User
  │
  ▼
┌──────────────────────────────────────────────────┐
│  Host                                            │
│  ┌──────────────────────────────────────┐        │
│  │  clawpot attach <vm-id>             │        │
│  │  (interactive terminal via          │        │
│  │   ExecVMStream)                     │        │
│  └──────────────────┬──────────────────┘        │
│                     │ bidirectional              │
│                     │ streaming                  │
└─────────────────────┼───────────────────────────┘
                      │
          ┌───────────▼──────────────┐
          │  Firecracker VM          │
          │                          │
          │  ┌────────────────────┐  │
          │  │  claude (CLI)      │  │
          │  │  Node.js runtime   │  │
          │  │  ─ Bash tool       │  │
          │  │  ─ Read/Write      │  │
          │  │  ─ Edit            │  │
          │  │  ─ Glob/Grep       │  │
          │  │  All native, local │  │
          │  └────────┬───────────┘  │
          │           │              │
          │  ┌────────▼───────────┐  │
          │  │  Anthropic API     │──┼──▶ api.anthropic.com
          │  │  (HTTPS via proxy) │  │    (through TLS MITM proxy)
          │  └────────────────────┘  │
          │                          │
          │  filesystem, processes   │
          └──────────────────────────┘
```

**How it works:** Install Node.js and `claude` (Claude Code CLI) directly into
the VM rootfs. The user attaches to the VM with an interactive terminal session
and runs `claude` as they normally would. All tool execution happens natively
inside the VM — no translation layer needed.

**Prerequisites:**
1. Implement `ExecVMStream` on the host side (agent side already works)
2. Add Node.js + Claude Code to rootfs build
3. Configure network egress to allow `api.anthropic.com`
4. Pass API key into VM (env var at creation time, or mounted secret)

**Pros:**
- Full Claude Code fidelity — every tool works exactly as upstream
- No custom agent code to write or maintain
- Gets upstream improvements for free (new tools, better context management)
- Familiar UX for anyone who has used Claude Code
- Simple architecture — just "run claude in a sandbox"

**Cons:**
- API key must be inside the VM (security concern; mitigated by VM isolation
  and short VM lifetimes)
- Larger VM footprint (Node.js ~60MB, Claude Code ~30MB, npm deps)
- Less host-side visibility — host sees network traffic but not individual
  tool calls unless you parse Claude Code's output
- Requires `ExecVMStream` implementation (currently unimplemented on host)
- Harder to add custom policies (e.g., "don't allow rm -rf /") — would
  need Claude Code hooks or wrapper scripts
- Network egress must be allowed to Anthropic API (through the existing
  MITM proxy, which can enforce allowlists)
- VM boot time increases with larger rootfs

---

## Architecture C: Agent SDK as In-VM Service

```
 User
  │
  ▼
┌──────────────────────────────────────────────────┐
│  Host                                            │
│  ┌────────────────────────────────────────┐      │
│  │  clawpot agent <vm-id> "fix the bug"  │      │
│  │  (streams output via new AgentRPC)    │      │
│  └──────────────────┬────────────────────┘      │
│                     │                            │
│  ┌──────────────────▼────────────────────┐      │
│  │  clawpot-server                       │      │
│  │  New RPC: RunAgent / AgentStream      │      │
│  │  Manages agent lifecycle, streams     │      │
│  │  output back to CLI                   │      │
│  └──────────────────┬────────────────────┘      │
│                     │ vsock                      │
└─────────────────────┼───────────────────────────┘
                      │
          ┌───────────▼──────────────┐
          │  Firecracker VM          │
          │                          │
          │  ┌────────────────────┐  │
          │  │  clawpot-agent     │  │
          │  │  (extended with    │  │
          │  │   agent harness)   │  │
          │  │                    │  │
          │  │  Python/TS runtime │  │
          │  │  Agent SDK         │  │
          │  │  ─ Bash tool       │  │
          │  │  ─ Read/Write      │  │
          │  │  ─ all local tools │  │
          │  └────────┬───────────┘  │
          │           │              │
          │  ┌────────▼───────────┐  │
          │  │  Anthropic API     │──┼──▶ api.anthropic.com
          │  └────────────────────┘  │
          │                          │
          └──────────────────────────┘
```

**How it works:** The Agent SDK runs inside the VM as part of (or alongside) the
clawpot-agent. The host sends a prompt to the VM via a new gRPC RPC, the in-VM
agent loop executes tools locally, and streams progress/output back through
vsock. The host CLI just displays the stream.

**What changes:**
1. New protobuf RPCs for agent sessions (start, stream output, cancel)
2. Agent SDK (Python or TypeScript) installed in VM rootfs
3. clawpot-agent extended to manage agent subprocess lifecycle
4. New CLI command: `clawpot agent <vm-id> "prompt"` or
   `clawpot agent --create "prompt"` (creates VM + runs agent)

**Pros:**
- Tools execute natively in VM (full fidelity, no translation)
- Clean API: host sends prompt, gets streaming results
- Can customize agent behavior via Agent SDK hooks
- Host controls lifecycle (can cancel, timeout, observe)
- Natural fit with existing gRPC architecture
- Agent SDK handles context management, tool dispatch, etc.

**Cons:**
- Larger VM footprint (Python/Node.js runtime + Agent SDK)
- API key must be in VM (same concern as Architecture B)
- More moving parts: host CLI → server → agent → SDK → API
- Agent SDK updates require rootfs rebuilds
- Two runtimes in VM: Rust clawpot-agent + Python/Node.js agent SDK

---

## Architecture D: Rust-Native Agent Loop on Host

```
 User
  │
  ▼
┌──────────────────────────────────────────────────┐
│  Host                                            │
│  ┌──────────────┐     ┌───────────────────────┐  │
│  │  CLI / TUI   │────▶│  Rust Agent Harness   │  │
│  │  (clawpot    │     │  (custom agentic      │  │
│  │   agent cmd) │◀────│   loop using raw      │  │
│  │              │     │   Anthropic API)       │  │
│  └──────────────┘     └───────┬───────────────┘  │
│                               │                  │
│                    ┌──────────▼──────────┐       │
│                    │ Anthropic API Client│       │
│                    │ (reqwest + serde)   │       │
│                    └──────────┬──────────┘       │
│                               │                  │
│                    ┌──────────▼──────────┐       │
│                    │  Tool Executor      │       │
│                    │  Maps tool_use to   │       │
│                    │  ExecVM calls       │       │
│                    └──────────┬──────────┘       │
│                               │ gRPC/vsock       │
└───────────────────────────────┼──────────────────┘
                                │
                    ┌───────────▼──────────────┐
                    │  Firecracker VM          │
                    │  (unchanged, no new deps)│
                    └──────────────────────────┘
```

**How it works:** Same as Architecture A, but everything is written in Rust,
matching the existing codebase. The agentic loop is built directly against the
Anthropic Messages API (HTTP + JSON). No Python/Node.js anywhere.

**Pros:**
- Single language — everything is Rust, consistent with the codebase
- No runtime dependencies in VM (smallest footprint)
- API key on host only
- Full control over every aspect of the agent loop
- Fastest possible performance (no interpreter overhead)
- Can integrate deeply with event store, network auth, VM lifecycle

**Cons:**
- Most engineering effort — must implement the entire agentic loop:
  - Message history management and context window compaction
  - Tool definition schemas
  - Tool result formatting
  - Streaming response parsing
  - Error recovery and retry logic
  - Multi-turn conversation state
- Must replicate Claude Code's tool semantics (Edit's exact-match, etc.)
- No benefit from Agent SDK improvements
- Rust is more verbose for this kind of string-heavy, JSON-heavy work

---

## Comparison Matrix

| Criterion | A: Host Agent + SDK | B: Claude Code in VM | C: Agent SDK in VM | D: Rust-Native |
|---|---|---|---|---|
| **Tool fidelity** | Medium (translation layer) | Perfect | High (SDK tools) | Medium (translation layer) |
| **API key security** | Host only | In VM | In VM | Host only |
| **VM footprint** | Minimal | Large (+Node.js) | Medium (+Python) | Minimal |
| **Engineering effort** | Low-Medium | Low (mostly infra) | Medium | High |
| **Host observability** | Full | Limited | Medium (via RPC) | Full |
| **Upstream updates** | SDK updates only | Full Claude Code | SDK updates | Manual |
| **Network control** | Natural (host-side) | Via proxy allowlist | Via proxy allowlist | Natural (host-side) |
| **Interactive UX** | Custom TUI | Native Claude Code | Custom CLI | Custom TUI |
| **Language consistency** | Mixed (Python+Rust) | N/A (Node.js in VM) | Mixed | Pure Rust |
| **Prerequisite work** | None (ExecVM works) | ExecVMStream impl | New RPCs + rootfs | None |

---

## Hybrid Approaches Worth Considering

### A+B: Host orchestration with optional attach

Combine Architecture A (for programmatic/API use) with Architecture B (for
interactive use). The host-side agent harness handles automated tasks, but
users can also `clawpot attach <vm-id>` to drop into an interactive Claude
Code session inside the same VM.

### A with file-transfer RPC

Architecture A's biggest weakness is file I/O through shell commands. Adding
dedicated `ReadFile` and `WriteFile` RPCs to the agent protocol would fix
this cleanly — binary-safe, no shell escaping, proper error handling. This
is a modest protocol extension:

```protobuf
// Added to clawpot_agent.proto
rpc ReadFile(ReadFileRequest) returns (ReadFileResponse);
rpc WriteFile(WriteFileRequest) returns (WriteFileResponse);
rpc ListFiles(ListFilesRequest) returns (ListFilesResponse);

message ReadFileRequest {
  string path = 1;
  int64 offset = 2;    // byte offset for large files
  int64 limit = 3;     // max bytes to return
}

message ReadFileResponse {
  bytes content = 1;
  int64 total_size = 2;
  bool is_binary = 3;
}
```

### C with API proxy

Run the Agent SDK in the VM but proxy API calls through the host. The host
intercepts calls to `api.anthropic.com`, injects the API key, and can
observe/log every API interaction. This keeps the API key on the host while
getting native tool execution in the VM. The existing TLS MITM proxy could
be extended to do this.

---

## Recommendation

**Start with Architecture A (Host-Side Agent + Agent SDK), enhanced with
file-transfer RPCs.**

Rationale:

1. **Lowest risk.** The ExecVM RPC already works. You can have a working
   prototype without touching the agent or server code at all — just a Python
   script that calls the Agent SDK with custom MCP tools.

2. **Best security posture.** API key never enters the VM. Every tool call
   is visible and auditable on the host. Network authorization is enforced
   naturally.

3. **Smallest VM changes.** The VM rootfs stays lean. No new runtimes to
   install, no new attack surface.

4. **Clear upgrade path.** Once the prototype works, you can:
   - Add file-transfer RPCs for better I/O performance
   - Implement ExecVMStream for long-running commands
   - Add a TUI for richer interactive use
   - Layer on Architecture B for users who want raw Claude Code

5. **The tool fidelity gap is manageable.** The Agent SDK lets you define
   custom tools with arbitrary implementations. You don't need to match
   Claude Code's exact tool names — you define your own tools that Claude
   learns to use from their descriptions. A `run_command`, `read_file`,
   `write_file`, `search_files`, and `edit_file` tool set covers 95% of
   coding agent use cases.

### Suggested Implementation Phases

**Phase 1: Proof of concept (Python script)**
- Python script using Claude Agent SDK
- Define MCP tools: `run_command`, `read_file`, `write_file`, `list_files`,
  `search_files`
- Each tool calls `clawpot exec <vm-id> -- ...` via subprocess
- Run from host, targeting an existing VM
- Validates the approach end-to-end

**Phase 2: Integrated CLI command**
- New `clawpot agent` subcommand
- Embeds the agent harness (either as Python subprocess or rewritten in Rust)
- `clawpot agent <vm-id> "prompt"` — run agent against existing VM
- `clawpot agent --create "prompt"` — create VM + run agent
- Streaming output to terminal

**Phase 3: File-transfer RPCs**
- Add `ReadFile`, `WriteFile`, `ListFiles` RPCs to agent protocol
- Implement in clawpot-agent (Rust, straightforward)
- Update tool implementations to use RPCs instead of shell commands
- Binary-safe, proper error codes, pagination for large files

**Phase 4: ExecVMStream + long-running commands**
- Implement `ExecVMStream` on the host side (agent side already done)
- Enable streaming output for long builds, test runs, etc.
- Add timeout and cancellation support

**Phase 5: Session management + event integration**
- Agent session tracking in event store
- `agent.session.started`, `agent.tool.called`, `agent.session.completed`
- Timeline shows agent reasoning interleaved with VM operations
- Session resume (persist conversation state)

**Phase 6 (optional): Interactive mode**
- `clawpot agent -i <vm-id>` — interactive multi-turn conversation
- TUI with tool call visualization
- Or: install Claude Code in VM for native interactive experience
  (Architecture B as an alternative mode)
