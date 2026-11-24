"""Hardware accelerator resolution and Kaggle runtime lifetime caps."""

from __future__ import annotations

import re
from dataclasses import dataclass

MAX_GPU_LIFETIME_SEC = 42000  # 11h 40m safe exit before Kaggle 12h GPU/CPU reaper
GPU_DRAIN_AT_SEC = 41700      # 11h 35m SIGTERM checkpoint window

MAX_TPU_LIFETIME_SEC = 31200  # 8h 40m safe exit before Kaggle 9h TPU reaper
TPU_DRAIN_AT_SEC = 30900      # 8h 35m SIGTERM checkpoint window


@dataclass(frozen=True)
class AcceleratorSpec:
    enable_gpu: bool
    enable_tpu: bool
    machine_shape: str | None
    max_lifetime_sec: int = MAX_GPU_LIFETIME_SEC
    drain_at_sec: int = GPU_DRAIN_AT_SEC

    @property
    def label(self) -> str:
        if self.machine_shape:
            return self.machine_shape
        if self.enable_tpu:
            return "TPU"
        if self.enable_gpu:
            return "GPU"
        return "CPU"


_GPU_ALIASES: dict[str, str] = {
    "t4": "NvidiaTeslaT4",
    "t4x2": "NvidiaTeslaT4",
    "teslat4": "NvidiaTeslaT4",
    "nvidiateslat4": "NvidiaTeslaT4",
    "l4": "NvidiaL4",
    "l4x1": "NvidiaL4",
    "nvidial4": "NvidiaL4",
    "nvidial4x1": "NvidiaL4",
    "l4x4": "NvidiaL4x4",
    "nvidial4x4": "NvidiaL4x4",
    "rtx6000": "NvidiaRtxPro6000",
    "rtxpro6000": "NvidiaRtxPro6000",
    "pro6000": "NvidiaRtxPro6000",
    "nvidiartxpro6000": "NvidiaRtxPro6000",
    "h100": "NvidiaH100",
    "nvidiah100": "NvidiaH100",
    "p100": "NvidiaTeslaP100",
    "nvidiateslap100": "NvidiaTeslaP100",
}

_TPU_ALIASES: dict[str, str] = {
    "tpu": "TpuV5E8",
    "tpuv5e8": "TpuV5E8",
    "v5e8": "TpuV5E8",
    "tpuvmv5e8": "TpuV5E8",
    "tpuv6e8": "TpuV6E8",
    "v6e8": "TpuV6E8",
}

_CPU_ALIASES = {"none", "cpu", "off", "false", "no"}


def resolve_accelerator(accelerator: str | None = None, gpu: bool = True) -> AcceleratorSpec:
    """Normalize user/agent accelerator aliases into Kaggle kernel metadata flags."""
    raw = (accelerator or "").strip()
    norm = re.sub(r"[\s_-]+", "", raw.lower())

    if norm in _CPU_ALIASES or (not raw and not gpu):
        return AcceleratorSpec(
            enable_gpu=False,
            enable_tpu=False,
            machine_shape=None,
            max_lifetime_sec=MAX_GPU_LIFETIME_SEC,
            drain_at_sec=GPU_DRAIN_AT_SEC,
        )

    if not raw and gpu:
        return AcceleratorSpec(
            enable_gpu=True,
            enable_tpu=False,
            machine_shape="NvidiaTeslaT4",
            max_lifetime_sec=MAX_GPU_LIFETIME_SEC,
            drain_at_sec=GPU_DRAIN_AT_SEC,
        )

    if norm in _TPU_ALIASES:
        return AcceleratorSpec(
            enable_gpu=False,
            enable_tpu=True,
            machine_shape=_TPU_ALIASES[norm],
            max_lifetime_sec=MAX_TPU_LIFETIME_SEC,
            drain_at_sec=TPU_DRAIN_AT_SEC,
        )

    if norm in _GPU_ALIASES:
        return AcceleratorSpec(
            enable_gpu=True,
            enable_tpu=False,
            machine_shape=_GPU_ALIASES[norm],
            max_lifetime_sec=MAX_GPU_LIFETIME_SEC,
            drain_at_sec=GPU_DRAIN_AT_SEC,
        )

    if norm.startswith("tpu"):
        return AcceleratorSpec(
            enable_gpu=False,
            enable_tpu=True,
            machine_shape=raw,
            max_lifetime_sec=MAX_TPU_LIFETIME_SEC,
            drain_at_sec=TPU_DRAIN_AT_SEC,
        )

    # Passthrough unknown future GPU shapes (e.g. NvidiaB200) without requiring code updates
    return AcceleratorSpec(
        enable_gpu=True,
        enable_tpu=False,
        machine_shape=raw,
        max_lifetime_sec=MAX_GPU_LIFETIME_SEC,
        drain_at_sec=GPU_DRAIN_AT_SEC,
    )
