"""Weekly Kaggle GPU & TPU quota inspection via kagglesdk native timedelta attributes."""

from __future__ import annotations

import re
from datetime import datetime
from typing import Any

from .config import Settings
from .launcher import get_kaggle_api, protect_stdout


def _td_seconds(val: Any) -> float:
    """Convert kagglesdk timedelta / numeric / string duration safely to float seconds."""
    if val is None:
        return 0.0
    if hasattr(val, "total_seconds"):
        return max(0.0, float(val.total_seconds()))
    if isinstance(val, (int, float)):
        return max(0.0, float(val))
    if isinstance(val, str):
        m = re.match(r"^(\d+(?:\.\d+)?)", val.strip())
        if m:
            return max(0.0, float(m.group(1)))
    return 0.0


def parse_quota_bucket(bucket: Any) -> dict[str, Any]:
    """Parse an ApiAcceleratorQuota object without triggering kagglesdk's broken to_dict()."""
    if bucket is None:
        return {
            "used_hours": 0.0,
            "reserved_hours": 0.0,
            "total_hours": 0.0,
            "remaining_hours": 0.0,
            "used_pct": 0.0,
            "over_quota": False,
        }

    used_s = _td_seconds(getattr(bucket, "time_used", 0))
    reserved_s = _td_seconds(getattr(bucket, "time_reserved", 0))
    total_s = _td_seconds(getattr(bucket, "total_time_allowed", 0))
    rem_s = max(0.0, total_s - used_s - reserved_s)
    pct = round(min(100.0, ((used_s + reserved_s) / total_s) * 100.0), 1) if total_s > 0 else 0.0

    return {
        "used_hours": round(used_s / 3600.0, 2),
        "reserved_hours": round(reserved_s / 3600.0, 2),
        "total_hours": round(total_s / 3600.0, 2),
        "remaining_hours": round(rem_s / 3600.0, 2),
        "used_pct": pct,
        "over_quota": bool(total_s > 0 and used_s > total_s),
    }


def fetch_quota_summary(settings: Settings) -> dict[str, Any]:
    """Fetch weekly GPU and TPU quota statistics from Kaggle."""
    api = get_kaggle_api(settings)
    with protect_stdout():
        from kagglesdk.kernels.types.kernels_api_service import (
            ApiGetAcceleratorQuotaStatisticsRequest,
        )

        with api.build_kaggle_client() as kc:
            resp = kc.kernels.kernels_api_client.get_accelerator_quota_statistics(
                ApiGetAcceleratorQuotaStatisticsRequest()
            )

    refresh_raw = getattr(resp, "quota_refresh_time", None)
    if isinstance(refresh_raw, datetime):
        refresh_str = refresh_raw.isoformat()
    elif refresh_raw is not None:
        refresh_str = str(refresh_raw)
    else:
        try:
            refresh_str = str(resp.to_dict().get("quotaRefreshTime") or "")
        except Exception:
            refresh_str = ""

    return {
        "username": settings.kaggle_username,
        "gpu": parse_quota_bucket(getattr(resp, "gpu_quota", None)),
        "tpu": parse_quota_bucket(getattr(resp, "tpu_quota", None)),
        "quota_refresh_time": refresh_str,
    }
