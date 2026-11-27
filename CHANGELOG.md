# Changelog

All notable changes to **`kaggle-shell-mcp`** (`kgsh`) will be documented in this file.

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
