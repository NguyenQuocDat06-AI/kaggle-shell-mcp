"""Tests for Kaggle Hub helpers, local activity spans, Rust JSON bridge, and stdio MCP protocol."""

from __future__ import annotations

import io
import json
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from kaggle_mcp import bridge, kaggle_hub
from kaggle_mcp.activity import (
    mcp_call_span,
    mcp_flow_mtime_ns,
    read_local_activities,
    read_mcp_flow_events,
    record_local_activity,
)
from kaggle_mcp.config import Settings

EXPECTED_MCP_TOOLS = {
    "kaggle_quota",
    "kaggle_up",
    "kaggle_status",
    "kaggle_down",
    "term_exec",
    "job_spawn",
    "job_list",
    "job_logs",
    "job_kill",
    "fs_sync",
    "kaggle_wheelhouse",
    "kaggle_batch_run",
    "kaggle_batch_logs",
}


def test_kaggle_hub_search_inspect_logs_delete_and_submit(
    tmp_path: Path,
    sample_settings: Settings,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    assert (
        kaggle_hub.clean_competition_slug("https://www.kaggle.com/competitions/arc-prize-2026/")
        == "arc-prize-2026"
    )
    assert kaggle_hub.clean_competition_slug("https://www.kaggle.com/c/titanic") == "titanic"

    calls: list[tuple[str, tuple]] = []

    class FakeApi:
        def dataset_list(self, search=None, mine=False, page=1):
            return [
                SimpleNamespace(
                    ref="alice/llm-eval-pack",
                    title="LLM Eval Pack",
                    vote_count=42,
                    is_private=mine,
                    total_bytes=15 * 1024 * 1024,
                )
            ]

        def dataset_list_files(self, ref, page_size=40):
            return SimpleNamespace(
                dataset_files=[
                    SimpleNamespace(name="train.parquet", total_bytes=10 * 1024 * 1024),
                    SimpleNamespace(name="valid.parquet", total_bytes=5 * 1024 * 1024),
                ]
            )

        def competitions_list(self, search=None, page_size=25):
            return SimpleNamespace(
                competitions=[
                    SimpleNamespace(
                        ref="https://www.kaggle.com/competitions/titanic",
                        title="Titanic - Machine Learning from Disaster",
                        reward="Knowledge",
                        deadline="2030-01-01 00:00:00",
                        user_has_entered=True,
                    )
                ]
            )

        def competition_submissions(self, slug, page_size=8):
            return [
                SimpleNamespace(
                    date="2026-11-25 14:20:00",
                    file_name="sub_xgb.csv",
                    public_score="0.81240",
                    private_score="--",
                    status="COMPLETE",
                )
            ]

        def competition_leaderboard_view(self, slug, page_size=15):
            return [
                SimpleNamespace(
                    team_name="GrandmasterTeam",
                    score="0.84910",
                    submission_date="2026-11-20",
                )
            ]

        def kernels_logs(self, ref):
            calls.append(("logs", (ref,)))
            return json.dumps([{"data": "Epoch 1\n"}, {"data": "loss=0.12\n"}])

        def kernels_delete(self, ref, no_confirm=False):
            calls.append(("delete", (ref, no_confirm)))

        def competition_submit(self, file_name, message, competition, quiet=False):
            calls.append(("submit", (file_name, message, competition, quiet)))
            return "Successfully submitted"

    monkeypatch.setattr(kaggle_hub, "get_kaggle_api", lambda _s: FakeApi())

    ds_rows = kaggle_hub.search_kaggle_hub(sample_settings, category="datasets", query="llm")
    assert ds_rows[0]["ref"] == "alice/llm-eval-pack" and "15.0 MB" in ds_rows[0]["metric"]
    ds_detail = kaggle_hub.inspect_kaggle_hub_item(sample_settings, "datasets", "alice/llm-eval-pack")
    assert ds_detail["stage_kind"] == "dataset" and "train.parquet" in ds_detail["text"]

    comp_rows = kaggle_hub.search_kaggle_hub(sample_settings, category="competitions", query="titanic")
    assert comp_rows[0]["ref"] == "titanic" and "Joined" in comp_rows[0]["info"]
    comp_detail = kaggle_hub.inspect_kaggle_hub_item(sample_settings, "competitions", "titanic")
    assert "0.81240" in comp_detail["text"] and "GrandmasterTeam" in comp_detail["text"]

    logs = kaggle_hub.fetch_kernel_logs(sample_settings, "alice/my-kernel")
    assert "Epoch 1" in logs and "loss=0.12" in logs
    assert kaggle_hub.delete_kaggle_kernel(sample_settings, "alice/my-kernel") == {
        "ok": True,
        "ref": "alice/my-kernel",
    }

    sub_csv = tmp_path / "submission.csv"
    sub_csv.write_text("id,pred\n1,0.9\n", encoding="utf-8")
    sub_res = kaggle_hub.submit_competition_file(
        sample_settings,
        "https://www.kaggle.com/competitions/titanic/",
        str(sub_csv),
        "xgb v1",
    )
    assert sub_res["ok"] is True and sub_res["competition"] == "titanic"
    assert ("submit", (str(sub_csv.resolve()), "xgb v1", "titanic", True)) in calls

    with pytest.raises(FileNotFoundError):
        kaggle_hub.submit_competition_file(sample_settings, "titanic", str(tmp_path / "missing.csv"), "x")


def test_local_activity_log_and_mcp_call_spans(isolated_state_dir: Path) -> None:
    t0 = time.time() - 1.0
    record_local_activity("1", "mcp", "boot_start", "Booting slot 1")
    record_local_activity("2", "mcp", "boot_start", "Booting slot 2")

    s1_events = read_local_activities(slot="1", since_ts=t0)
    assert len(s1_events) == 1 and s1_events[0]["summary"] == "Booting slot 1"

    with mcp_call_span("term_exec", slot="1", summary="nvidia-smi"):
        pass
    with pytest.raises(RuntimeError, match="boom"):
        with mcp_call_span("job_spawn", slot="2", summary="train.py"):
            raise RuntimeError("boom")

    assert mcp_flow_mtime_ns() > 0
    events = read_mcp_flow_events(since_ts=t0)
    assert [e["phase"] for e in events] == ["start", "done", "start", "error"]
    assert events[1]["tool"] == "term_exec"
    assert events[3]["tool"] == "job_spawn" and "boom" in events[3]["error"]


def test_bridge_stdout_protection_errors_and_cp1252_encoding(
    sample_settings: Settings, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(bridge, "load_settings", lambda: sample_settings)

    def _noisy_quota(_s: Settings) -> dict:
        print("NOISY THIRD PARTY STDOUT SHOULD NOT LEAK")
        return {"username": "alice", "gpu": {"remaining_hours": 28.5}}

    monkeypatch.setattr(bridge, "fetch_quota_summary", _noisy_quota)
    monkeypatch.setattr(
        bridge,
        "search_kaggle_hub",
        lambda _s, category, query: [
            {"category": category, "ref": "alice/ds", "title": "Eval", "info": "▲ 42 · ── •", "metric": "1.0 MB"}
        ],
    )

    out_buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", out_buf)
    monkeypatch.setattr(sys, "stdin", io.StringIO("{}"))
    assert bridge.main(["quota"]) == 0
    parsed = json.loads(out_buf.getvalue().strip())
    assert parsed["ok"] is True and parsed["data"]["username"] == "alice"

    err_buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", err_buf)
    monkeypatch.setattr(sys, "stdin", io.StringIO("{}"))
    assert bridge.main(["no_such_action"]) == 1
    parsed_err = json.loads(err_buf.getvalue().strip())
    assert parsed_err["ok"] is False and "Unknown bridge action" in parsed_err["error"]

    raw = io.BytesIO()
    cp1252_out = io.TextIOWrapper(raw, encoding="cp1252", errors="strict")
    monkeypatch.setattr(sys, "stdout", cp1252_out)
    monkeypatch.setattr(sys, "stdin", io.StringIO(json.dumps({"category": "datasets", "query": ""})))
    assert bridge.main(["hub_search"]) == 0
    cp1252_out.flush()
    parsed_cp = json.loads(raw.getvalue().decode("cp1252").strip())
    assert parsed_cp["ok"] is True and parsed_cp["data"][0]["info"] == "▲ 42 · ── •"


@pytest.mark.asyncio
async def test_mcp_stdio_registers_all_thirteen_tools() -> None:
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "kaggle_mcp.server"],
    )
    async with stdio_client(params) as (read_stream, write_stream):
        async with ClientSession(read_stream, write_stream) as session:
            await session.initialize()
            result = await session.list_tools()
            names = {t.name for t in result.tools}
            assert names == EXPECTED_MCP_TOOLS
            for tool in result.tools:
                assert tool.description and len(tool.description) > 15
