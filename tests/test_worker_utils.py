"""Offline tests for auth env setup, bootstrap rendering, and tqdm carriage-return compaction."""

from __future__ import annotations

import ast
import json
import os
import re
from pathlib import Path

from kaggle_mcp.config import Settings, configure_kaggle_env
from kaggle_mcp.launcher import build_bootstrap_bundle


def test_configure_kaggle_env_distinguishes_kgat_and_legacy(monkeypatch) -> None:
    monkeypatch.setenv("KAGGLE_KEY", "old_value")
    monkeypatch.delenv("KAGGLE_API_TOKEN", raising=False)

    configure_kaggle_env("alice", "KGAT_secret123")
    assert os.environ["KAGGLE_USERNAME"] == "alice"
    assert os.environ["KAGGLE_API_TOKEN"] == "KGAT_secret123"
    assert "KAGGLE_KEY" not in os.environ

    configure_kaggle_env("bob", "0123456789abcdef0123456789abcdef")
    assert os.environ["KAGGLE_USERNAME"] == "bob"
    assert os.environ["KAGGLE_KEY"] == "0123456789abcdef0123456789abcdef"


def test_bootstrap_bundle_renders_valid_python_and_metadata(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr("kaggle_mcp.launcher.STATE_DIR", tmp_path)
    settings = Settings(kaggle_username="tester", kaggle_secret="KGAT_dummy")

    bundle = build_bootstrap_bundle(
        settings=settings,
        session_id="sess123",
        worker_token="tok456",
        gpu=True,
        idle_timeout_min=25,
        mount_datasets=["tester/my-data"],
        chain_previous_output=True,
    )

    worker_src = (bundle / "bootstrap_worker.py").read_text(encoding="utf-8")
    assert "__BOOTSTRAP_CONFIG_B64__" not in worker_src
    compile(worker_src, "bootstrap_worker.py", "exec")

    meta = json.loads((bundle / "kernel-metadata.json").read_text(encoding="utf-8"))
    assert meta["id"] == "tester/kaggle-mcp-worker"
    assert meta["enable_gpu"] is True
    assert meta["machine_shape"] == "NvidiaTeslaT4"
    assert meta["is_private"] is True
    assert meta["dataset_sources"] == ["tester/my-data"]
    assert meta["kernel_sources"] == ["tester/kaggle-mcp-worker"]


def test_clean_tqdm_collapses_carriage_returns() -> None:
    worker_file = (
        Path(__file__).resolve().parents[1]
        / "src"
        / "kaggle_mcp"
        / "remote"
        / "worker_template.py"
    )
    tree = ast.parse(worker_file.read_text(encoding="utf-8"))
    ns: dict = {"re": re, "ANSI_CURSOR_RE": re.compile(r"\x1b\[[0-9;?]*[ABCDEFGHJKSTfhlm]")}
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name == "clean_tqdm":
            mod = ast.Module(body=[node], type_ignores=[])
            exec(compile(mod, str(worker_file), "exec"), ns)
            break

    clean_tqdm = ns["clean_tqdm"]
    noisy = "Epoch 1\r\n10%|#   |\r50%|##### |\r100%|##########| 10/10 [00:02]\r\nloss=0.12"
    assert clean_tqdm(noisy) == "Epoch 1\n100%|##########| 10/10 [00:02]\nloss=0.12"
