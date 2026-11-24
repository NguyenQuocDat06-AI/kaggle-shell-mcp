# Architecture of `kaggle-shell-mcp` (`kgsh`)

This document describes the container constraints of Kaggle Kernels, the dual-mode execution architecture of `kaggle-shell-mcp`, and the internal mechanisms that enable multi-slot live terminals and air-gapped competition execution over the Model Context Protocol (MCP).

---

## 1. Platform Constraints & Design Drivers

Kaggle's official `kernels_push` API executes non-interactive batch runs inside ephemeral, unprivileged Linux containers. Turning these containers into reliable, interactive GPU compute nodes for AI Agents requires designing around six hard platform constraints:

| Platform Constraint | Runtime Reality | Architectural Design in `kgsh` |
| :--- | :--- | :--- |
| **Inbound Firewall & Missing TUN** | All inbound ports are blocked; `/dev/net/tun` (`CAP_NET_ADMIN`) is unavailable | Outbound reverse tunnels (`cloudflared` Quick Tunnel by default; optional `tailscaled --tun=userspace-networking`) |
| **Hard Execution Ceilings** | **12 hours** for GPU/CPU commits; **9 hours** for TPU commits. Hard-killed runs lose `/kaggle/working` snapshots | In-container watchdog sends `SIGTERM` at `11h35m` (`8h35m` on TPU) and calls `sys.exit(0)` at `11h40m` (`8h40m` on TPU) |
| **Persistent vs. Ephemeral Disk** | `/kaggle/working` persists up to **~19.5 GB** on clean `exit(0)`; `/tmp` has larger scratch space but wipes on exit | Keep transient bundles/wheels in `/tmp`; reserve `/kaggle/working` strictly for logs, checkpoints, and final outputs |
| **CUDA Environment Inheritance** | `PATH`, `LD_LIBRARY_PATH`, and Conda paths are initialized on the kernel Python process, not system-wide `sshd` | Run a zero-dependency Python stdlib RPC + PTY daemon directly inside the kernel Python process (no `apt` / `openssh-server`) |
| **Concurrent GPU Session Limit** | Up to **2 concurrent GPU commit sessions** per Kaggle account, but a single kernel slug cannot track two live runs cleanly | Zero-contention **Slot Isolation** (`slot="1"`, `"2"`, ...) with dedicated kernel slugs, dedicated state datasets, and OS file locking |
| **Air-Gapped Competition GPUs** | High-tier competition shapes (`NvidiaL4x4`, `NvidiaRtxPro6000`) often enforce `enable_internet: false` | **Dual-Mode Engine**: falls back to offline wheelhouse + atomic code bundling and streams live logs via Kaggle's **Host SSE** endpoint |

---

## 2. System Architecture

`kaggle-shell-mcp` combines **Mode 1 (Interactive Live Worker)** for rapid R&D/debugging and **Mode 2 (Offline Competition Runner)** for air-gapped hardware inside a single MCP server and CLI.

```mermaid
flowchart TB
    subgraph Client["Local Host"]
        Agent["MCP Client (Claude / Cursor / Antigravity) or CLI (kgsh)"]
        Server["server.py — 12 MCP Tools & CLI Router\n(stdout-protected JSON-RPC)"]
        Store[".kaggle_mcp/sessions.json\n(RLock + OS File Lock: msvcrt / fcntl)"]
    end

    subgraph Mode1["Mode 1: Interactive Live Worker (Internet ON — Up to 2 Parallel GPU VMs)"]
        Boot["bootstrap_worker.py (stdlib ThreadingHTTPServer)"]
        Sup["Tunnel Supervisor\nCloudflared / Tailscale netstack"]
        StateDS["Private Handshake Dataset\n<user>/kaggle-mcp-state[-<slot>]"]
        PTY["PtyManager (/bin/bash)\nSplit-Quote Sentinel Framing"]
        Jobs["JobManager (setsid)\nCUDA_VISIBLE_DEVICES Pinning"]
        Dog["Lifecycle Watchdog\n11h40m Safe Drain + Optional Idle Guard"]
    end

    subgraph Mode2["Mode 2: Offline Competition Runner (Internet OFF — L4x4 / RTX Pro 6000 / TPU)"]
        Wheels["Private Wheelhouse Dataset\n<user>/kgsh-wheels-* (manylinux_x86_64)"]
        Offline["offline_runner.py\nInline <=512KB Tar or .kgsh Dataset"]
        HostSSE["Kaggle Host Orchestrator\n/api/v1/kernels/logs/stream (SSE)"]
    end

    Agent <-->|"MCP stdio"| Server
    Server <--> Store
    Server -->|"kernels_push"| Boot
    Boot --> Sup & PTY & Jobs & Dog
    Sup -.->|"Publish tunnel_url"| StateDS
    StateDS -.->|"Auto-healing lookup"| Server
    Server <-->|"HTTPS + 256-bit Bearer HMAC"| Sup

    Server -->|"pip download"| Wheels
    Server -->|"kernels_push (enable_internet=false)"| Offline
    Wheels --> Offline
    Offline -->|"stdout/stderr"| HostSSE
    HostSSE -->|"Bounded Window Collector"| Server
```

