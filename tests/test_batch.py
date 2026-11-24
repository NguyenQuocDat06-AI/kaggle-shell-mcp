"""Tests for offline competition runner template, source validation, and bounded SSE log collector."""

from __future__ import annotations

import base64
import json
import time
from types import SimpleNamespace

import pytest

from kaggle_mcp.batch import OFFLINE_RUNNER_TEMPLATE, clean_log_text, collect_stream_window
from kaggle_mcp.launcher import validate_push_response


class _InfiniteSSEStream:
    def __init__(self) -> None:
        self.closed = False
        self._step = 0

    def __iter__(self):
        return self

    def __next__(self):
        self._step += 1
        if self._step == 1:
            return {"data": json.dumps({"stream_name": "stdout", "data": "Epoch 1/3\n"})}
        if self._step == 2:
            return {"data": "10%|#|\r100%|##########| 50/50\n"}
        # Simulate long-running offline training stream blocking indefinitely
        time.sleep(10.0)
        return {"data": "never reached\n"}

    def close(self) -> None:
        self.closed = True


def test_collect_stream_window_breaks_cleanly_without_hanging() -> None:
    stream = _InfiniteSSEStream()
    t0 = time.monotonic()
    raw, eof = collect_stream_window(stream, wait_sec=0.8, idle_break_sec=0.3)
    elapsed = time.monotonic() - t0

    assert elapsed < 2.5
    assert eof is False
    assert stream.closed is True
    cleaned = clean_log_text(raw)
    assert cleaned.splitlines() == ["Epoch 1/3", "100%|##########| 50/50"]


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
