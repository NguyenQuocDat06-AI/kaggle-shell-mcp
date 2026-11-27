"""Tests for worker bootstrap rendering, source validation, provisioning resume, and 3-tier shutdown."""

from __future__ import annotations

import base64
import json
import re
from pathlib import Path
from types import SimpleNamespace

import pytest

from kaggle_mcp import launcher
from kaggle_mcp.client import WorkerClient
from kaggle_mcp.config import (
    DEFAULT_IDLE_TIMEOUT_MIN,
    ActiveSession,
    Settings,
    load_active_session,
    save_active_session,
)
from kaggle_mcp.launcher import (
    build_bootstrap_bundle,
    launch_workers_multi,
    validate_push_response,
)


def test_bootstrap_bundle_renders_slot_accelerator_models_and_competition(
    isolated_state_dir: Path,
) -> None:
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


def test_validate_push_response_catches_invalid_sources() -> None:
    ok_resp = SimpleNamespace(
        error=None,
        version_number=7,
        invalid_competition_sources=[],
        invalid_dataset_sources=[],
        invalid_model_sources=[],
        invalid_kernel_sources=[],
    )
    assert validate_push_response(ok_resp) == 7

    bad_resp = SimpleNamespace(
        error=None,
        version_number=None,
        invalid_competition_sources=["unaccepted-comp"],
        invalid_dataset_sources=[],
        invalid_model_sources=["bad/model/1"],
        invalid_kernel_sources=[],
    )
    with pytest.raises(RuntimeError, match="unaccepted-comp"):
        validate_push_response(bad_resp)


