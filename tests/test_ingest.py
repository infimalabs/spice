"""Ingest resolves origin and project from flags or the active claim.

Origin reuses the creation-path resolver; project inheritance from the active
claim is new ingest surface. Both resolve before any board write, so a missing
reference refuses instead of half-applying a document.
"""

from __future__ import annotations

import io

import pytest

from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.tasks import cli as task_cli
from spice.tasks import config, create, identity, ops, tw
from spice.tasks.markdown import apply
from spice.tasks.markdown.classifier import parse
from spice.tasks.markdown.dialect import (
    DOCUMENT_ROOT_SLUG,
    DOCUMENT_ROOT_TITLE,
    graph_signature,
    slugify,
)
from spice.tasks.markdown.ledger import (
    LEDGER_ROUND_TRIP_WARNING,
    export_document,
    render_ledger,
)

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
        origin="ack:1jNmXPHn",
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
    report = apply.execute_plan(plan)
    fresh = identity.resolve(root)

    assert plan.edge_drops == (apply.EdgeChange("root", "child"),)
    assert fresh["depends"] == [identity.uuid_of(identity.resolve(external))]
    assert report.splitlines()[1:] == [
        "edge-dropped root -> child",
        f"loose child {child}",
    ]


def test_apply_creates_dependency_postorder_with_atomic_identity_and_no_auto_due(
    task_repo,
):
    assert task_repo.is_dir()
    document = parse(
        "# Root\n"
        "Acceptance: root complete\n"
        "Priority: high\n"
        "Flow: todo\n"
        "## Child\n"
        "Acceptance: child complete\n"
        "Flow: todo\n"
    )

    report = apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    rows = apply.load_family_rows("task.unit", f"ack:{ACK_KEY}")
    by_slug = {str(row[config.TASKDOC_ID_UDA]): row for row in rows}
    root = by_slug["root"]
    child = by_slug["child"]
    root_handle = identity.render_handle(root)
    child_handle = identity.render_handle(child)
    assert str(child[config.TASKDOC_PARENT_UDA]) == "root"
    assert str(root.get(config.TASKDOC_PARENT_UDA) or "") == ""
    assert root["depends"] == [identity.uuid_of(child)]
    assert root["priority"] == "H"
    assert str(root.get("due") or "") == ""
    assert child["incepted"] < root["incepted"]
    assert report.splitlines() == [
        f"root {root_handle}",
        f"created child {child_handle}",
        f"created root {root_handle}",
    ]


def test_second_apply_is_reused_and_keeps_the_board_byte_identical(task_repo):
    assert task_repo.is_dir()
    document = parse(
        "# Root\n"
        "Acceptance: root complete\n"
        "Flow: todo\n"
        "## Child\n"
        "Acceptance: child complete\n"
        "Flow: todo\n"
    )
    apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    before = apply.load_family_rows("task.unit", f"ack:{ACK_KEY}")
    handles = {
        str(row[config.TASKDOC_ID_UDA]): identity.render_handle(row) for row in before
    }

    report = apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    assert report.splitlines() == [
        f"root {handles['root']}",
        f"reused root {handles['root']}",
        f"reused child {handles['child']}",
    ]
    assert apply.load_family_rows("task.unit", f"ack:{ACK_KEY}") == before


def test_apply_lands_statement_fields_annotations_and_family_edges(task_repo):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    tw.run(
        [
            identity.uuid_of(identity.resolve(root)),
            "modify",
            "+legacy",
            "due:2026-09-01",
        ]
    )
    document = parse(
        "# Root\n"
        "Landed body\n"
        "Acceptance: landed criterion\n"
        "Priority: high\n"
        "Flow: todo\n"
        "Tags: importer, perf\n"
        "> landed note\n"
        "## Child\n"
        "Acceptance: family matching fixture\n"
        "Flow: todo\n"
    )

    report = apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    fresh = identity.resolve(root)
    annotations = [
        str(annotation.get("description") or "")
        for annotation in fresh.get("annotations") or ()
    ]
    assert fresh["task_description"] == "Landed body"
    assert fresh["acceptance"] == "landed criterion"
    assert fresh["priority"] == "H"
    assert sorted(fresh["tags"]) == ["importer", "perf"]
    assert str(fresh.get("due") or "") == ""
    assert fresh["depends"] == [identity.uuid_of(identity.resolve(child))]
    assert annotations == ["> landed note"]
    assert report.splitlines() == [
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


def test_write_time_settlement_demotes_field_and_edge_writes_to_drift(
    task_repo, monkeypatch
):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    original_fresh_row = apply._fresh_row
    settled: set[str] = set()

    def settle_root(row):
        slug = str(row.get(config.TASKDOC_ID_UDA) or "")
        if slug == "root" and slug not in settled:
            settled.add(slug)
            tw.run(
                [
                    identity.uuid_of(row),
                    "modify",
                    "start:now",
                    f"claim_at:{tw.now_iso()}",
                ]
            )
        return original_fresh_row(row)

    monkeypatch.setattr(apply, "_fresh_row", settle_root)
    document = parse(
        "# Root\n"
        "Document rewrite after planning\n"
        "Acceptance: family matching fixture\n"
        "Flow: todo\n"
        "## Child\n"
        "Acceptance: family matching fixture\n"
        "Flow: todo\n"
    )

    report = apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )

    fresh = identity.resolve(root)
    assert settled == {"root"}
    assert str(fresh.get("task_description") or "") == ""
    assert list(fresh.get("depends") or ()) == []
    assert report.splitlines() == [
        f"root {root}",
        f"reused child {child}",
        f"drift root {root} description",
        f"drift root {root} after",
    ]


