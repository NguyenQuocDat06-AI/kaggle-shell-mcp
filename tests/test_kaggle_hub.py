"""Unit tests for Kaggle Hub convenience helpers (`kaggle_hub.py`)."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from kaggle_mcp import kaggle_hub
from kaggle_mcp.config import Settings


def test_clean_competition_slug_strips_kaggle_urls() -> None:
    assert (
        kaggle_hub.clean_competition_slug("https://www.kaggle.com/competitions/arc-prize-2026/")
        == "arc-prize-2026"
    )
    assert kaggle_hub.clean_competition_slug("https://www.kaggle.com/c/titanic") == "titanic"
    assert kaggle_hub.clean_competition_slug("house-prices") == "house-prices"


def test_search_and_inspect_kaggle_hub_datasets_and_competitions(monkeypatch) -> None:
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

    monkeypatch.setattr(kaggle_hub, "get_kaggle_api", lambda _s: FakeApi())
    settings = Settings(kaggle_username="alice", kaggle_secret="KGAT_dummy")

    ds_rows = kaggle_hub.search_kaggle_hub(settings, category="datasets", query="llm")
    assert len(ds_rows) == 1
    assert ds_rows[0]["ref"] == "alice/llm-eval-pack"
    assert "15.0 MB" in ds_rows[0]["metric"]

    ds_detail = kaggle_hub.inspect_kaggle_hub_item(settings, "datasets", "alice/llm-eval-pack")
    assert ds_detail["stage_kind"] == "dataset"
    assert "train.parquet" in ds_detail["text"]

    comp_rows = kaggle_hub.search_kaggle_hub(settings, category="competitions", query="titanic")
    assert comp_rows[0]["ref"] == "titanic"
    assert "Joined" in comp_rows[0]["info"]

    comp_detail = kaggle_hub.inspect_kaggle_hub_item(settings, "competitions", "titanic")
    assert comp_detail["stage_kind"] == "competition"
    assert "0.81240" in comp_detail["text"]
    assert "GrandmasterTeam" in comp_detail["text"]


def test_kernel_logs_delete_and_competition_submit(tmp_path: Path, monkeypatch) -> None:
    calls: list[tuple[str, tuple]] = []

    class FakeApi:
        def kernels_logs(self, ref):
            calls.append(("logs", (ref,)))
            return json.dumps([{"data": "Epoch 1\n"}, {"data": "loss=0.12\n"}])

        def kernels_delete(self, ref, no_confirm=False):
            calls.append(("delete", (ref, no_confirm)))

        def competition_submit(self, file_name, message, competition, quiet=False):
            calls.append(("submit", (file_name, message, competition, quiet)))
            return "Successfully submitted"

    monkeypatch.setattr(kaggle_hub, "get_kaggle_api", lambda _s: FakeApi())
    settings = Settings(kaggle_username="alice", kaggle_secret="KGAT_dummy")

    logs = kaggle_hub.fetch_kernel_logs(settings, "alice/my-kernel")
    assert "Epoch 1" in logs and "loss=0.12" in logs

    del_res = kaggle_hub.delete_kaggle_kernel(settings, "alice/my-kernel")
    assert del_res == {"ok": True, "ref": "alice/my-kernel"}
    assert ("delete", ("alice/my-kernel", True)) in calls

    sub_csv = tmp_path / "submission.csv"
    sub_csv.write_text("id,pred\n1,0.9\n", encoding="utf-8")
    sub_res = kaggle_hub.submit_competition_file(
        settings,
        "https://www.kaggle.com/competitions/titanic/",
        str(sub_csv),
        "xgb v1",
    )
    assert sub_res["ok"] is True
    assert sub_res["competition"] == "titanic"
    assert ("submit", (str(sub_csv.resolve()), "xgb v1", "titanic", True)) in calls

    with pytest.raises(FileNotFoundError):
        kaggle_hub.submit_competition_file(settings, "titanic", str(tmp_path / "missing.csv"), "x")
