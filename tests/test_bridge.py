"""Unit tests for the Rust TUI JSON stdin/stdout Python SDK bridge (`bridge.py`)."""

from __future__ import annotations

import io
import json
import sys

from kaggle_mcp import bridge
from kaggle_mcp.config import Settings


def test_bridge_dispatches_quota_and_hub_without_stdout_pollution(monkeypatch) -> None:
    settings = Settings(kaggle_username="alice", kaggle_secret="KGAT_dummy")
    monkeypatch.setattr(bridge, "load_settings", lambda: settings)

    def _noisy_quota(_s):
        print("NOISY THIRD PARTY STDOUT SHOULD NOT LEAK")
        return {"username": "alice", "gpu": {"remaining_hours": 28.5}}

    monkeypatch.setattr(bridge, "fetch_quota_summary", _noisy_quota)
    monkeypatch.setattr(
        bridge,
        "search_kaggle_hub",
        lambda _s, category, query: [{"category": category, "ref": "alice/ds", "title": query}],
    )
    monkeypatch.setattr(
        bridge,
        "submit_competition_file",
        lambda _s, competition, local_csv_path, message: {
            "ok": True,
            "competition": competition,
            "file": local_csv_path,
            "message": message,
        },
    )

    out_buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", out_buf)
    monkeypatch.setattr(sys, "stdin", io.StringIO("{}"))
    assert bridge.main(["quota"]) == 0
    parsed = json.loads(out_buf.getvalue().strip())
    assert parsed["ok"] is True
    assert parsed["data"]["username"] == "alice"

    out_buf2 = io.StringIO()
    monkeypatch.setattr(sys, "stdout", out_buf2)
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps({"category": "datasets", "query": "llm"})))
    assert bridge.main(["hub_search"]) == 0
    parsed2 = json.loads(out_buf2.getvalue().strip())
    assert parsed2["ok"] is True
    assert parsed2["data"][0]["ref"] == "alice/ds"


def test_bridge_returns_json_error_on_unknown_action(monkeypatch) -> None:
    monkeypatch.setattr(bridge, "load_settings", lambda: Settings("alice", "KGAT_dummy"))
    out_buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", out_buf)
    monkeypatch.setattr(sys, "stdin", io.StringIO("{}"))
    assert bridge.main(["no_such_action"]) == 1
    parsed = json.loads(out_buf.getvalue().strip())
    assert parsed["ok"] is False
    assert "Unknown bridge action" in parsed["error"]


def test_bridge_encodes_unicode_symbols_on_cp1252_stdout(monkeypatch) -> None:
    monkeypatch.setattr(bridge, "load_settings", lambda: Settings("alice", "KGAT_dummy"))
    monkeypatch.setattr(
        bridge,
        "search_kaggle_hub",
        lambda _s, category, query: [
            {"category": category, "ref": "alice/ds", "title": "Eval", "info": "▲ 42 · ── •", "metric": "1.0 MB"}
        ],
    )
    raw = io.BytesIO()
    cp1252_out = io.TextIOWrapper(raw, encoding="cp1252", errors="strict")
    monkeypatch.setattr(sys, "stdout", cp1252_out)
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps({"category": "datasets", "query": ""})))
    assert bridge.main(["hub_search"]) == 0
    cp1252_out.flush()
    parsed = json.loads(raw.getvalue().decode("cp1252").strip())
    assert parsed["ok"] is True
    assert parsed["data"][0]["info"] == "▲ 42 · ── •"
