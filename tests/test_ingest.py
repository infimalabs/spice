"""Ingest resolves origin and project from flags or the active claim.

Origin reuses the creation-path resolver; project inheritance from the active
claim is new ingest surface. Both resolve before any board write, so a missing
reference refuses instead of half-applying a document.
"""

from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.tasks import config, create, identity, ops, tw
from spice.tasks.markdown import apply
from spice.tasks.markdown.classifier import parse

from tests.test_tasks import task_repo
from tests.test_taskorigin import ACK_KEY, _seed_task

__all__ = ["task_repo"]


def _actor() -> str:
    return tw.canonical_actor(tw.current_actor())


def _family_task(
    title: str,
    *,
    slug: str,
    project: str = "task.unit",
    origin: str = f"ack:{ACK_KEY}",
    parent: str = "",
) -> str:
    return create.add_one(
        title=title,
        project=project,
        priority="none",
        flow=["todo"],
        tags=[],
        after=[],
        acceptance=["family matching fixture"],
        wait=None,
        claim=False,
        origin=origin,
        extra=[
            f"{config.TASKDOC_ID_UDA}:{slug}",
            f"{config.TASKDOC_PARENT_UDA}:{parent}",
        ],
    )


def test_explicit_flags_resolve_origin_and_project(task_repo):
    assert task_repo.is_dir()

    project, origin = apply.resolve_ingest_target(
        _actor(), project="task.unit", origin=f"ack:{ACK_KEY}"
    )

    assert project == "task.unit"
    assert origin == f"ack:{ACK_KEY}"


def test_active_claim_supplies_origin_and_project(task_repo):
    assert task_repo.is_dir()
    root = _seed_task("Ingest inherits the active claim")
    ops.claim(root)

    project, origin = apply.resolve_ingest_target(_actor(), project=None, origin=None)

    assert origin == f"task:{root}"
    assert project == "task.unit"


def test_explicit_project_overrides_the_active_claim_project(task_repo):
    assert task_repo.is_dir()
    root = _seed_task("Claim on one project, ingest onto another")
    ops.claim(root)

    project, origin = apply.resolve_ingest_target(
        _actor(), project="task.plan", origin=None
    )

    # Origin still inherits the claim; the explicit project wins over it.
    assert origin == f"task:{root}"
    assert project == "task.plan"


def test_missing_origin_without_claim_refuses_before_any_write(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="requires an origin"):
        apply.ingest_path("never-read.md", project="task.unit", origin=None)

    assert tw.export(["status:pending"]) == []


def test_missing_project_without_claim_refuses_before_any_write(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match="requires a project"):
        apply.ingest_path("never-read.md", project=None, origin=f"ack:{ACK_KEY}")

    assert tw.export(["status:pending"]) == []


def test_family_matching_scopes_rows_and_keeps_completed_work(task_repo):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    gone = _family_task("Gone", slug="gone", parent="root")
    _family_task("Other project child", slug="child", project="task.plan")
    _family_task(
        "Other origin child",
        slug="child",
        origin="ack:20260104T000000000005Z",
    )
    create.add(
        "Ordinary same-family row",
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
        priority="none",
    )
    ops.claim(root)
    ops.done(root, validation=["completed family rows remain satisfied matches"])
    ops.delete(gone, "deleted family rows never match")

    matched = apply.match_family(
        parse("# Root\n## Child\n## Gone\n"),
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    assert sorted(matched.by_slug) == ["child", "root"]
    assert identity.render_handle(matched.by_slug["root"]) == root
    assert identity.render_handle(matched.by_slug["child"]) == child
    assert sorted(
        (
            str(row[config.TASKDOC_ID_UDA]),
            str(row["status"]),
            str(row.get(config.TASKDOC_PARENT_UDA) or ""),
        )
        for row in matched.rows
    ) == [("child", "pending", "root"), ("root", "completed", "")]


def test_ambiguous_slug_names_family_handles(task_repo):
    assert task_repo.is_dir()
    first = _family_task("First duplicate", slug="duplicate")
    second = _family_task("Second duplicate", slug="duplicate")

    with pytest.raises(SpiceError) as error:
        apply.match_family(
            parse("# Duplicate\n"),
            project="task.unit",
            origin=f"ack:{ACK_KEY}",
        )

    assert str(error.value) == (
        "duplicate is ambiguous in family: " + ", ".join(sorted((first, second)))
    )


def test_dry_run_reports_dependency_postorder_and_keeps_board_identical(task_repo):
    assert task_repo.is_dir()
    document = parse(
        "# Root\n"
        "Acceptance: root complete\n"
        "Flow: todo\n"
        "## Child\n"
        "Acceptance: child complete\n"
        "Flow: todo\n"
    )
    before = tw.export(["status:pending"])

    report = apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
        dry_run=True,
    )

    lines = report.splitlines()
    root_handle = lines[0].removeprefix("root ")
    assert lines[1].startswith("created child ")
    assert lines[2] == f"created root {root_handle}"
    assert tw.export(["status:pending"]) == before


