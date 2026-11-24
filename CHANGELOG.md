# Changelog

All notable changes to **`kaggle-shell-mcp`** (`kgsh`) will be documented in this file.

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
