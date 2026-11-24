# Changelog

All notable changes to **`kaggle-shell-mcp`** will be documented in this file.

## [0.1.0] - 2026-11-24

### Added
- **Headless 12h Kaggle Dual-T4 Provisioner (`kaggle_up`, `kaggle_status`, `kaggle_down`)**: Boots background Kaggle GPU kernels (`NvidiaTeslaT4`, 2×15 GB VRAM) via Kaggle API with zero browser interaction.
- **Zero-Setup Tunnel & Private Dataset Handshake**: Automatic `cloudflared` Quick Tunnel supervisor paired with private Kaggle Dataset state sync (`<user>/kaggle-mcp-state`), plus optional Tailscale userspace networking (`TS_AUTHKEY`).
- **Stateful Remote PTY (`term_exec`)**: Persistent `/bin/bash` PTY preserving `cwd`, shell exports, Conda/venv environments, and native CUDA paths.
- **Detached Background Job Engine (`job_spawn`, `job_list`, `job_logs`, `job_kill`)**: Process-group isolated training jobs with persistent logs in `/kaggle/working/mcp_logs` and automatic `tqdm` carriage-return (`\r`) compaction.
- **Filtered Tar Streaming (`fs_sync`)**: Fast bidirectional directory/file sync ignoring `.git`, `.venv`, `__pycache__`, and `node_modules`.
- **Dual Quota & Snapshot Watchdogs**: Graceful `SIGTERM` checkpoint flush at `11h35m` + clean `exit(0)` at `11h40m` (preserving `/kaggle/working` snapshots before Kaggle's 12h reaper) and automatic idle GPU shutdown.
- **Dual Interface**: Full Model Context Protocol (`stdio`) server (`mcp` v1 & v2 compatible) and standalone `kaggle-shell-mcp` CLI.
