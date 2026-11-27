"""Read-only Kaggle Platform Hub helpers for `kgsh ui` (Datasets, Competitions, Models, Kernels).

Keeps all Kaggle platform browsing, leaderboard/submission checks, and optional downloads
cleanly isolated in the TUI layer without altering any MCP tools or remote worker behavior.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .config import Settings
from .launcher import get_kaggle_api, get_kernel_status, protect_stdout


def _fmt_bytes(num_bytes: int | float | None) -> str:
    val = float(num_bytes or 0)
    if val <= 0:
        return "--"
    if val < 1024:
        return f"{int(val)} B"
    if val < 1024**2:
        return f"{val / 1024:.1f} KB"
    if val < 1024**3:
        return f"{val / (1024**2):.1f} MB"
    return f"{val / (1024**3):.2f} GB"


def clean_competition_slug(raw_ref: str) -> str:
    """Normalize either a full Kaggle competition URL or a slug into a clean slug."""
    txt = str(raw_ref or "").strip().rstrip("/")
    for prefix in (
        "https://www.kaggle.com/competitions/",
        "https://www.kaggle.com/c/",
        "http://www.kaggle.com/competitions/",
        "http://www.kaggle.com/c/",
    ):
        if txt.startswith(prefix):
            txt = txt[len(prefix) :]
    return txt.split("/")[0].strip()


def search_kaggle_hub(
    settings: Settings,
    category: str = "datasets",
    query: str = "",
) -> list[dict[str, Any]]:
    """Search Kaggle Datasets, My Datasets, Competitions, Models, or My Kernels."""
    api = get_kaggle_api(settings)
    q = query.strip() or None
    rows: list[dict[str, Any]] = []

    if category in {"datasets", "my_datasets"}:
        mine = category == "my_datasets"
        with protect_stdout():
            items = api.dataset_list(search=q, mine=mine, page=1) or []
        for ds in items[:25]:
            if ds is None:
                continue
            ref = str(getattr(ds, "ref", "") or "")
            title = str(getattr(ds, "title", "") or ref)
            votes = int(getattr(ds, "vote_count", 0) or 0)
            priv = "🔒 Private" if getattr(ds, "is_private", False) else f"▲ {votes}"
            rows.append(
                {
                    "category": category,
                    "ref": ref,
                    "title": title,
                    "info": priv,
                    "metric": _fmt_bytes(getattr(ds, "total_bytes", 0)),
                }
            )
        return rows

    if category == "competitions":
        with protect_stdout():
            resp = api.competitions_list(search=q, page_size=25)
        comp_list = (
            getattr(resp, "competitions", resp) if resp is not None else []
        ) or []
        for comp in comp_list[:25]:
            if comp is None:
                continue
            slug = clean_competition_slug(getattr(comp, "ref", ""))
            title = str(getattr(comp, "title", "") or slug)
            reward = str(getattr(comp, "reward", "") or getattr(comp, "category", "") or "--")
            dl_raw = getattr(comp, "deadline", None)
            dl_str = str(dl_raw)[:10] if dl_raw else "--"
            entered = "✓ Joined · " if getattr(comp, "user_has_entered", False) else ""
            rows.append(
                {
                    "category": "competitions",
                    "ref": slug,
                    "title": title,
                    "info": f"{entered}{reward}",
                    "metric": f"Due {dl_str}",
                }
            )
        return rows

    if category == "models":
        with protect_stdout():
            models = api.model_list(search=q, page_size=25) or []
        for mdl in models[:25]:
            if mdl is None:
                continue
            ref = str(getattr(mdl, "ref", "") or "")
            title = str(getattr(mdl, "title", "") or ref)
            author = str(getattr(mdl, "author", "") or "")
            votes = int(getattr(mdl, "vote_count", 0) or 0)
            rows.append(
                {
                    "category": "models",
                    "ref": ref,
                    "title": title,
                    "info": author or "Kaggle Model",
                    "metric": f"▲ {votes}",
                }
            )
        return rows

    if category == "my_kernels":
        with protect_stdout():
            kernels = api.kernels_list(mine=True, search=q, page_size=20) or []
        for idx, krn in enumerate(kernels[:20]):
            if krn is None:
                continue
            ref = str(getattr(krn, "ref", "") or "")
            title = str(getattr(krn, "title", "") or ref)
            shape = str(getattr(krn, "machine_shape", "") or "")
            if not shape:
                if getattr(krn, "enable_tpu", False):
                    shape = "TPU"
                elif getattr(krn, "enable_gpu", False):
                    shape = "GPU"
                else:
                    shape = "CPU"
            # Fetch live status for top 6 recent kernels to keep listing snappy
            if idx < 6 and ref:
                st = get_kernel_status(api, ref).get("status", "")
            else:
                st = f"v{getattr(krn, 'current_version_number', '?')}"
            last_run = str(getattr(krn, "last_run_time", "") or "")[:16]
            rows.append(
                {
                    "category": "my_kernels",
                    "ref": ref,
                    "title": title,
                    "info": f"{shape} · {st}",
                    "metric": last_run or "--",
                }
            )
        return rows

    return rows


def inspect_kaggle_hub_item(
    settings: Settings,
    category: str,
    ref: str,
) -> dict[str, Any]:
    """Fetch detailed info (files, leaderboard + my submissions, variations, or kernel status)."""
    api = get_kaggle_api(settings)
    lines: list[str] = []
    stage_kind: str | None = None
    stage_value: str = ref
    can_download = False

    if category in {"datasets", "my_datasets"}:
        stage_kind = "dataset"
        can_download = True
        lines.append(f"Dataset Ref : {ref}")
        lines.append(f"Kaggle URL  : https://www.kaggle.com/datasets/{ref}")
        lines.append("")
        lines.append("── Files inside Dataset ──────────────────────────────────────")
        try:
            with protect_stdout():
                f_resp = api.dataset_list_files(ref, page_size=40)
            files = getattr(f_resp, "dataset_files", None) or getattr(f_resp, "files", None) or []
            if not files:
                lines.append("  (No files returned)")
            for f in files[:40]:
                fname = getattr(f, "name", "") or getattr(f, "ref", "")
                fsize = _fmt_bytes(getattr(f, "total_bytes", 0))
                lines.append(f"  • {fname:<44}  {fsize:>10}")
        except Exception as exc:
            lines.append(f"  [Could not list files: {exc}]")

    elif category == "competitions":
        slug = clean_competition_slug(ref)
        stage_kind = "competition"
        stage_value = slug
        can_download = True
        lines.append(f"Competition : {slug}")
        lines.append(f"Kaggle URL  : https://www.kaggle.com/competitions/{slug}")
        lines.append("")
        lines.append("── My Recent Submissions ─────────────────────────────────────")
        try:
            with protect_stdout():
                subs = api.competition_submissions(slug, page_size=8) or []
            if not subs:
                lines.append("  (No submissions yet for this account)")
            for s in subs[:8]:
                if s is None:
                    continue
                dt = str(getattr(s, "date", ""))[:16]
                fn = str(getattr(s, "file_name", ""))[:24]
                pub = str(getattr(s, "public_score", "--") or "--")
                priv = str(getattr(s, "private_score", "--") or "--")
                st = str(getattr(s, "status", "")).split(".")[-1]
                lines.append(f"  {dt:<16}  {fn:<24}  Pub: {pub:<10}  Priv: {priv:<10}  [{st}]")
        except Exception as exc:
            lines.append(f"  [Submissions unavailable — ensure competition rules accepted: {exc}]")

        lines.append("")
        lines.append("── Public Leaderboard (Top 15) ───────────────────────────────")
        try:
            with protect_stdout():
                lb = api.competition_leaderboard_view(slug, page_size=15) or []
            if not lb:
                lines.append("  (Leaderboard empty or unavailable)")
            for rank, row in enumerate(lb[:15], start=1):
                if row is None:
                    continue
                team = str(getattr(row, "team_name", ""))[:32]
                score = str(getattr(row, "score", ""))
                dt = str(getattr(row, "submission_date", ""))[:10]
                lines.append(f"  #{rank:<3}  {team:<34}  Score: {score:<12}  ({dt})")
        except Exception as exc:
            lines.append(f"  [Leaderboard unavailable: {exc}]")

    elif category == "models":
        stage_kind = "model"
        lines.append(f"Model Ref   : {ref}")
        lines.append(f"Kaggle URL  : https://www.kaggle.com/models/{ref}")
        lines.append("")
        lines.append("── Mountable Model Variations (use in Boot Wizard F5) ────────")
        try:
            with protect_stdout():
                mdl = api.model_get(ref)
            insts = getattr(mdl, "instances", None) or []
            if not insts:
                lines.append(
                    f"  Tip: format as '{ref}/<framework>/<variation>' (e.g. {ref}/transformers/default)"
                )
            for idx, inst in enumerate(insts[:25]):
                fw = getattr(inst, "framework", "") or ""
                slug = getattr(inst, "slug", "") or ""
                fw_clean = str(fw).split(".")[-1].lower()
                mount_id = f"{ref}/{fw_clean}/{slug}" if (fw_clean and slug) else str(getattr(inst, "url", ref))
                if idx == 0 and "/" in mount_id and not mount_id.startswith("http"):
                    stage_value = mount_id
                lines.append(f"  • {mount_id}")
        except Exception as exc:
            lines.append(f"  [Variation lookup note: {exc}]")
            lines.append(f"  Format in F5 Boot Wizard: {ref}/<framework>/<variation>")

    elif category == "my_kernels":
        can_download = True
        st_info = get_kernel_status(api, ref)
        lines.append(f"Kernel Ref  : {ref}")
        lines.append(f"Kaggle URL  : https://www.kaggle.com/code/{ref}")
        lines.append(f"Live Status : {st_info.get('status', 'UNKNOWN')}")
        if st_info.get("failure_message"):
            lines.append(f"Failure Msg : {st_info['failure_message']}")
        lines.append("")
        lines.append("Tip: Click 'Download to ./kaggle_downloads' below to pull saved Kernel outputs.")

    return {
        "category": category,
        "ref": ref,
        "text": "\n".join(lines),
        "stage_kind": stage_kind,
        "stage_value": stage_value,
        "can_download": can_download,
    }


def download_kaggle_hub_item(
    settings: Settings,
    category: str,
    ref: str,
    base_dir: str = "./kaggle_downloads",
) -> str:
    """Download a Kaggle Dataset, Competition archive, or Kernel output into `./kaggle_downloads`."""
    api = get_kaggle_api(settings)
    safe_sub = ref.replace("/", "__").strip("_") or "item"
    dest = Path(base_dir).resolve() / safe_sub
    dest.mkdir(parents=True, exist_ok=True)

    if category in {"datasets", "my_datasets"}:
        with protect_stdout():
            api.dataset_download_files(ref, path=str(dest), force=True, quiet=True, unzip=True)
        return str(dest)

    if category == "competitions":
        slug = clean_competition_slug(ref)
        with protect_stdout():
            api.competition_download_files(slug, path=str(dest), force=True, quiet=True)
        return str(dest)

    if category == "my_kernels":
        with protect_stdout():
            api.kernels_output(ref, path=str(dest), force=True, quiet=True)
        return str(dest)

    raise ValueError(f"Download not supported for category '{category}'")


def fetch_kernel_logs(settings: Settings, kernel_ref: str, tail_lines: int = 120) -> str:
    """Fetch last execution logs of a Kaggle Kernel (`username/slug`)."""
    ref = str(kernel_ref or "").strip()
    if not ref:
        raise ValueError("Kernel ref cannot be empty.")
    api = get_kaggle_api(settings)
    with protect_stdout():
        raw = api.kernels_logs(ref)

    text = ""
    if isinstance(raw, dict):
        text = str(raw.get("log") or raw.get("output") or json.dumps(raw, indent=2))
    elif isinstance(raw, list):
        parts = []
        for entry in raw:
            if isinstance(entry, dict):
                t = entry.get("time", "")
                d = entry.get("data", "")
                parts.append(f"[{t}] {d}" if t else str(d))
            else:
                parts.append(str(entry))
        text = "".join(parts)
    else:
        raw_str = str(raw or "")
        stripped = raw_str.strip()
        if stripped.startswith(("[", "{")):
            try:
                parsed = json.loads(stripped)
                if isinstance(parsed, list):
                    lines_out = []
                    for entry in parsed:
                        if isinstance(entry, dict):
                            lines_out.append(str(entry.get("data", entry)))
                        else:
                            lines_out.append(str(entry))
                    text = "".join(lines_out)
                elif isinstance(parsed, dict) and "log" in parsed:
                    text = str(parsed["log"])
                else:
                    text = raw_str
            except Exception:
                text = raw_str
        else:
            text = raw_str

    lines = [ln.rstrip("\r") for ln in text.splitlines() if ln.strip() != ""]
    if not lines:
        return "(No execution logs available for this kernel.)"
    if tail_lines > 0 and len(lines) > tail_lines:
        lines = lines[-tail_lines:]
    return "\n".join(lines)


def delete_kaggle_kernel(settings: Settings, kernel_ref: str) -> dict[str, Any]:
    """Delete/cancel a Kaggle Kernel owned by the current account (`no_confirm=True`)."""
    ref = str(kernel_ref or "").strip()
    if not ref:
        raise ValueError("Kernel ref cannot be empty.")
    api = get_kaggle_api(settings)
    with protect_stdout():
        api.kernels_delete(ref, no_confirm=True)
    return {"ok": True, "ref": ref}


def submit_competition_file(
    settings: Settings,
    competition: str,
    local_csv_path: str,
    message: str = "Submitted via kgsh",
) -> dict[str, Any]:
    """Submit a local CSV prediction file to a Kaggle Competition."""
    slug = clean_competition_slug(competition)
    if not slug:
        raise ValueError("Competition slug cannot be empty.")
    csv_path = Path(local_csv_path).expanduser().resolve()
    if not csv_path.is_file():
        raise FileNotFoundError(f"Submission file not found: {csv_path}")
    msg = (message or "Submitted via kgsh").strip()
    api = get_kaggle_api(settings)
    with protect_stdout():
        res = api.competition_submit(str(csv_path), msg, slug, quiet=True)
    return {
        "ok": True,
        "competition": slug,
        "file": str(csv_path),
        "message": msg,
        "result": str(res or "Submitted"),
    }
