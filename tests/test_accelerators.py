"""Tests for hardware accelerator alias resolution and runtime lifetime caps."""

from __future__ import annotations

from kaggle_mcp.accelerators import (
    MAX_GPU_LIFETIME_SEC,
    MAX_TPU_LIFETIME_SEC,
    resolve_accelerator,
)


def test_gpu_aliases_and_defaults() -> None:
    t4 = resolve_accelerator()
    assert t4.enable_gpu is True
    assert t4.enable_tpu is False
    assert t4.machine_shape == "NvidiaTeslaT4"
    assert t4.max_lifetime_sec == MAX_GPU_LIFETIME_SEC

    l4x4 = resolve_accelerator("l4x4")
    assert l4x4.enable_gpu is True
    assert l4x4.machine_shape == "NvidiaL4x4"

    rtx = resolve_accelerator("rtx-pro-6000")
    assert rtx.enable_gpu is True
    assert rtx.machine_shape == "NvidiaRtxPro6000"


def test_tpu_lowers_safe_drain_to_9h_window() -> None:
    tpu = resolve_accelerator("tpuv5e8")
    assert tpu.enable_gpu is False
    assert tpu.enable_tpu is True
    assert tpu.machine_shape == "TpuV5E8"
    assert tpu.max_lifetime_sec == MAX_TPU_LIFETIME_SEC


def test_cpu_and_future_passthrough() -> None:
    cpu = resolve_accelerator("cpu")
    assert cpu.enable_gpu is False
    assert cpu.enable_tpu is False
    assert cpu.machine_shape is None

    cpu2 = resolve_accelerator(None, gpu=False)
    assert cpu2.enable_gpu is False
    assert cpu2.machine_shape is None

    future = resolve_accelerator("NvidiaB200")
    assert future.enable_gpu is True
    assert future.enable_tpu is False
    assert future.machine_shape == "NvidiaB200"
