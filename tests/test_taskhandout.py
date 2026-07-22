"""On-demand board handout contracts."""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.tasks.graphs import handout, registry

PROJECT_ROOT = Path(__file__).resolve().parent.parent
MERMAID_BUNDLE = PROJECT_ROOT / "node_modules" / "mermaid" / "dist" / "mermaid.min.js"


def _rows() -> list[dict]:
    return [
        {
            "uuid": "live",
            "incepted": "1kG0aaaa",
            "project": "serve.graphs",
            "description": "Render the live board",
            "status": "completed",
            "origin": "ack:live-key",
            "origin_worktree": "/repo/spice-a",
            "origin_thread": "thread-a",
            "entry": "20260721T035259Z",
            "end": "20260721T041008Z",
            "phase_0": "todo",
            "phase_1": "review",
            "phase_i": 1,
        },
        {
            "uuid": "archived",
            "incepted": "1kG0bbbb",
            "project": ".oops.workflow",
            "description": "Record a withdrawn filing",
            "status": "deleted",
            "origin": "ack:archive-key",
            "origin_worktree": "/repo/spice-b",
            "origin_thread": "thread-b",
            "entry": "20260721T045259Z",
            "phase_0": "todo",
            "phase_i": 0,
        },
    ]


def test_handout_payload_uses_one_census_with_per_cut_archive_policy() -> None:
    payload = handout.build_payload(_rows())
    diagrams = {entry["name"]: entry for entry in payload["diagrams"]}

    assert tuple(diagrams) == registry.NAMES
    assert payload["facts"] == {
        "tasks": 1,
        "completed": 1,
        "archived": 1,
        "lanes": 1,
        "days": 1,
        "diagrams": 37,
    }
    assert diagrams["02-agent-worktrees-xy"]["caption"] == (
        "2 archived filings included in this cut."
    )
    assert diagrams["01-project-stems-xy"]["caption"] == ("1 live rows in this cut.")


def test_handout_cli_passes_output_and_snapshot_ceiling(monkeypatch, capsys) -> None:
    calls: list[tuple[Path, str]] = []

    def generate(output: Path, *, ceiling: str = "") -> str:
        calls.append((output, ceiling))
        return "handout=fixed.pdf pages=39 diagrams=37 pdf_kb=1"

    monkeypatch.setattr(handout, "generate", generate)
    args = build_parser().parse_args(
        [
            "task",
            "handout",
            "--output",
            "build/board",
            "--ceiling",
            "GRAPHS-1kG0aaaa",
        ]
    )

    assert args.func(args) == 0
    assert calls == [(Path("build/board"), "GRAPHS-1kG0aaaa")]
    assert (
        capsys.readouterr().out == "handout=fixed.pdf pages=39 diagrams=37 pdf_kb=1\n"
    )


def test_handout_command_renders_all_assets_and_pdf(
    tmp_path: Path, monkeypatch
) -> None:
    if shutil.which("node") is None:
        pytest.skip("node is required")
    if not MERMAID_BUNDLE.is_file():
        pytest.skip(f"missing Node dependencies: run npm install in {PROJECT_ROOT}")
    exports: list[str] = []

    def export() -> list[dict]:
        exports.append("snapshot")
        return _rows()

    monkeypatch.setattr(handout.tw, "export", export)

    result = handout.generate(tmp_path)
    manifest = json.loads((tmp_path / "manifest.json").read_text(encoding="utf-8"))

    assert result.startswith(f"handout={tmp_path / 'handout.pdf'}")
    assert exports == ["snapshot"]
    assert (tmp_path / "handout.pdf").read_bytes().startswith(b"%PDF")
    assert len(list((tmp_path / "diagrams").glob("*.svg"))) == len(registry.CUTS)
    assert len(list((tmp_path / "diagrams").glob("*.png"))) == len(registry.CUTS)
    assert [entry["name"] for entry in manifest["diagrams"]] == list(registry.NAMES)
    assert manifest["composition"]["diagram"] == "32-board-at-a-glance"
    assert manifest["composition"]["verifiedHtmlLabels"] >= 6
    assert tuple(lines[0] for lines in manifest["composition"]["indexLabels"]) == (
        "spice task board",
        "origin forest",
        "dependency DAG",
        "review network",
        "phase ladder",
        "taskdoc tree",
    )
