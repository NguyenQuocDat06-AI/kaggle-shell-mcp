# Security Policy

## Credential, Sandbox & Transport Architecture

`kaggle-shell-mcp` (`kgsh`) enforces layered security controls across local state files, archive transfers, prebuilt binary distribution, and remote Kaggle containers:

1. **Per-Session Ephemeral Auth & Owner-Only State (`0700` / `0600`)**:
   - Every `kaggle_up` run generates a fresh 256-bit `worker_token` (`secrets.token_hex(32)`) and `session_id`, verified in constant time (`hmac.compare_digest`) on every authenticated RPC call.
   - Local state directories (`.kaggle_mcp`) are created with `0700` permissions on POSIX, all state/profile/activity files (`sessions.json`, `profiles.json`, `activity.jsonl`) are written atomically with `0600` permissions, and temporary `build_bundle_*` staging folders are deleted immediately after `kernels_push`.
2. **Child-Process Secret Scrubbing**:
   - Inside the remote worker, `PtySession` and `JobManager` scrub `KAGGLE_API_TOKEN`, `KAGGLE_KEY`, and `TS_AUTHKEY` from child process environments (`_scrubbed_child_env`) so user/Agent scripts cannot accidentally print or leak account credentials.
3. **Default Credential Filtering on File Sync**:
   - Both `fs_sync(direction="push")` (`sync.py`) and TUI Push (`worker.rs`) automatically block `.env`, `.env.*` (preserving `.env.example` / `.env.sample` / `.env.template`), `kaggle.json`, `access_token`, `.netrc`, `id_rsa`, `id_ed25519`, and `.pem`/`.key`/`.p12`/`.pfx` files by default, preventing accidental upload of API keys into `/kaggle/working` snapshots.
   - `.env` and `.kaggle_mcp/` are also excluded by `.gitignore`.
4. **Hardened Filesystem Sandbox & Archive Extraction**:
   - Remote `/fs/*` endpoints enforce `_assert_writable_path`, restricting mutations strictly to `/kaggle/working` and `/tmp` while protecting `/kaggle/input` and `/tmp/kaggle_mcp_runtime`.
   - Local and remote `.tar.gz` / `.zip` extractors enforce `Path.is_relative_to` (blocking `../` and sibling-prefix traversal), reject intermediate symlink escapes, skip archive symlinks/hardlinks, and enforce a streaming `2 GiB` Tar Bomb extraction cap.
5. **Fail-Closed Prebuilt TUI Verification**:
   - When `kgsh ui` downloads prebuilt `kgsh-tui` binaries from GitHub Releases over `rustls`/HTTPS, `tui_bin.py` enforces **Fail-Closed** verification against `SHA256SUMS.txt`—refusing installation if the checksum manifest is missing, non-200, or mismatched.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities. Report security issues privately via GitHub Security Advisories on this repository or email `nquocdat06.work@gmail.com` with reproduction steps.
