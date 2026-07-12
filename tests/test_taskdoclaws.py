"""Board-level laws binding task-document apply and ledger export."""

from __future__ import annotations

import pytest

from spice.tasks import config, identity, ops
from spice.tasks.markdown.apply import apply_document, load_family_rows, plan_document
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import graph_signature
from spice.tasks.markdown.ledger import export_document, export_ledger

from tests.test_tasks import task_repo
from tests.test_taskorigin import ACK_KEY

__all__ = ["task_repo"]


BOARD_CORPUS = {
    "named-tree": (
        "# Release\n"
        "Acceptance: release complete\n"
        "Flow: todo\n"
        "## Build\n"
        "Acceptance: artifact built\n"
        "Flow: todo\n"
        "## Verify\n"
        "Acceptance: checks pass\n"
        "Flow: todo\n"
    ),
    "synthetic-root": (
        "Flow: todo\n\n"
        "- First track\n"
        "  Acceptance: first complete\n"
        "  Flow: todo\n"
        "- Second track\n"
        "  Acceptance: second complete\n"
        "  Flow: todo\n"
    ),
    "cross-edge-fields-annotation": (
        "# Rollout\n"
        "Acceptance: rollout complete\n"
        "Priority: high\n"
        "Flow: todo\n"
        "Tags: release, staging\n"
        "> authored context\n"
        "## Prepare\n"
        "Acceptance: preparation complete\n"
        "Flow: todo\n"
        "## Promote\n"
        "Acceptance: promotion complete\n"
        "After: prepare\n"
        "Flow: todo\n"
    ),
}


@pytest.mark.parametrize("source", BOARD_CORPUS.values(), ids=BOARD_CORPUS)
def test_apply_ledger_convergence_fixed_point_and_steady_reentrancy(task_repo, source):
    assert task_repo.is_dir()
    document = parse(source)
    apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    family = load_family_rows("task.unit", f"ack:{ACK_KEY}")
    root_slug = document.nodes[document.root].slug
    root = identity.render_handle(
        next(row for row in family if row[config.TASKDOC_ID_UDA] == root_slug)
    )

    first_plan = plan_document(document, project="task.unit", origin=f"ack:{ACK_KEY}")
    second_plan = plan_document(document, project="task.unit", origin=f"ack:{ACK_KEY}")
    ledger = export_ledger(root)

    assert first_plan == second_plan
    assert graph_signature(parse(ledger)) == graph_signature(document)
    assert export_document(parse(ledger)) == ledger


def test_idempotence_repeats_reused_loose_drift_and_warn_facts(task_repo):
    assert task_repo.is_dir()
    initial = parse(
        "# Root\n"
        "Acceptance: root complete\n"
        "Flow: todo\n"
        "- Drift\n"
        "  Original board body\n"
        "  Acceptance: drift complete\n"
        "  Flow: todo\n"
        "- Loose\n"
        "  Acceptance: loose complete\n"
        "  Flow: todo\n"
    )
    apply_document(
        initial,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    family = load_family_rows("task.unit", f"ack:{ACK_KEY}")
    handles = {
        str(row[config.TASKDOC_ID_UDA]): identity.render_handle(row) for row in family
    }
    ops.claim(handles["drift"])
    ops.unclaim(handles["drift"])
    revised = parse(
        "# Root\n"
        "Acceptance: root complete\n"
        "Flow: todo\n"
        "- [x] Drift\n"
        "  Revised document body\n"
        "  Acceptance: drift complete\n"
        "  Flow: todo\n"
    )
    apply_document(
        revised,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    before = load_family_rows("task.unit", f"ack:{ACK_KEY}")

    first_report = apply_document(
        revised,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    middle = load_family_rows("task.unit", f"ack:{ACK_KEY}")
    second_report = apply_document(
        revised,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    after = load_family_rows("task.unit", f"ack:{ACK_KEY}")

    assert first_report == second_report
    assert before == middle == after
    assert first_report.splitlines() == [
        f"root {handles['root']}",
        f"reused root {handles['root']}",
        f"reused drift {handles['drift']}",
        f"loose loose {handles['loose']}",
        f"drift drift {handles['drift']} description",
        "warn 4 checked-discarded checked marker stripped; the board owns status",
    ]
