"""Compose every named board diagram into an on-demand PDF handout."""

from __future__ import annotations

import json
import shutil
import tempfile
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.process.tool import run_tool_command
from spice.tasks import graph, identity, tw
from spice.tasks.graphs import derive, registry

DEFAULT_OUTPUT = Path("output/pdf/spice-board-handout")
RENDERER = Path(__file__).with_name("handout.js")


def _module_root() -> Path:
    candidates = [Path.cwd(), *Path.cwd().parents, *Path(__file__).resolve().parents]
    for root in candidates:
        modules = root / "node_modules"
        if (modules / "mermaid" / "dist" / "mermaid.min.js").is_file() and (
            modules / "playwright" / "package.json"
        ).is_file():
            return modules
    raise SpiceError(
        "board handout rendering requires the repository Node dependencies; "
        "run npm install before spice task handout"
    )


def _facts(rows: list[dict[str, Any]]) -> dict[str, int]:
    live = [row for row in rows if str(row.get("status") or "") != "deleted"]
    days = {
        stamp.date()
        for row in rows
        if (stamp := derive.epoch(row, "entry")) is not None
    }
    lanes = {derive.lane(row) for row in live} - {"(unplaced)"}
    return {
        "tasks": len(live),
        "completed": sum(str(row.get("status") or "") == "completed" for row in live),
        "archived": len(rows) - len(live),
        "lanes": len(lanes),
        "days": len(days),
        "diagrams": len(registry.CUTS),
    }


def build_payload(
    rows: list[dict[str, Any]], *, ceiling: str = ""
) -> dict[str, object]:
    snapshot = graph.live_rows(rows, ceiling=ceiling, include_deleted=True)
    stamp = identity.incepted_of_handle(ceiling) if ceiling else ""
    diagrams = []
    for selected in registry.CUTS:
        selected_rows = graph.rows_for(selected.name, rows, ceiling=ceiling)
        title, note, _body = registry.describe(selected.name, selected_rows)
        census = (
            "archived filings included" if selected.include_archived else "live rows"
        )
        diagrams.append(
            {
                "name": selected.name,
                "family": selected.family,
                "rank": selected.rank,
                "title": title,
                "note": note,
                "caption": f"{len(selected_rows)} {census} in this cut.",
                "includeArchived": selected.include_archived,
                "source": graph.render(selected.name, rows, ceiling=ceiling),
            }
        )
    command = "spice task handout"
    if ceiling:
        command += f" --ceiling {ceiling}"
    return {
        "facts": _facts(snapshot),
        "ceiling": stamp,
        "command": command,
        "aspect": {
            "minimum": registry.MIN_ASPECT_RATIO,
            "maximum": registry.MAX_ASPECT_RATIO,
        },
        "palette": list(registry.PALETTE),
        "diagrams": diagrams,
    }


def generate(output: Path = DEFAULT_OUTPUT, *, ceiling: str = "") -> str:
    node = shutil.which("node")
    if node is None:
        raise SpiceError("board handout rendering requires node")
    if not RENDERER.is_file():
        raise SpiceError(f"board handout renderer is missing: {RENDERER}")
    output = output.resolve()
    if output.exists() and not output.is_dir():
        raise SpiceError(f"board handout output must be a directory: {output}")
    output.mkdir(parents=True, exist_ok=True)
    payload = build_payload(tw.export(), ceiling=ceiling)
    modules = _module_root()
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", encoding="utf-8"
    ) as source:
        json.dump(payload, source, ensure_ascii=False)
        source.flush()
        result = run_tool_command(
            [node, str(RENDERER), source.name, str(output), str(modules)],
            policy="release",
            operation="render board handout",
            capture_output=True,
            text=True,
        )
    if result.returncode:
        detail = (result.stderr or result.stdout).strip()
        raise SpiceError(f"board handout rendering failed: {detail}")
    return result.stdout.strip()