def test_acceptance_pipe_refuses_apply_before_creating_rows(task_repo):
    assert task_repo.is_dir()

    with pytest.raises(SpiceError, match=r"acceptance criterion on root contains '\|'"):
        apply.apply_document(
            parse("# Root\nAcceptance: first | second\nFlow: todo\n"),
            project="task.unit",
            origin=f"ack:{ACK_KEY}",
        )

    assert tw.export(["status:pending"]) == []


def test_ledger_reconstructs_applied_family_without_board_owned_edges(task_repo):
    assert task_repo.is_dir()
    document = parse(
        "# Root\n"
        "Root body\n"
        "Acceptance: root complete\n"
        "Priority: high\n"
        "Flow: todo\n"
        "Tags: importer, perf\n"
        "> document note\n"
        "## Child\n"
        "Acceptance: child complete\n"
        "Flow: todo\n"
    )
    apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    rows = apply.load_family_rows("task.unit", f"ack:{ACK_KEY}")
    root = next(
        identity.render_handle(row)
        for row in rows
        if row[config.TASKDOC_ID_UDA] == "root"
    )
    external = create.add(
        "Board-owned prerequisite",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["outside family"],
        origin=f"ack:{ACK_KEY}",
    )
    deleted_external = create.add(
        "Deleted board-owned prerequisite",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["deleted outside family"],
        origin=f"ack:{ACK_KEY}",
    )
    ops.depends(root, [external, deleted_external])
    ops.delete(deleted_external, "deleted external ledger fixture")
    ops.note(root, "ack 1jN54zJJ: runtime steering handled")

    rendered, _ = render_ledger(root)
    reparsed = parse(rendered)

    assert graph_signature(reparsed) == graph_signature(document)
    assert export_document(reparsed) == rendered


@pytest.mark.parametrize(
    ("parent", "message"),
    (
        ("missing-parent", "child has unknown taskdoc_parent: missing-parent"),
        ("child", "child cannot be its own taskdoc_parent"),
    ),
    ids=("missing", "self"),
)
def test_ledger_refuses_invalid_containment_metadata(task_repo, parent, message):
    assert task_repo.is_dir()
    root = _family_task("Root", slug="root")
    child = _family_task("Child", slug="child", parent="root")
    child_row = identity.resolve(child)
    tw.run(
        [
            identity.uuid_of(child_row),
            "modify",
            f"{config.TASKDOC_PARENT_UDA}:{parent}",
        ]
    )

    with pytest.raises(SpiceError) as error:
        render_ledger(root)

    assert str(error.value) == message


def test_ledger_exports_plain_board_dependency_family(task_repo, capsys):
    assert task_repo.is_dir()
    parent = create.add(
        "Plain board parent",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["parent coordinates its prerequisites"],
        origin=f"ack:{ACK_KEY}",
    )
    first = create.add(
        "First plain prerequisite",
        project="task.unit",
        priority="high",
        flow=["todo"],
        acceptance=["first prerequisite is complete"],
        origin=f"task:{parent}",
    )
    second = create.add(
        "Second plain prerequisite",
        project="serve.unit",
        priority="none",
        flow=["todo"],
        acceptance=["second prerequisite is complete"],
        origin=f"task:{parent}",
    )
    ops.depends(parent, [first, second])

    ledger_args = build_parser().parse_args(
        ["task", "--backend", str(config.backend_root()), "ledger", first]
    )

    assert task_cli.handle(ledger_args) == 0
    rendered = capsys.readouterr().out
    document = parse(rendered)
    assert {node.title for node in document.nodes} == {
        f"Plain board parent {parent}",
        f"First plain prerequisite {first}",
        f"Second plain prerequisite {second}",
        DOCUMENT_ROOT_TITLE,
    }
    parent_slug = slugify(f"Plain board parent {parent}")
    first_slug = slugify(f"First plain prerequisite {first}")
    second_slug = slugify(f"Second plain prerequisite {second}")
    assert graph_signature(document)[1] == frozenset(
        {
            (DOCUMENT_ROOT_SLUG, parent_slug),
            (DOCUMENT_ROOT_SLUG, first_slug),
            (DOCUMENT_ROOT_SLUG, second_slug),
            (parent_slug, first_slug),
            (parent_slug, second_slug),
        }
    )
    assert export_document(document) == rendered