---

## 3. Core Mechanisms

### 3.1. Zero-Contention Multi-Slot Isolation (`config.py`, `launcher.py`)
Running two GPU sessions simultaneously on one Kaggle account fails if both sessions share a single kernel slug or state dataset. `kaggle-shell-mcp` isolates every resource by a normalized `slot` identifier (`"1"`, `"2"`, `"comp-1"`, etc.):

- **Backward-Compatible Naming**:
  - `slot="1"` (default) maps to `<user>/kaggle-mcp-worker` and `<user>/kaggle-mcp-state`.
  - Any additional `slot="<id>"` maps to `<user>/kaggle-mcp-worker-<id>` and `<user>/kaggle-mcp-state-<id>`.
  - Batch runs map to `<user>/kaggle-mcp-worker-batch[-<id>]`.
- **Parallel Provisioning (`slot="1,2"`)**: `launch_workers_multi` uses a thread pool with per-slot staging directories (`.kaggle_mcp/build_bundle_<slot>`) so concurrent `kernels_push` calls never overwrite each other's rendered scripts or `kernel-metadata.json`.
- **Cross-Process Atomic State Store**: Access to `.kaggle_mcp/sessions.json` is serialized by both an in-process `threading.RLock` and an OS-level byte-range/advisory lock (`msvcrt.locking` on Windows, `fcntl.flock` on POSIX) plus atomic temp-file rename (`os.replace`).

### 3.2. Zero-Setup Tunnel & Auto-Healing Handshake (`worker_template.py`, `client.py`)
1. On `kaggle_up`, the local launcher generates a fresh 12-byte `session_id` and a 32-byte (256-bit) `worker_token`, base64-encodes the bootstrap configuration into `worker_template.py`, and pushes the private script kernel.
2. Inside Kaggle, `cloudflared_supervisor` downloads the static `cloudflared-linux-amd64` binary into `/tmp`, opens a tunnel to `http://127.0.0.1:8765`, parses the generated `https://*.trycloudflare.com` URL, and publishes `state.json` to the slot's **private** Kaggle Dataset (`<user>/kaggle-mcp-state[-<slot>]`).
3. If `cloudflared` drops mid-session during a 12-hour run, the supervisor respawns it within 3 seconds and publishes the new URL. On the local side, `WorkerClient` detects `ConnectError` / `502` / `530`, refreshes `state.json` from the private Dataset, verifies `/ping` matches `session_id`, updates `sessions.json`, and transparently retries the RPC.

### 3.3. Stateful PTY Execution & Echo-Immune Sentinel Framing (`worker_template.py`)
Agent tool calls need persistent shell state (`cd`, `export`, activated virtual environments) without hanging indefinitely or misreading echoed input:
- `PtySession` allocates a Linux pseudo-terminal (`pty.openpty()`) configured to `40×160` (`TIOCSWINSZ`) with `stty -echo` and `TERM=dumb`.
- Every command appends a split-quote `printf` sentinel:
  ```bash
  __mcp_ec=$?; printf '\n__MCP_END_%s__:%d:%s\n' '<half1>''<half2>' "$__mcp_ec" "$PWD"
  ```
  Because the input contains single quotes splitting the UUID in half while `printf` emits the concatenated UUID, terminal echo (even if re-enabled by a child binary) can never prematurely trigger sentinel detection.
- If a foreground command exceeds `timeout_sec`, `PtySession` sends `SIGINT` (`\x03`), drains remaining bytes, and returns partial output with `timed_out=True` so the shell remains usable for subsequent calls.

