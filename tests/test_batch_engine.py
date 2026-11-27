"""Tests for offline competition runner template, artifact harvesting, and bounded Host SSE streaming."""

from __future__ import annotations

import base64
import json
import threading
import time
from pathlib import Path

from conftest import load_offline_runner_symbols

from kaggle_mcp.batch import (
    OFFLINE_RUNNER_TEMPLATE,
    clean_log_text,
    collect_stream_window,
    merge_sse_log_snapshot,
)


class _InfiniteSSEStream:
    def __init__(self) -> None:
        self.closed = False
        self._step = 0

    def __iter__(self) -> _InfiniteSSEStream:
        return self

    def __next__(self) -> dict[str, str]:
        self._step += 1
        if self._step == 1:
            return {"data": json.dumps({"stream_name": "stdout", "data": "Epoch 1/3\n"})}
        if self._step == 2:
            return {"data": "10%|#|\r100%|##########| 50/50\n"}
        time.sleep(10.0)
        return {"data": "never reached\n"}

    def close(self) -> None:
        self.closed = True


def test_offline_runner_template_compiles_cleanly() -> None:
    cfg = {
        "command": "torchrun --nproc_per_node=4 train.py",
        "code_dataset_slug": "kgsh-code-batch-1",
        "top_entry": "my_proj",
        "wheel_slugs": ["kgsh-wheels"],
        "max_lifetime_sec": 42000,
        "drain_at_sec": 41700,
    }
    b64 = base64.b64encode(json.dumps(cfg).encode("utf-8")).decode("ascii")
    rendered = OFFLINE_RUNNER_TEMPLATE.replace("__BATCH_CONFIG_B64__", b64)
    assert "__BATCH_CONFIG_B64__" not in rendered
    compile(rendered, "offline_runner.py", "exec")


def test_batch_auto_harvest_copies_new_and_modified_outputs(tmp_path: Path) -> None:
    fake_working = tmp_path / "kaggle_working"
    fake_tmp_ws = tmp_path / "tmp_kgsh_batch_work"
    fake_working.mkdir()
    fake_tmp_ws.mkdir()

    ns = load_offline_runner_symbols(
        "_SKIP_HARVEST_DIRS",
        "_SKIP_HARVEST_SUFFIXES",
        "_snapshot_tree",
        "harvest_workspace_outputs",
        working_dir=fake_working,
    )

    (fake_tmp_ws / "train.py").write_text("print('train')\n", encoding="utf-8")
    (fake_tmp_ws / "configs").mkdir()
    (fake_tmp_ws / "configs" / "exp.yaml").write_text("lr: 1e-4\n", encoding="utf-8")
    baseline = ns["_snapshot_tree"](fake_tmp_ws)

    (fake_tmp_ws / "submission.csv").write_text("id,pred\n1,0.9\n", encoding="utf-8")
    (fake_tmp_ws / "checkpoints").mkdir()
    (fake_tmp_ws / "checkpoints" / "fold0.pt").write_bytes(b"PT_WEIGHTS")
    (fake_tmp_ws / "__pycache__").mkdir()
    (fake_tmp_ws / "__pycache__" / "train.cpython-312.pyc").write_bytes(b"\x00\x01")

    copied = ns["harvest_workspace_outputs"](fake_tmp_ws, baseline)
    assert copied == 2
    assert (fake_working / "submission.csv").read_text(encoding="utf-8") == "id,pred\n1,0.9\n"
    assert (fake_working / "checkpoints" / "fold0.pt").read_bytes() == b"PT_WEIGHTS"
    assert not (fake_working / "train.py").exists()
    assert not (fake_working / "__pycache__").exists()


def test_collect_stream_window_breaks_cleanly_on_iterators_and_generators() -> None:
    stream = _InfiniteSSEStream()
    t0 = time.monotonic()
    raw, eof = collect_stream_window(stream, wait_sec=0.8, idle_break_sec=0.3)
    elapsed = time.monotonic() - t0

    assert elapsed < 2.5
    assert eof is False
    assert stream.closed is True
    assert clean_log_text(raw).splitlines() == ["Epoch 1/3", "100%|##########| 50/50"]

    # Running Python generator blocked inside iter_lines() has its underlying HTTP socket closed
    closed_flag = threading.Event()

    class FakeHttpResponse:
        def iter_lines(self):
            yield '{"data": "first line\\n"}'
            closed_flag.wait(timeout=5.0)

        def close(self) -> None:
            closed_flag.set()

    def _kaggle_sse_gen():
        resp = FakeHttpResponse()
        for ln in resp.iter_lines():
            yield {"data": ln}

    raw_gen, _eof_gen = collect_stream_window(_kaggle_sse_gen(), wait_sec=0.6, idle_break_sec=0.2)
    assert "first line" in raw_gen
    assert closed_flag.is_set() is True


def test_merge_sse_log_snapshot_prevents_epoch_duplication() -> None:
    snap1 = merge_sse_log_snapshot("", "Epoch 1\nloss=0.9\n")
    snap2 = merge_sse_log_snapshot(snap1, "Epoch 1\nloss=0.9\nEpoch 2\nloss=0.4\n")
    assert snap2 == "Epoch 1\nloss=0.9\nEpoch 2\nloss=0.4\n"

    snap3 = merge_sse_log_snapshot(snap2, "Epoch 2\nloss=0.4\nEpoch 3\nloss=0.2\n")
    assert snap3 == "Epoch 1\nloss=0.9\nEpoch 2\nloss=0.4\nEpoch 3\nloss=0.2\n"
