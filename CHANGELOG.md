# Changelog

All notable changes to **`kaggle-shell-mcp`** (`kgsh`) will be documented in this file.

## [1.0.1] - 2026-11-28

### Added
- **Prebuilt Linux ARM64 (`aarch64`) & Pure-Rust `rustls` TUI Builds (`release-tui.yml`, `tui/Cargo.toml`)**:
  - Added static `aarch64-unknown-linux-musl` (`kgsh-tui-linux-aarch64.tar.gz`) alongside `linux-x86_64`, `windows-x86_64`, `macos-arm64`, and `macos-x86_64`, plus `KGSH_TUI_BIN` environment override.
- **SSH-Ready Clipboard (`OSC 52` + Internal `yank_buffer` + Bracketed Paste) & Multi-GPU Telemetry (`tui/`)**:
  - Preserves `Ctrl+X` / `Ctrl+C` / `Ctrl+V` 100% via an internal yank buffer on headless SSH servers without X11/Wayland while emitting Base64-whitelisted `OSC 52` sequences to the host terminal and sanitizing Bracketed Paste input.
  - Upgraded the telemetry strip to display utilization, VRAM, and `°C` across all attached GPUs (`GPU0`, `GPU1`, ...) instead of only `gpus[0]`, and added viewport scrolling to `ActionMenu`, `CommandPalette`, and `Help` modals on compact `80×24` terminals.
- **Offline Batch Auto-Harvest & Run-Scoped Batch Slugs (`batch.py`, `config.py`)**:
  - `OFFLINE_RUNNER_TEMPLATE` snapshots `/tmp/kgsh_batch_work` before launch and automatically harvests newly created or modified artifacts (`submission.csv`, `checkpoints/`) into `/kaggle/working` (`os.sync()`) on exit.
  - Batch runs now allocate isolated per-run slugs (`<user>/kgsh-batch-<slot>-<run6>`) so cancelling one batch run never deletes outputs from earlier runs.

### Fixed
- **Remote `FileEditor` Truncation & Unsaved-Edit Guards (`tui/`, `worker_template.py`)**:
  - Raised editor read cap to `256 KB` (`512 KB` on worker), forced `Read-Only` mode when a file is truncated or binary (preventing silent `>64 KB` tail loss on `Ctrl+S`), kept the editor open until `FsSaveDone` succeeds, added an `Esc` confirmation prompt on dirty buffers, and guarded `/fs/write_text` with `HTTP 412` when overwriting large files without `allow_overwrite_large`.
- **3-Tier Verified `kaggle_down`, Provisioning Token Persistence & Idempotent RPCs (`launcher.py`, `client.py`, `worker_template.py`)**:
  - `shutdown_slot` waits up to 60s for `COMPLETE` before pulling kernel outputs, supports out-of-band `kernels_delete` cancellation for ephemeral/queued runners (`--force` / `force=True`), and refuses to wipe `sessions.json` (`status="unreachable"`) if a GPU VM is still running.
  - `launch_worker` saves `ActiveSession(status="provisioning")` immediately after `kernels_push` and resumes waiting instead of pushing duplicate kernel versions after a timeout.
  - Added `X-Idempotency-Key` + `IdempotencyRegistry` and disabled blind retries on timed-out mutating `POST` requests.
- **TUI/Agent PTY Isolation, Poll Storm Fix, `STATE_DIR` Sync & Host SSE Stability (`worker_template.py`, `batch.py`, `tui/`)**:
  - Scoped `PtyManager.interrupt` strictly to the target session (`"tui"` vs `"default"`), fixed `poll_in_flight` and `poll_backoff_until` ordering in batch mode, synchronized `--state-dir` / `KAGGLE_MCP_HOME` between Python and Rust, closed underlying SSE HTTP sockets cleanly, eliminated $O(N^2)$ SSE log duplication (`merge_sse_log_snapshot`), fixed `cloudflared` QUIC -> HTTP/2 fallback counting, and bumped minimum SDK dependency to `kaggle>=2.2.0`.

