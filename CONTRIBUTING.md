# Contributing to `kaggle-shell-mcp`

Contributions, bug fixes, and transport extensions are welcome!

## Local Development Setup

```bash
git clone https://github.com/NguyenQuocDat06-AI/kaggle-shell-mcp.git
cd kaggle-shell-mcp
python -m venv .venv

# Windows
.\.venv\Scripts\pip install -e ".[dev]"

# Linux / macOS
./.venv/bin/pip install -e ".[dev]"
```

## Running Checks Before Submitting a PR

```bash
ruff check src tests
pytest -v

# Rust TUI (`tui/`)
cargo clippy --manifest-path tui/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path tui/Cargo.toml
cargo build --release --manifest-path tui/Cargo.toml
```

> Note: Unit and MCP protocol tests in `tests/` do **not** require live Kaggle GPU quota and run offline in seconds.

## Design Principles

1. **Zero-Setup First**: Everything must work out of the box with only `KAGGLE_USERNAME` and `KAGGLE_KEY` (`KGAT_...`, legacy hex key, or `~/.kaggle/kaggle.json`). Optional transports (such as Tailscale) must remain strictly opt-in.
2. **Stdlib-Only Remote Worker**: `src/kaggle_mcp/remote/worker_template.py` runs directly inside the Kaggle Python image without requiring `apt-get` or extra system packages, preserving 100% of pre-configured CUDA/Conda environment variables.
3. **Stdio Purity**: Never write diagnostic text to `sys.stdout` inside library modules—MCP `stdio` uses `stdout` exclusively for JSON-RPC frames.
4. **Safe Drain & Multi-Slot Isolation**: Any changes to worker lifecycle or local state storage must preserve the `11h40m` (`8h40m` TPU) graceful drain (`exit 0`) and OS file locking on `sessions.json`.

## Releasing to PyPI (Maintainers)

Publishing is automated via [`.github/workflows/publish.yml`](./.github/workflows/publish.yml) using PyPI **Trusted Publishing** (OIDC—no long-lived PyPI API tokens stored in GitHub Secrets):

1. Configure a Trusted Publisher once at `https://pypi.org/manage/account/publishing/`:
   - **PyPI Project Name**: `kaggle-shell-mcp`
   - **Owner**: `NguyenQuocDat06-AI`
   - **Repository name**: `kaggle-shell-mcp`
   - **Workflow name**: `publish.yml`
2. Bump the version in `pyproject.toml`, `src/kaggle_mcp/__init__.py`, and `CHANGELOG.md`.
3. Create a GitHub Release (`gh release create vX.Y.Z --generate-notes`) or trigger `gh workflow run publish.yml`.
