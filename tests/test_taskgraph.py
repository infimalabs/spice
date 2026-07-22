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

from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.tasks import graph
from spice.tasks.graphs import registry

PROJECT_ROOT = Path(__file__).resolve().parent.parent
RENDERER = PROJECT_ROOT / "tests" / "browser" / "task_graph_mermaid_render.js"
MERMAID_BUNDLE = PROJECT_ROOT / "node_modules" / "mermaid" / "dist" / "mermaid.min.js"


def _rows() -> list[dict]:
    """A small board carrying every relationship type at once.

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
            "phase_i": 1,
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
            "phase_i": 2,
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
            "phase_i": 0,
        },
        {
            "uuid": "u-deleted",
            "incepted": "1kG0dddd",
            "project": "smoke",
            "description": "Smoke residue that must not reach any view",
            "status": "deleted",
            "origin": "task:LINEAGE-1kG0aaaa",
            "phase_0": "todo",
            "phase_i": 0,
        },
    ]


def _diagrams() -> dict[str, str]:
    """Every advertised view, rendered from the fixture board."""
    rows = _rows()
    return {view: graph.render(view, rows) for view in graph.VIEWS}


def test_live_rows_drop_deleted_smoke_residue() -> None:
    rows = graph.live_rows(_rows())

    assert [row["uuid"] for row in rows] == ["u-seed", "u-child", "u-blocked"]


def test_canned_derivations_accept_export_shapes() -> None:
    from spice.tasks.graphs import derive

    row = {"entry": "20260721T035259Z", "project": ".oops.workflow"}

    assert derive.epoch(row, "entry") is not None
    assert derive.stem(row) == "oops"


def test_task_graph_cli_passes_the_inception_ceiling_to_rendering(
    monkeypatch, capsys
) -> None:
    calls: list[tuple[str, str, str]] = []

    def fake_render(view: str, *, ceiling: str = "", check_aspect: str = "") -> str:
        calls.append((view, ceiling, check_aspect))
        return "%% fixed snapshot"

    monkeypatch.setattr(graph, "render", fake_render)
    args = build_parser().parse_args(
        [
            "task",
            "graph",
            "20-daily-throughput-xy",
            "--ceiling",
            "LINEAGE-9kG0bbbb",
            "--check-aspect",
            "1100x560",
        ]
    )

    assert args.func(args) == 0
    assert capsys.readouterr().out == "%% fixed snapshot\n"
    assert calls == [("20-daily-throughput-xy", "LINEAGE-9kG0bbbb", "1100x560")]


def test_fixed_ceiling_is_reproducible_across_live_board_churn(monkeypatch) -> None:
    board = _rows()
    monkeypatch.setattr(graph.tw, "export", lambda: [dict(row) for row in board])
    ceiling = "LINEAGE-9kG0bbbb"

    before = {view: graph.render(view, ceiling=ceiling) for view in graph.VIEWS}
    board.append(
        {
            "uuid": "u-future",
            "incepted": "BkG0bbbb",
            "project": "task.lineage",
            "description": "Post-ceiling row touching every graph",
            "status": "completed",
            "origin": "task:LINEAGE-1kG0aaaa",
            "origin_worktree": "worktrees/spice-z",
            "origin_thread": "thread-z",
            "review_author": "thread-g",
            "depends": ["u-seed"],
            "phase_0": "todo",
            "phase_1": "review",
            "phase_i": 1,
        }
    )
    after = {view: graph.render(view, ceiling=ceiling) for view in graph.VIEWS}

    assert after == before
    assert all(
        "%% Snapshot ceiling: incepted <= 9kG0bbbb" in diagram
        for diagram in after.values()
    )


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


def test_phase_edges_exclude_configured_steps_the_task_has_not_reached() -> None:
    rows = [
        {
            "phase_0": "plan",
            "phase_1": "todo",
            "phase_2": "review",
            "phase_i": 0,
            "status": "pending",
        },
        {
            "phase_0": "plan",
            "phase_1": "todo",
            "phase_2": "review",
            "phase_i": 2,
            "status": "completed",
        },
    ]

    assert dict(graph.phase_edges(rows)) == {
        ("(filed)", "plan"): 2,
        ("plan", "todo"): 1,
        ("todo", "review"): 1,
        ("review", "(completed)"): 1,
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


def test_canned_registry_is_the_complete_handout_selection() -> None:
    expected = (
        "29-integration-gitgraph",
        "23-hour-of-day-xy",
        "15-reviewer-strictness-xy",
        "05-phase-flow-sankey",
        "26-era-timeline",
        "13b-review-matrix-sankey",
        "17-stem-quadrant",
        "20-daily-throughput-xy",
        "30-origin-kind-sankey",
        "07-origin-family-1",
        "01-project-stems-xy",
        "06-lifecycle-state",
        "24b-final-phase-turnaround-xy",
        "12-lineage-handoff-sankey",
        "18-friction-oops-board",
        "02-agent-worktrees-xy",
        "03-project-tree-mindmap",
        "04-agent-to-stem-sankey",
        "07-origin-family-2",
        "07-origin-family-3",
        "08-origin-deepest-spines",
        "09-ack-seeding-fanout",
        "10-dependency-component-1",
        "10-dependency-component-2",
        "11-taskdoc-families",
        "13-review-network",
        "14-review-findings-pie",
        "16-stem-difficulty-xy",
        "19-priority-to-stem-sankey",
        "21-cumulative-burnup-xy",
        "22-lane-concurrency-xy",
        "24-cycle-time-xy",
        "25-campaign-gantt",
        "27-task-verbs-xy",
        "28-record-schema-er",
        "31-title-length-xy",
        "32-board-at-a-glance",
    )

    assert registry.NAMES == expected
    assert {cut.family for cut in registry.CUTS} == {
        "flow",
        "magnitude",
        "time",
        "topology",
    }


def test_canned_registry_bakes_in_exactly_eight_unique_palette_slots() -> None:
    assert len(registry.PALETTE) == 8
    assert len(set(registry.PALETTE)) == 8
    for name in registry.NAMES:
        rendered = graph.render(name, _rows())
        assert registry.PALETTE[0] in rendered


def test_aspect_guard_admits_the_whole_shipped_range() -> None:
    """Both bounds are inclusive, and the guard hands back the ratio it measured.

    Asserting the returned ratio pins the two shipped limits with literals. A
    bound that silently widened would raise nothing at either edge, so a test
    that only watched for errors would keep passing while the gate it guards
    stopped meaning anything.
    """
    name = registry.NAMES[0]

    assert registry.validate_aspect(name, 1000, 4000) == 0.25
    assert registry.validate_aspect(name, 1200, 600) == 2.0
    assert registry.validate_aspect(name, 4000, 800) == 5.0


def test_check_aspect_parses_the_dimensions_the_browser_reports() -> None:
    """`--check-aspect` carries a real measurement, so parsing is the contract.

    Chromium reports fractional dimensions and the flag is typed by hand, so
    the shipped pattern accepts decimals, either case of the separator, and
    surrounding space. Each of those is a promise the CLI makes to its caller.
    """
    assert registry.parse_aspect("1200x660") == (1200.0, 660.0)
    assert registry.parse_aspect("  1100X560.5  ") == (1100.0, 560.5)

    name = registry.NAMES[0]
    rendered = graph.render(name, _rows(), check_aspect="1200x660")

    assert rendered == graph.render(name, _rows())


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

    diagrams = {
        **_diagrams(),
        **{f"empty_{view}": graph.render(view, []) for view in graph.VIEWS},
    }
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
    assert sorted(reported) == sorted(diagrams)
    for view in diagrams:
        assert reported[view]["ok"] is True, f"{view}: {reported[view]['error']}"
        assert reported[view]["svgLength"] > 0
        if view in registry.NAMES:
            registry.validate_aspect(
                view, reported[view]["width"], reported[view]["height"]
            )