### Security
- **Default Credential Filtering, Child Env Scrubbing, Sandbox & Fail-Closed SHA-256 (`sync.py`, `tui_bin.py`, `worker.rs`, `config.py`, `worker_template.py`)**:
  - Excluded `.env`, `.env.*` (keeping `.env.example`), `kaggle.json`, SSH/TLS private keys, `.netrc`, and `kaggle_pull/` directories from `fs_sync` and TUI Push by default.
  - Scrubbed `KAGGLE_API_TOKEN`, `KAGGLE_KEY`, and `TS_AUTHKEY` from child PTY/job environments, enforced `0700`/`0600` permissions on `.kaggle_mcp` files on POSIX, blocked writes to `/tmp/kaggle_mcp_runtime`, hardened archive extractors against sibling-prefix/symlink escapes and `>2 GiB` Tar Bombs, and switched `tui_bin.py` to Fail-Closed `SHA256SUMS.txt` verification.

## [1.0.0] - 2026-11-27

### Added
- **Prebuilt Rust TUI Distribution & Auto-Downloader (`tui_bin.py`, `release-tui.yml`)**:
  - `kgsh ui` automatically locates a local/PATH `kgsh-tui` executable or downloads the matching prebuilt Windows/Linux/macOS binary from GitHub Releases into `~/.kaggle_mcp/bin/` with `SHA256SUMS.txt` verification—zero Rust setup required for PyPI/`uv tool` users.
- **Native Rust 60-FPS Dual-Panel TUI Cockpit (`tui/` -> `kgsh ui` / `kgsh top`)**:
  - Replaced legacy Python Textual UI with a native `ratatui` + `tokio` + `reqwest` binary (`kgsh-tui`) featuring 4 instant dual-panel presets (`F1 Cockpit`, `F2 Files`, `F3 Jobs`, `F4 Hub`), `NAV`/`INPUT` modes, 100% pane zoom (`z`), collapsible telemetry, Yazi-style arrow file navigation, cross-panel clipboard/transfers, auto-follow preview drawer (`v`), live foreground PTY output tailing (`pty_live` on `/monitor` + `POST /term/interrupt`), clean content-only mouse/keyboard text selection (`Shift` / `Ctrl+Shift` + `↑`/`↓`/`←`/`→` + `F8` native select), and clickable navigation.
- **File Operations, Smart Inspector, Kaggle Hub & Boot Profiles (`worker_template.py`, `client.py`, `kaggle_hub.py`, `profiles.py`, `bridge.py`)**:
  - Added `/fs/list`, `/fs/read_text`, `/fs/write_text`, `/fs/mkdir`, `/fs/rename`, `/fs/copy`, `/fs/delete`, `/fs/du`, `/fs/find`, `/fs/inspect` (CSV/Parquet/Archive/Safetensors/PyTorch), `/fs/archive`, `/fs/extract`, `/fs/publish_dataset`, and `/proc/kill` with HTTP 404 PTY fallbacks on older workers.
  - Added Kaggle Hub browser (`datasets`, `my_datasets`, `competitions`, `models`, `my_kernels` logs/delete, `submit_csv`), saved boot profiles (`~/.kaggle_mcp/profiles.json`), and UTF-8 JSON bridge (`kaggle_mcp.bridge`).
- **Real-Time MCP Activity Bus & Combined `/monitor` RPC (`activity.py`, `worker_template.py`)**:
  - Added in-worker `EventBus` ring buffer tagging caller origin (`X-Kgsh-Caller: mcp | tui | shell | cli`) plus local `~/.kaggle_mcp/activity.jsonl` and `mcp_flow.jsonl` streams so `kgsh ui` displays every MCP tool action in real time.
  - Added single-roundtrip `POST /monitor` (with GPU/VRAM, RAM, disk, jobs, logs, events, and live PTY tail) and incremental byte-offset `POST /jobs/logs_chunk` endpoints (passive monitoring does not reset `idle_timeout_min`).
- **Interactive SSH-Style REPL & Live Log Follower (`repl.py`)**:
  - `kgsh shell [-s 1]`: Stateful remote bash REPL sharing `cwd` and environment variables with the MCP Agent.
  - `kgsh logs <job_id> -f`: Continuous `tail -f` streaming until job completion, plus `kgsh exec -r/--raw`.
- **Weekly Accelerator Quota Inspection (`kaggle_quota` MCP tool & `kgsh quota`, `quota.py`)**:
  - Reads native `datetime.timedelta` attributes from `KernelsApiClient.get_accelerator_quota_statistics()` to report exact weekly GPU & TPU hours remaining and reset timestamps.

