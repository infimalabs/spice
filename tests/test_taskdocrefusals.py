"""Every documented refusal fires its exact sentence before any board write.

The Refusals table in ``docs/design/accepted/task-documents.md`` promises
that everything that can fail fails loudly, completely, and before any write: a
single ``spice: <sentence>`` on stderr, exit code 2, zero writes. This suite is
the messages mirror of that table -- each row gets a positive test asserting the
exact CLI stderr sentence, exit code 2, and a byte-identical board afterward
(the fire-before-write guarantee).

The boarding note counted thirteen rows; the table now carries fourteen, and
the coverage matrix below covers all fourteen.
"""

from __future__ import annotations

import io
from dataclasses import dataclass

import pytest

from spice.cli import entry
from spice.tasks import config, create, tw

from tests.test_ingest import _family_task
from tests.test_tasks import task_repo
from tests.test_taskorigin import ACK_KEY

__all__ = ["task_repo"]


@dataclass(frozen=True)
class RefusalCase:
    code: str
    source: str
    message: str


# Rows a parsed document alone triggers: parse-time refusals surface through
# apply's plan validation, and the acceptance-pipe row is caught there too. Each
# applies against an empty board, so zero writes reads as a still-empty board.
DOCUMENT_REFUSALS = (
    RefusalCase("empty", "", "task document is empty"),
    RefusalCase(
        "no-nodes",
        "just prose\n",
        "task document has no nodes (content but no structure)",
    ),
    RefusalCase(
        "no-ascii-title",
        "# 数字\n",
        "title has no ASCII words: 数字 (line 1)",
    ),
    RefusalCase(
        "duplicate-title",
        "# Root\n## Same\n## Same\n",
        "duplicate title in document: same (lines 2 and 3; nothing distinguishes them)",
    ),
    RefusalCase(
        "synthetic-root-collision",
        "- Document root\n- Other\n",
        "title collides with the synthetic root: Document root (line 1); rename or nest it",
    ),
    RefusalCase(
        "unknown-after",
        "# Root\n## A\nAfter: ghost\n",
        "unknown After target: ghost (line 3)",
    ),
    RefusalCase(
        "ambiguous-after",
        "# Root\n## Phase 1\n- Same\n## Phase 2\n- Same\n## Ref\nAfter: same\n",
        "ambiguous After target: same (title is duplicated; use a qualified slug)",
    ),
    RefusalCase(
        "invalid-priority",
        "# Root\nPriority: bogus\n",
        "invalid priority: bogus",
    ),
    RefusalCase(
        "invalid-flow",
        "# Root\nFlow: bogus\n",
        "invalid flow phase: bogus",
    ),
    RefusalCase(
        "dependency-cycle",
        "# Root\n## A\nAfter: b\n## B\nAfter: a\n",
        "dependency cycle at a",
    ),
    RefusalCase(
        "acceptance-pipe",
        "# Root\nAcceptance: first | second\nFlow: todo\n",
        "acceptance criterion on root contains '|'",
    ),
)

# The remaining rows are board-level: reference resolution and family matching
# fire before a write, so each has its own test below.
BOARD_REFUSAL_CODES = frozenset(
    {"missing-project", "missing-origin", "family-ambiguity"}
)

DOCUMENTED_REFUSAL_CODES = frozenset(
    {case.code for case in DOCUMENT_REFUSALS} | BOARD_REFUSAL_CODES
)


def _run_ingest(source: str, monkeypatch, *args: str) -> int:
    monkeypatch.setattr("sys.stdin", io.StringIO(source))
    return entry.main(
        ["task", "--backend", str(config.backend_root()), "ingest", "-", *args]
    )


@pytest.mark.parametrize("case", DOCUMENT_REFUSALS, ids=lambda case: case.code)
def test_document_refusal_fires_exact_sentence_before_any_write(
    task_repo, case, monkeypatch, capsys
):
    code = _run_ingest(
        case.source,
        monkeypatch,
        "--project",
        "task.unit",
        "--origin",
        f"ack:{ACK_KEY}",
    )

    assert code == 2
    assert capsys.readouterr().err == f"spice: {case.message}\n"
    assert tw.export(["status:pending"]) == []


def test_missing_project_refuses_before_any_write(task_repo, monkeypatch, capsys):
    code = _run_ingest("# Root\n", monkeypatch, "--origin", f"ack:{ACK_KEY}")

    assert code == 2
    assert capsys.readouterr().err == (
        "spice: task ingest requires a project: pass --project <stem.child>, "
        "or run while holding an active claim to inherit its project\n"
    )
    assert tw.export(["status:pending"]) == []


def test_missing_origin_refuses_before_any_write(task_repo, monkeypatch, capsys):
    code = _run_ingest("# Root\n", monkeypatch, "--project", "task.unit")

    assert code == 2
    assert capsys.readouterr().err == f"spice: {create.TASK_ORIGIN_REQUIRED_ERROR}\n"
    assert tw.export(["status:pending"]) == []


def test_family_ambiguity_names_the_colliding_handles_before_any_write(
    task_repo, monkeypatch, capsys
):
    first = _family_task("First duplicate", slug="duplicate")
    second = _family_task("Second duplicate", slug="duplicate")
    before = tw.export(["status:pending"])

    code = _run_ingest(
        "# Duplicate\n",
        monkeypatch,
        "--project",
        "task.unit",
        "--origin",
        f"ack:{ACK_KEY}",
    )

    assert code == 2
    assert capsys.readouterr().err == (
        "spice: duplicate is ambiguous in family: "
        + ", ".join(sorted((first, second)))
        + "\n"
    )
    assert tw.export(["status:pending"]) == before


def test_refusal_matrix_covers_the_documented_rows(task_repo):
    assert DOCUMENTED_REFUSAL_CODES == {
        "acceptance-pipe",
        "ambiguous-after",
        "dependency-cycle",
        "duplicate-title",
        "empty",
        "family-ambiguity",
        "invalid-flow",
        "invalid-priority",
        "missing-origin",
        "missing-project",
        "no-ascii-title",
        "no-nodes",
        "synthetic-root-collision",
        "unknown-after",
    }