def test_plan_equalizes_fields_appends_annotations_and_orders_verbs(task_repo):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    document = parse(
        "# Root\n"
        "Root body\n"
        "Acceptance: rewritten criterion\n"
        "Priority: high\n"
        "Flow: todo\n"
        "Due: 2026-08-01\n"
        "Tags: Importer, perf\n"
        "> source note\n"
        "## Child\n"
        "Acceptance: family matching fixture\n"
        "Flow: todo\n"
    )

    plan = apply.plan_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    root_plan = next(item for item in plan.nodes if item.node.slug == "root")
    assert root_plan.handle == root
    assert root_plan.annotations == ("> source note",)
    assert {update.field: update.value for update in root_plan.updates} == {
        "description": "Root body",
        "acceptance": "rewritten criterion",
        "priority": "H",
        "due": "2026-08-01",
        "tags": ("importer", "perf"),
        "annotations": ("> source note",),
    }
    assert plan.edge_additions == (apply.EdgeChange("root", "child"),)
    assert plan.report().splitlines() == [
        f"root {root}",
        f"reused child {child}",
        f"updated root {root} description",
        f"updated root {root} acceptance",
        f"updated root {root} priority",
        f"updated root {root} due",
        f"updated root {root} tags",
        f"updated root {root} annotations",
        "edge-added root -> child",
    ]


def test_released_claim_stays_settled_while_new_annotations_append(task_repo):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    ops.claim(root)
    ops.unclaim(root)
    released = identity.resolve(root)
    document = parse(
        "# Root\n"
        "Board-owned rewrite\n"
        "Acceptance: changed after work began\n"
        "Flow: todo\n"
        "> later evidence\n"
        "## Child\n"
        "Acceptance: family matching fixture\n"
        "Flow: todo\n"
    )

    plan = apply.plan_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    root_plan = next(item for item in plan.nodes if item.node.slug == "root")
    assert released["claim_at"]
    assert root_plan.settled is True
    assert root_plan.updates == (
        apply.FieldUpdate("annotations", ("> later evidence",)),
    )
    assert [verb.render() for verb in plan.verbs] == [
        f"reused child {child}",
        f"updated root {root} annotations",
        f"drift root {root} description",
        f"drift root {root} acceptance",
        f"drift root {root} after",
    ]


def test_edge_diff_keeps_board_owned_dependencies_outside_the_family(task_repo):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    external = create.add(
        "External prerequisite",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["external fixture"],
        origin=f"ack:{ACK_KEY}",
    )
    ops.depends(root, [child, external])

    plan = apply.plan_document(
        parse("# Root\nAcceptance: family matching fixture\nFlow: todo\n"),
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    assert plan.edge_drops == (apply.EdgeChange("root", "child"),)
    assert [verb.render() for verb in plan.verbs] == [
        "edge-dropped root -> child",
        f"loose child {child}",
    ]