def test_launch_workers_multi_runs_two_gpu_slots_in_parallel(
    sample_settings: Settings, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls: list[str] = []

    def _fake_launch(settings: Settings, *, slot: str = "1", **kwargs: object) -> dict:
        calls.append(slot)
        return {
            "slot": slot,
            "kernel_status": "RUNNING",
            "endpoint": f"https://s{slot}.trycloudflare.com",
        }

    monkeypatch.setattr(launcher, "launch_worker", _fake_launch)
    res = launch_workers_multi(sample_settings, slots=["1", "2"], accelerator="t4")

    assert res["multi_slot"] is True
    assert set(res["workers"].keys()) == {"1", "2"}
    assert res["errors"] == {}


def test_launch_worker_persists_provisioning_token_and_resumes_without_repush(
    isolated_state_dir: Path,
    sample_settings: Settings,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    push_count = [0]

    class FakeApi:
        def kernels_push(self, folder: str, **_kwargs: object):
            push_count[0] += 1
            assert (Path(folder) / "bootstrap_worker.py").is_file()
            return SimpleNamespace(
                error=None,
                version_number=3,
                invalid_competition_sources=[],
                invalid_dataset_sources=[],
                invalid_model_sources=[],
                invalid_kernel_sources=[],
            )

    monkeypatch.setattr(launcher, "get_kaggle_api", lambda _s: FakeApi())
    monkeypatch.setattr(launcher, "ensure_state_dataset", lambda _a, _s, slot="1": "alice/kaggle-mcp-state")
    monkeypatch.setattr(
        launcher,
        "get_kernel_status",
        lambda _a, _k: {"status": "QUEUED", "failure_message": ""},
    )
    monkeypatch.setattr(launcher, "fetch_remote_state", lambda *_a, **_kw: None)
    monkeypatch.setattr(launcher.time, "sleep", lambda _sec: None)

    clock = [0.0]

    def _advancing_monotonic() -> float:
        clock[0] += 40.0
        return clock[0]

    monkeypatch.setattr(launcher.time, "monotonic", _advancing_monotonic)

    r1 = launcher.launch_worker(sample_settings, slot="1", accelerator="cpu", wait_ready_sec=1)
    assert r1["ready"] is False
    assert push_count[0] == 1
    assert list(isolated_state_dir.glob("build_bundle_*")) == []

    saved = load_active_session("1")
    assert saved is not None and saved.status == "provisioning"
    first_token = saved.worker_token
    first_sid = saved.session_id

    monkeypatch.setattr(
        launcher,
        "fetch_remote_state",
        lambda *_a, **_kw: {
            "session_id": first_sid,
            "status": "running",
            "tunnel_url": "https://resumed.trycloudflare.com",
        },
    )
    monkeypatch.setattr(
        launcher,
        "probe_worker_ping",
        lambda url, sid: {"ok": True, "uptime_sec": 4.2} if sid == first_sid else None,
    )
    clock2 = [200.0]

    def _advancing_monotonic_2() -> float:
        clock2[0] += 5.0
        return clock2[0]

    monkeypatch.setattr(launcher.time, "monotonic", _advancing_monotonic_2)

    r2 = launcher.launch_worker(sample_settings, slot="1", accelerator="cpu", wait_ready_sec=30)
    assert r2["reused_existing"] is True
    assert r2["endpoint"] == "https://resumed.trycloudflare.com"
    assert push_count[0] == 1
    ready_sess = load_active_session("1")
    assert ready_sess is not None
    assert ready_sess.status == "ready"
    assert ready_sess.worker_token == first_token


def test_shutdown_slot_guards_running_gpu_and_supports_run_scoped_cancel(
    isolated_state_dir: Path,
    sample_settings: Settings,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bref = sample_settings.batch_run_ref("batch-1", "a1b2c3")
    sess_live = ActiveSession(
        session_id="s_live",
        worker_token="tok_live",
        kernel_ref="alice/kaggle-mcp-worker",
        tunnel_url="https://dead.trycloudflare.com",
        slot="1",
        mode="live",
        status="ready",
    )
    save_active_session(sess_live, slot="1")

    deleted_refs: list[str] = []
    pulled_calls: list[str] = []

    class FakeApi:
        def kernels_delete(self, kref: str, no_confirm: bool = True) -> None:
            deleted_refs.append(kref)

        def kernels_output(self, kref: str, path: str, force: bool = True, quiet: bool = True):
            pulled_calls.append(kref)
            out_file = Path(path) / "submission.csv"
            out_file.parent.mkdir(parents=True, exist_ok=True)
            out_file.write_text("id,y\n1,2\n", encoding="utf-8")
            return [str(out_file)], None

    monkeypatch.setattr(launcher, "get_kaggle_api", lambda _s: FakeApi())
    monkeypatch.setattr(
        launcher,
        "get_kernel_status",
        lambda _api, _kref: {"status": "RUNNING", "failure_message": ""},
    )

    def _fail_shutdown(self: WorkerClient) -> dict:
        raise RuntimeError("Tunnel HTTP 530")

    monkeypatch.setattr(WorkerClient, "shutdown", _fail_shutdown)

    res_blocked = launcher.shutdown_slot(sample_settings, slot="1", pull_outputs=True, force=False)
    assert res_blocked["stopped"] is False
    assert res_blocked["session_kept"] is True
    assert deleted_refs == []
    kept = load_active_session("1")
    assert kept is not None and kept.status == "unreachable"

    res_forced = launcher.shutdown_slot(sample_settings, slot="1", pull_outputs=True, force=True)
    assert res_forced["stopped"] is True
    assert res_forced["hard_cancelled"] is True
    assert deleted_refs == ["alice/kaggle-mcp-worker"]
    assert load_active_session("1") is None

    sess_batch = ActiveSession(
        session_id="s_batch",
        kernel_ref=bref,
        slot="batch-1",
        mode="batch",
        status="ready",
    )
    save_active_session(sess_batch, slot="batch-1")
    monkeypatch.setattr(
        launcher,
        "get_kernel_status",
        lambda _api, _kref: {"status": "COMPLETE", "failure_message": ""},
    )
    res_batch = launcher.shutdown_slot(
        sample_settings,
        slot="batch-1",
        pull_outputs=True,
        local_output_dir=str(isolated_state_dir / "pulled"),
        single_slot_dest=True,
    )
    assert res_batch["stopped"] is True
    assert pulled_calls == [bref]
    assert load_active_session("batch-1") is None
