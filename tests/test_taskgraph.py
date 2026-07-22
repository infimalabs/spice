"""Task graph surface contracts.

The surface's whole promise is that its output renders unmodified, so the
central test here renders every view through real mermaid in a real browser
rather than asserting on the emitted text. Text assertions cannot see a label
mermaid reads as markup or a node id its grammar rejects; only rendering can.

The rows are a fixture rather than the live board, and deliberately hostile:
a description carrying a double quote and a pound sign, a project whose key
collides with another's, a handle whose stamp leads with a digit, and a label
past the truncation limit. Live rows would prove less — they only exercise the
characters the board happens to contain today.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.tasks import graph

PROJECT_ROOT = Path(__file__).resolve().parent.parent
RENDERER = PROJECT_ROOT / "tests" / "browser" / "task_graph_mermaid_render.js"
MERMAID_BUNDLE = PROJECT_ROOT / "node_modules" / "mermaid" / "dist" / "mermaid.min.js"


def _rows() -> list[dict]:
    """A small board carrying all four graphs at once.

    Every field the surface reads is present on at least one row, and every
    escaping hazard is present on at least one label.
    """
    return [
        {
            "uuid": "u-seed",
            "incepted": "1kG0aaaa",
            "project": "task.lineage",
            "description": 'Draw the board\'s "origin" forest — pass #1',
            "status": "completed",
            "origin": "ack:1kG1szwQ",
            "origin_worktree": "worktrees/spice-g",
            "origin_thread": "thread-g",
            "phase_0": "todo",
            "phase_1": "review",
        },
        {
            "uuid": "u-child",
            "incepted": "9kG0bbbb",
            "project": "task.lineage",
            "description": "A description long past the truncation limit, "
            "so the label has to be cut and ellipsed before mermaid sees it",
            "status": "completed",
            "origin": "task:LINEAGE-1kG0aaaa",
            "origin_worktree": "worktrees/spice-a",
            "origin_thread": "thread-a",
            "review_author": "thread-g",
            "depends": ["u-seed"],
            "phase_0": "plan",
            "phase_1": "todo",
            "phase_2": "review",
        },
        {
            "uuid": "u-blocked",
            "incepted": "1kG0cccc",
            "project": "serve.mosaic",
            "description": "Blocked on both of the above",
            "status": "pending",
            "origin": "task:LINEAGE-9kG0bbbb",
            "origin_worktree": "worktrees/spice-z",
            "origin_thread": "thread-z",
            "review_author": "thread-a",
            "depends": ["u-seed", "u-child"],
            "phase_0": "todo",
        },
        {
            "uuid": "u-deleted",
            "incepted": "1kG0dddd",
            "project": "smoke",
            "description": "Smoke residue that must not reach any view",
            "status": "deleted",
            "origin": "task:LINEAGE-1kG0aaaa",
            "phase_0": "todo",
        },
    ]


def _diagrams() -> dict[str, str]:
    """Every advertised view, rendered from the fixture board."""
    rows = _rows()
    return {view: graph.render(view, rows) for view in graph.VIEWS}


def test_live_rows_drop_deleted_smoke_residue() -> None:
    rows = graph.live_rows(_rows())

    assert [row["uuid"] for row in rows] == ["u-seed", "u-child", "u-blocked"]


def test_origin_edges_link_child_to_the_task_that_caused_it() -> None:
    rows = graph.live_rows(_rows())

    assert graph.origin_edges(rows) == [
        ("LINEAGE-1kG0aaaa", "LINEAGE-9kG0bbbb"),
        ("LINEAGE-9kG0bbbb", "MOSAIC-1kG0cccc"),
    ]


def test_dependency_edges_resolve_the_uuid_keyed_depends_list() -> None:
    rows = graph.live_rows(_rows())

    assert graph.dependency_edges(rows) == [
        ("LINEAGE-1kG0aaaa", "LINEAGE-9kG0bbbb"),
        ("LINEAGE-1kG0aaaa", "MOSAIC-1kG0cccc"),
        ("LINEAGE-9kG0bbbb", "MOSAIC-1kG0cccc"),
    ]


def test_review_edges_map_reviewer_threads_onto_the_lanes_they_file_from() -> None:
    rows = graph.live_rows(_rows())

    assert dict(graph.review_edges(rows)) == {
        ("spice-g", "spice-a"): 1,
        ("spice-a", "spice-z"): 1,
    }


def test_phase_edges_count_the_route_each_task_actually_walked() -> None:
    rows = graph.live_rows(_rows())

    assert dict(graph.phase_edges(rows)) == {
        ("(filed)", "todo"): 2,
        ("(filed)", "plan"): 1,
        ("plan", "todo"): 1,
        ("todo", "review"): 2,
        ("review", "(completed)"): 2,
    }


def test_node_id_rewrites_handles_mermaid_grammar_would_reject() -> None:
    assert graph.node_id("LINEAGE-1kG0aaaa") == "LINEAGE_1kG0aaaa"
    assert graph.node_id("9kG0bbbb") == "n9kG0bbbb"

    with pytest.raises(SpiceError, match="empty handle"):
        graph.node_id("")


def test_node_label_neutralizes_quote_and_pound_and_truncates() -> None:
    overlong = "x" * (graph.LABEL_LIMIT + 16)

    assert graph.node_label('say "hi" about #7') == "say 'hi' about &num;7"
    assert graph.node_label(overlong).endswith("…")
    assert len(graph.node_label(overlong)) == graph.LABEL_LIMIT


def test_render_rejects_an_unknown_view() -> None:
    with pytest.raises(SpiceError, match="unknown graph view"):
        graph.render("lineage", _rows())


def test_render_annotates_every_advertised_view() -> None:
    rendered = _diagrams()

    assert sorted(rendered) == sorted(graph.VIEWS)
    assert all(text.startswith("%% ") for text in rendered.values())


def test_every_emitted_diagram_renders_through_real_mermaid(tmp_path: Path) -> None:
    """The load-bearing test: each view is laid out by mermaid in Chromium.

    A skip here means browser coverage did not run, which is not the same as
    the diagrams being fine — the two reasons are reported separately so a
    missing dependency is never read as a pass.
    """
    if shutil.which("node") is None:
        pytest.skip("node is required")
    if not MERMAID_BUNDLE.is_file():
        pytest.skip(f"missing Node dependencies: run npm install in {PROJECT_ROOT}")

    diagrams = _diagrams()
    payload = tmp_path / "diagrams.json"
    payload.write_text(json.dumps(diagrams), encoding="utf-8")

    result = subprocess.run(
        ["node", str(RENDERER), str(payload)],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
        timeout=180,
    )

    assert result.returncode == 0, result.stderr
    reported = json.loads(result.stdout)
    assert sorted(reported) == sorted(graph.VIEWS)
    for view in graph.VIEWS:
        assert reported[view]["ok"] is True, f"{view}: {reported[view]['error']}"
        assert reported[view]["svgLength"] > 0
