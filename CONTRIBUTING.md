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
```

> Note: Unit and MCP protocol tests in `tests/` do **not** require live Kaggle GPU quota and run offline in seconds.

## Design Principles

1. **Zero-Setup First**: Everything must work out of the box with only `KAGGLE_USERNAME` and `KAGGLE_KEY` (`KGAT_...` or legacy hex key). Optional transports (such as Tailscale) must remain strictly opt-in.
2. **Stdlib-Only Remote Worker**: `src/kaggle_mcp/remote/worker_template.py` runs directly inside the Kaggle Python image without requiring `apt-get` or extra system packages, preserving 100% of pre-configured CUDA/Conda environment variables.
3. **Stdio Purity**: Never write diagnostic text to `sys.stdout` inside library modules—MCP `stdio` uses `stdout` exclusively for JSON-RPC frames.
4. **Quota Respect**: Any changes to worker lifecycle logic must preserve both the `11h40m` graceful drain (`exit 0`) and the idle GPU auto-shutdown watchdog.
