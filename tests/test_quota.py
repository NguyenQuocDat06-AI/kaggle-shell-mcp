"""Tests for kagglesdk timedelta quota parsing (including 30h days=1 and over-quota clamping)."""

from __future__ import annotations

from datetime import timedelta
from types import SimpleNamespace

from kaggle_mcp.quota import _td_seconds, parse_quota_bucket


def test_timedelta_30h_gpu_quota_not_truncated_at_24h() -> None:
    # 30 hours = 1 day + 6 hours (21600s); kagglesdk.to_dict() drops days=1, so we read timedelta directly
    bucket = SimpleNamespace(
        time_used=timedelta(hours=7, minutes=30),
        time_reserved=timedelta(0),
        total_time_allowed=timedelta(days=1, seconds=21600),
    )
    parsed = parse_quota_bucket(bucket)
    assert parsed["total_hours"] == 30.0
    assert parsed["used_hours"] == 7.5
    assert parsed["remaining_hours"] == 22.5
    assert parsed["used_pct"] == 25.0
    assert parsed["over_quota"] is False


def test_over_quota_clamps_remaining_to_zero_and_handles_malformed_strings() -> None:
    bucket = SimpleNamespace(
        time_used=timedelta(hours=35),
        time_reserved=timedelta(0),
        total_time_allowed=timedelta(hours=30),
    )
    parsed = parse_quota_bucket(bucket)
    assert parsed["remaining_hours"] == 0.0
    assert parsed["used_pct"] == 100.0
    assert parsed["over_quota"] is True

    # Malformed double-dot duration string fallback
    assert _td_seconds("81802.260441.0s") == 81802.260441
