"""Shared pytest fixtures and AST-based runtime template loaders for offline testing."""

from __future__ import annotations

import ast
import csv
import fnmatch
import io
import json
import os
import re
import shutil
import struct
import tarfile
import threading
import time
import zipfile
from collections import deque
from pathlib import Path
from typing import Any

import pytest

from kaggle_mcp import batch, config, launcher
from kaggle_mcp.config import Settings

REPO_ROOT = Path(__file__).resolve().parents[1]
WORKER_TEMPLATE_PATH = REPO_ROOT / "src" / "kaggle_mcp" / "remote" / "worker_template.py"


@pytest.fixture
def isolated_state_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect local `.kaggle_mcp` state directory to an isolated temporary folder."""
    state_dir = tmp_path / "state"
    state_dir.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(config, "STATE_DIR", state_dir)
    monkeypatch.setattr(launcher, "STATE_DIR", state_dir)
    return state_dir


@pytest.fixture
def sample_settings() -> Settings:
    """Return deterministic offline Kaggle credentials for unit testing."""
    return Settings(kaggle_username="alice", kaggle_secret="KGAT_secret_token")


def load_worker_symbols(
    *symbol_names: str,
    extra_globals: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Compile and execute selected top-level AST nodes from `worker_template.py` without POSIX pty."""
    wanted = set(symbol_names)
    src = WORKER_TEMPLATE_PATH.read_text(encoding="utf-8")
    tree = ast.parse(src, filename=str(WORKER_TEMPLATE_PATH))

    body: list[ast.stmt] = []
    for node in tree.body:
        if isinstance(node, ast.Assign):
            targets = [t.id for t in node.targets if isinstance(t, ast.Name)]
            if any(t in wanted for t in targets):
                body.append(node)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            if node.target.id in wanted:
                body.append(node)
        elif isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in wanted:
            body.append(node)

    ns: dict[str, Any] = {
        "ANSI_CURSOR_RE": re.compile(r"\x1b\[[0-9;?]*[ABCDEFGHJKSTfhlm]"),
        "Path": Path,
        "csv": csv,
        "deque": deque,
        "fnmatch": fnmatch,
        "io": io,
        "json": json,
        "os": os,
        "re": re,
        "shutil": shutil,
        "struct": struct,
        "tarfile": tarfile,
        "threading": threading,
        "time": time,
        "zipfile": zipfile,
    }
    if extra_globals:
        ns.update(extra_globals)

    mod = ast.Module(body=body, type_ignores=[])
    exec(compile(mod, str(WORKER_TEMPLATE_PATH), "exec"), ns)
    return ns


def load_offline_runner_symbols(
    *symbol_names: str,
    working_dir: Path,
) -> dict[str, Any]:
    """Compile and execute selected top-level AST nodes from `batch.OFFLINE_RUNNER_TEMPLATE`."""
    wanted = set(symbol_names)
    src = batch.OFFLINE_RUNNER_TEMPLATE.replace(
        'Path("/kaggle/working")', f"Path({str(working_dir)!r})"
    )
    tree = ast.parse(src, filename="offline_runner.py")

    body: list[ast.stmt] = []
    for node in tree.body:
        if isinstance(node, ast.Assign):
            targets = [t.id for t in node.targets if isinstance(t, ast.Name)]
            if any(t in wanted for t in targets):
                body.append(node)
        elif isinstance(node, ast.FunctionDef) and node.name in wanted:
            body.append(node)

    ns: dict[str, Any] = {"Path": Path, "os": os, "shutil": shutil}
    exec(compile(ast.Module(body=body, type_ignores=[]), "offline_runner.py", "exec"), ns)
    return ns