def test_ledger_warns_and_exits_zero_on_free_text_annotation_family(task_repo, capsys):
    assert task_repo.is_dir()
    parent = create.add(
        "Free-text annotation parent",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["parent carries an operator note"],
        origin=f"ack:{ACK_KEY}",
    )
    child = create.add(
        "Annotated prerequisite",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["prerequisite is complete"],
        origin=f"task:{parent}",
    )
    ops.depends(parent, [child])
    # Free-text prose renders as a verbatim block and folds back into the node
    # description on re-read, so this family cannot round-trip.
    ops.note(parent, "Plan verification confirmed the contract still matches.")

    ledger_args = build_parser().parse_args(
        ["task", "--backend", str(config.backend_root()), "ledger", child]
    )

    assert task_cli.handle(ledger_args) == 0
    captured = capsys.readouterr()
    document = parse(captured.out)
    assert {node.title for node in document.nodes} == {
        f"Free-text annotation parent {parent}",
        f"Annotated prerequisite {child}",
        DOCUMENT_ROOT_TITLE,
    }
    assert captured.err == LEDGER_ROUND_TRIP_WARNING + "\n"


def test_ledger_round_trippable_family_emits_exact_rendered_output(task_repo, capsys):
    assert task_repo.is_dir()
    parent = create.add(
        "Clean board parent",
        project="task.unit",
        priority="none",
        flow=["todo"],
        acceptance=["parent coordinates its prerequisite"],
        origin=f"ack:{ACK_KEY}",
    )
    child = create.add(
        "Clean prerequisite",
        project="task.unit",
        priority="high",
        flow=["todo"],
        acceptance=["prerequisite is complete"],
        origin=f"task:{parent}",
    )
    ops.depends(parent, [child])

    ledger_args = build_parser().parse_args(
        ["task", "--backend", str(config.backend_root()), "ledger", child]
    )
    expected = render_ledger(child)[0]

    assert task_cli.handle(ledger_args) == 0
    captured = capsys.readouterr()
    output_events = [
        (channel, text)
        for channel, text in (("stdout", captured.out), ("stderr", captured.err))
        if text
    ]
    document = parse(captured.out)
    assert {node.title for node in document.nodes} == {
        f"Clean board parent {parent}",
        f"Clean prerequisite {child}",
        DOCUMENT_ROOT_TITLE,
    }
    assert output_events == [("stdout", expected)]


@pytest.mark.parametrize(
    "source",
    (
        "# Document root\nAcceptance: a real reserved-title task\nFlow: todo\n",
        (
            "- First leaf\n"
            "  Acceptance: first complete\n"
            "  Flow: todo\n"
            "- Second leaf\n"
            "  Acceptance: second complete\n"
            "  Flow: todo\n"
        ),
    ),
    ids=("real-document-root", "synthetic-document-root"),
)
def test_ledger_distinguishes_real_and_synthetic_document_roots(task_repo, source):
    assert task_repo.is_dir()
    document = parse(source)
    report = apply.apply_document(
        document,
        project="task.unit",
        origin=f"ack:{ACK_KEY}",
    )
    root = report.splitlines()[0].removeprefix("root ")
    if document.nodes[document.root].kind == "document":
        document.nodes[document.root].flow = ["plan", "todo", "review"]

    rendered, _ = render_ledger(root)

    assert graph_signature(parse(rendered)) == graph_signature(document)


def test_cli_ingest_dash_and_ledger_run_the_task_document_dialect(
    task_repo, monkeypatch, capsys
):
    assert task_repo.is_dir()
    source = "# Root\nAcceptance: complete\nFlow: todo\n"
    monkeypatch.setattr("sys.stdin", io.StringIO(source))
    ingest_args = build_parser().parse_args(
        [
            "task",
            "ingest",
            "-",
            "--project",
            "task.unit",
            "--origin",
            f"ack:{ACK_KEY}",
        ]
    )

    assert task_cli.handle(ingest_args) == 0
    ingest_report = capsys.readouterr().out
    family = apply.load_family_rows("task.unit", f"ack:{ACK_KEY}")
    root = identity.render_handle(family[0])
    assert ingest_report.splitlines() == [
        f"root {root}",
        f"created root {root}",
    ]

    ledger_args = build_parser().parse_args(["task", "ledger", root])
    assert task_cli.handle(ledger_args) == 0
    assert capsys.readouterr().out == render_ledger(root)[0]
