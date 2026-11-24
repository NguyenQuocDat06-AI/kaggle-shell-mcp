# Security Policy

## Credential & Transport Architecture

`kaggle-shell-mcp` is designed so your Kaggle compute containers are never exposed as unauthenticated endpoints:

1. **Per-Session Ephemeral Secrets**: Every `kaggle_up` run generates a fresh 256-bit `worker_token` (`secrets.token_hex(32)`) and `session_id`, verified in constant time (`hmac.compare_digest`) on every RPC call.
2. **Private Handshake Channel**: Dynamic tunnel URLs are exchanged exclusively via a **private** Kaggle Dataset (`<username>/kaggle-mcp-state`) and a **private** Kaggle Kernel (`is_private: true`).
3. **Path-Traversal Protection**: Both local and remote `.tar.gz` extractors validate resolved destination paths and skip symlinks/hardlinks.
4. **Never Commit `.env`**: `.env` and `.kaggle_mcp/` (which caches active session tokens locally) are excluded by `.gitignore`.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities. Report security issues privately via GitHub Security Advisories on this repository or email `nquocdat06.work@gmail.com` with reproduction steps.