## [0.2.0] - 2026-11-25

### Added
- **Multi-Slot Concurrent GPU Architecture (`slot="1"`, `"2"`, `"1,2"`)**:
  - Run up to 2 parallel headless Kaggle GPU sessions simultaneously (`4 × Tesla T4 = 60 GB VRAM`) with isolated kernel slugs, isolated handshake datasets, and a cross-process file-locked `.kaggle_mcp/sessions.json` store (auto-migrating legacy `active_session.json`).
  - Added per-job GPU pinning (`gpu_ids` / `--gpus`) in `job_spawn` via isolated `CUDA_VISIBLE_DEVICES`, enabling up to 4 concurrent single-GPU experiments across 2 Dual-T4 VMs.
  - Added `slot="all"` support in `kaggle_status` and `kaggle_down`, plus automatic stale-session cleanup when Kaggle reports terminal kernel states (`COMPLETE`, `ERROR`, `CANCEL_ACKNOWLEDGED`).
- **Full Hardware Accelerator, Competition & Model Hub Support (`accelerators.py`)**:
  - Added `accelerator` aliases and passthrough shapes (`NvidiaTeslaT4`, `NvidiaL4`, `NvidiaL4x4`, `NvidiaRtxPro6000`, `NvidiaH100`, `TpuV5E8`, `TpuV6E8`, `cpu`).
  - Automatic TPU runtime window adjustment (`8h35m` `SIGTERM` drain / `8h40m` clean exit before Kaggle's 9h TPU cap).
  - Added `competition` (`competition_sources`) and `mount_models` (`model_sources`) parameters with immediate source-validation error reporting from `ApiSaveKernelResponse`.
- **Offline Competition Runner & Live Host SSE Logs (`batch.py`)**:
  - `kaggle_wheelhouse` (`kgsh wheels`): Builds Linux `manylinux_x86_64` binary wheel datasets for air-gapped kernels.
  - `kaggle_batch_run` (`kgsh batch-run`): Runs competition jobs with `enable_internet=False` using atomic inline code embedding (`<= 512 KB`) or `.kgsh` dataset archives (`> 512 KB`, preventing Kaggle auto-decompression races).
  - `kaggle_batch_logs` (`kgsh batch-logs`): Non-blocking windowed Host SSE log reader (`kernels_logs_stream`) that streams live logs even when container internet access is disabled.

## [0.1.0] - 2026-11-24

### Added
- **Headless 12h Kaggle Dual-T4 Provisioner (`kaggle_up`, `kaggle_status`, `kaggle_down`)**: Boots background Kaggle GPU kernels (`NvidiaTeslaT4`, 2×15 GB VRAM) via Kaggle API with zero browser interaction.
- **Zero-Setup Tunnel & Private Dataset Handshake**: Automatic `cloudflared` Quick Tunnel supervisor paired with private Kaggle Dataset state sync (`<user>/kaggle-mcp-state`), plus optional Tailscale userspace networking (`TS_AUTHKEY`).
- **Stateful Remote PTY (`term_exec`)**: Persistent `/bin/bash` PTY preserving `cwd`, shell exports, Conda/venv environments, and native CUDA paths.
- **Detached Background Job Engine (`job_spawn`, `job_list`, `job_logs`, `job_kill`)**: Process-group isolated training jobs with persistent logs in `/kaggle/working/mcp_logs` and automatic `tqdm` carriage-return (`\r`) compaction.
- **Filtered Tar Streaming (`fs_sync`)**: Fast bidirectional directory/file sync ignoring `.git`, `.venv`, `__pycache__`, and `node_modules`.
- **Snapshot & Optional Idle Watchdogs**: Graceful `SIGTERM` checkpoint flush at `11h35m` + clean `exit(0)` at `11h40m` (preserving `/kaggle/working` snapshots before Kaggle's 12h reaper), plus opt-in idle GPU auto-shutdown (`idle_timeout_min`, disabled by default).
- **Dual Interface**: Full Model Context Protocol (`stdio`) server (`mcp` v1 & v2 compatible) and short `kgsh` CLI (`kaggle-shell-mcp` & `kaggle-mcp` aliases included).
