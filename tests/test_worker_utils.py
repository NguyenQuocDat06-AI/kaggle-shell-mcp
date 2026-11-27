"""Offline tests for auth env setup, multi-slot bootstrap rendering, and tqdm compaction."""

from __future__ import annotations

import ast
import base64
import json
import os
import re
from pathlib import Path

from kaggle_mcp import config
from kaggle_mcp.client import (
    WorkerClient,
    WorkerSessionStoppedError,
    summarize_http_error,
)
from kaggle_mcp.config import (
    DEFAULT_IDLE_TIMEOUT_MIN,
    ActiveSession,
    Settings,
    _read_kaggle_json_fallback,
    configure_kaggle_env,
    load_active_session,
    save_active_session,
)
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


def test_bootstrap_bundle_renders_slot_accelerator_models_and_competition(
    tmp_path: Path, monkeypatch
) -> None:
    monkeypatch.setattr("kaggle_mcp.launcher.STATE_DIR", tmp_path)
    settings = Settings(kaggle_username="tester", kaggle_secret="KGAT_dummy")

    bundle = build_bootstrap_bundle(
        settings=settings,
        session_id="sess123",
        worker_token="tok456",
        gpu=True,
        idle_timeout_min=DEFAULT_IDLE_TIMEOUT_MIN,
        mount_datasets=["tester/my-data"],
        chain_previous_output=True,
        slot="2",
        accelerator="rtx6000",
        competition="arc-prize-2026",
        mount_models=["google/gemma-2/transformers/9b/2"],
    )

    worker_src = (bundle / "bootstrap_worker.py").read_text(encoding="utf-8")
    assert "__BOOTSTRAP_CONFIG_B64__" not in worker_src
    compile(worker_src, "bootstrap_worker.py", "exec")
    m = re.search(r'BOOTSTRAP_CONFIG_B64 = "([^"]+)"', worker_src)
    assert m is not None
    decoded_cfg = json.loads(base64.b64decode(m.group(1)).decode("utf-8"))
    assert decoded_cfg["idle_timeout_sec"] == 0
    assert decoded_cfg["slot"] == "2"
    assert decoded_cfg["state_dataset_ref"] == "tester/kaggle-mcp-state-2"

    meta = json.loads((bundle / "kernel-metadata.json").read_text(encoding="utf-8"))
    assert meta["id"] == "tester/kaggle-mcp-worker-2"
    assert meta["enable_gpu"] is True
    assert meta["enable_tpu"] is False
    assert meta["machine_shape"] == "NvidiaRtxPro6000"
    assert meta["is_private"] is True
    assert meta["dataset_sources"] == ["tester/my-data"]
    assert meta["competition_sources"] == ["arc-prize-2026"]
    assert meta["model_sources"] == ["google/gemma-2/transformers/9b/2"]
    assert meta["kernel_sources"] == ["tester/kaggle-mcp-worker-2"]


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


def test_kaggle_json_fallback_for_pypi_installs(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("KAGGLE_CONFIG_DIR", str(tmp_path))
    (tmp_path / "kaggle.json").write_text(
        json.dumps({"username": "pypi_user", "key": "KGAT_from_json"}),
        encoding="utf-8",
    )
    user, secret = _read_kaggle_json_fallback()
    assert user == "pypi_user"
    assert secret == "KGAT_from_json"


def test_summarize_http_error_sanitizes_cloudflare_530_html() -> None:
    html_530 = (
        "<!doctype html>\n"
        "<!--[if lt IE 7]> <html class=\"no-js ie6 oldie\"> <![endif]-->\n"
        "<html><head><title>Cloudflare Tunnel error | demo.trycloudflare.com | Cloudflare</title>"
        "</head><body><h1>Origin unavailable</h1></body></html>"
    )
    msg = summarize_http_error("1", 530, html_530)
    assert "\n" not in msg
    assert "HTTP 530" in msg
    assert "tunnel closed" in msg
    assert "<!doctype" not in msg.lower()
    assert "<html" not in msg.lower()


def test_refresh_endpoint_auto_clears_stopped_session(
    tmp_path: Path, monkeypatch
) -> None:
    import pytest

    monkeypatch.setattr(config, "STATE_DIR", tmp_path)
    settings = Settings(kaggle_username="tester", kaggle_secret="KGAT_dummy")
    sess = ActiveSession(
        session_id="sess_stop",
        worker_token="tok",
        kernel_ref="tester/kaggle-mcp-worker",
        tunnel_url="https://dead.trycloudflare.com",
        slot="1",
        mode="live",
        accelerator="CPU",
    )
    save_active_session(sess, slot="1")

    monkeypatch.setattr("kaggle_mcp.client.get_kaggle_api", lambda _s: object())
    monkeypatch.setattr(
        "kaggle_mcp.client.fetch_remote_state",
        lambda _a, _s, slot="1": {"session_id": "sess_stop", "status": "stopped"},
    )

    client = WorkerClient(settings, slot="1")
    with pytest.raises(WorkerSessionStoppedError):
        client._refresh_endpoint(sess)

    assert load_active_session("1") is None