### 3.4. Detached GPU Jobs & Context-Window Protection (`worker_template.py`)
Long training runs should never be tied to an HTTP request lifetime:
- `JobManager.spawn` starts commands in a new process group (`start_new_session=True`), optionally pinning `CUDA_VISIBLE_DEVICES=<gpu_ids>` per job, and redirects output to `/kaggle/working/mcp_logs/<job_id>.log`.
- Combining `slot="1,2"` (`2 VMs × 2 Tesla T4 GPUs`) with `gpu_ids="0"` / `"1"` lets an Agent run **4 isolated 15 GB GPU jobs simultaneously**; `job_kill` signals the entire process group (`os.killpg`) so child `torchrun` workers release VRAM cleanly.
- `clean_tqdm` collapses intermediate carriage-return (`\r`) frames and strips ANSI cursor-movement sequences before returning logs over MCP, preventing `tqdm` progress bars from flooding LLM context windows.

### 3.5. Offline Competition Engine & Bounded Host SSE (`batch.py`)
When targeting restricted competition accelerators (`NvidiaL4x4`, `NvidiaRtxPro6000`) where `enable_internet: false` blocks outbound tunnels:
1. **Linux Wheelhouse (`kaggle_wheelhouse`)**: Downloads `manylinux2014_x86_64` / `manylinux_2_28_x86_64` binary `.whl` files matching the target CPython ABI (`cp312` / `cp313`) from any host OS (including Windows) and uploads them as a reusable private Kaggle Dataset.
2. **Atomic Code Delivery (`kaggle_batch_run`)**:
   - Source bundles `<= 512 KB` compressed are embedded directly inside `offline_runner.py` as base64—eliminating dataset creation latency and indexing races completely.
   - Larger bundles (`> 512 KB`) are uploaded to a private Dataset with a `.kgsh` extension (preventing Kaggle's GCS ingestion pipeline from auto-extracting `.tar.gz` archives) and verified via `dataset_list_files` before pushing the kernel.
   - At runtime, `offline_runner.py` extracts code into `/tmp/kgsh_batch_work` so `/kaggle/working`'s 20 GB output quota remains untouched for checkpoints and `submission.csv`.
3. **Bounded Host SSE Log Reader (`collect_stream_window`)**:
   - Kaggle's host-level `GET /api/v1/kernels/logs/stream/{owner}/{slug}` streams container stdout over Server-Sent Events (`text/event-stream`) even when container networking is disabled, but blocks until kernel completion.
   - `collect_stream_window` runs the SSE generator on a daemon thread feeding a `queue.Queue` with a bounded window (`wait_sec`) and early idle break (`idle_break_sec`), then closes the generator (`response.close()`) and caches cumulative output locally—providing instant, non-blocking live logs to MCP clients.

### 3.6. Stdout Purity & Security Boundaries
- **MCP `stdio` Integrity**: Third-party SDKs (`kaggle` / `kagglesdk`) occasionally print warnings or URLs to `sys.stdout`, which would corrupt MCP JSON-RPC frames. Every SDK import and API call in `launcher.py` and `batch.py` executes inside `contextlib.redirect_stdout(sys.stderr)`.
- **Constant-Time Auth & Safe Extraction**: All worker endpoints (except minimal `/ping` session-ID probes) verify `Authorization: Bearer <worker_token>` via `hmac.compare_digest`. Both local and remote tar extractors enforce resolved path-prefix checks and skip symlinks/hardlinks (`filter="data"`).

---

## 4. Module Map

```text
kaggle-shell-mcp/
├── pyproject.toml                     # Package metadata & CLI entrypoints (kgsh, kaggle-shell-mcp, kaggle-mcp)
├── ARCHITECTURE.md                    # System architecture & protocol internals
├── README.md                          # English quickstart & reference
├── docs/README.vi.md                  # Vietnamese quickstart & reference
├── src/kaggle_mcp/
│   ├── __init__.py                    # Version & package metadata
│   ├── accelerators.py                # Hardware alias resolver (T4, L4x4, RTX Pro 6000, H100, TPU, CPU) & caps
│   ├── config.py                      # .env loader, KGAT_ token setup, file-locked multi-slot sessions.json
│   ├── launcher.py                    # Multi-slot live bootstrap builder, source validator & parallel launcher
│   ├── client.py                      # Slotted HTTPS RPC client with auto-healing tunnel refresh
│   ├── batch.py                       # Offline wheelhouse builder, batch runner & bounded Host SSE log reader
│   ├── sync.py                        # Filtered .tar.gz packer/unpacker with path-traversal guards
│   ├── server.py                      # 12 MCP tools + standalone kgsh CLI parser
│   └── remote/
│       └── worker_template.py         # Zero-dep stdlib remote daemon (PTY, JobManager, tunnels & watchdogs)
└── tests/                             # Offline pytest suite (accelerators, multi-slot locking, SSE, sync, stdio)
```
