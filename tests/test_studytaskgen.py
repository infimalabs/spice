"""Integration coverage for study-generated task scheduling and identity."""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.reachability import (
    ReachabilityFinding,
    SymbolReachabilityFinding,
)
from spice.studies.taskgen import StudyTaskSpec, create_study_tasks
from spice.studies.testquality import (
    AssertionFreeTestFinding,
    PrivateInternalCouplingFinding,
)
from spice.tasks import claimstate, config, create, identity, tw

ACTOR = "abababababababababababababababab"
ACK_ORIGIN = "ack:20260101T000000000000Z"
STUDY_ACTIONS = (
    "reachability",
    "symbol-reachability",
    "assertion-free-tests",
    "private-internals",
)
MODULE_FINDING = ReachabilityFinding(
    subject="spice.onlytest",
    path="spice/onlytest.py",
    only_test_imports=["tests/test_only.py"],
)
SYMBOL_FINDING = SymbolReachabilityFinding(
    module="spice.live",
    module_path="spice/live.py",
    symbol="only_tested",
    kind="function",
    only_test_imports=["tests/test_live.py"],
)
ASSERTION_FINDING = AssertionFreeTestFinding(
    path="tests/test_quality.py",
    test_name="test_without_assertion",
    line=7,
)
COUPLING_FINDING = PrivateInternalCouplingFinding(
    path="tests/test_private.py",
    test_name="test_private_contract",
    line=11,
    kind="import",
    target="spice.live._private",
)
STALE_COUPLING = (
    "tests/test_stale.py",
    "test_old_contract",
    "spice.old._private",
)
EXPECTED_PROJECTS = (
    "tests.exhaust",
    "tests.exhaust",
    "tests.quality",
    "tests.quality",
    "tests.quality",
)
EXPECTED_TAGS = (
    {"exhaust", "decision", "wire_in_delete_both"},
    {"exhaust", "symbol_reachability", "decision"},
    {"test_quality", "assertion_free", "decision"},
    {"test_quality", "private_internals", "decision"},
    {"test_quality", "private_internals", "cleanup"},
)
EXPECTED_ACCEPTANCE = (
    (
        "Resolve python module spice.onlytest by either wiring it into a production "
        "entry point or deleting spice/onlytest.py along with every test that "
        "imports it. | Current test-only importers: tests/test_only.py."
    ),
    (
        "Resolve python function spice.live.only_tested by wiring it into production "
        "reachability, deleting the symbol and tests that only import it, or "
        "documenting a reviewed allowlist when dynamic production reachability "
        "cannot be made explicit. | Current test-only importers: tests/test_live.py."
    ),
    (
        "Resolve assertion-free test tests/test_quality.py:7 test_without_assertion "
        "by adding an assertion that constrains behavior or deleting the test if it "
        "carries no useful signal."
    ),
    (
        "Resolve private/internal coupling import spice.live._private in "
        "tests/test_private.py:11 test_private_contract by asserting through public "
        "behavior, moving the seam into production API, or documenting a reviewed "
        "policy exception."
    ),
    (
        "Remove stale [tool.spice.policy] internal_couplings entry for "
        "tests/test_stale.py test_old_contract spice.old._private, or restore the "
        "reviewed coupling if it is still required."
    ),
)
EXPECTED_DESCRIPTIONS = (
    (
        "Study finding identity: reachability | python | module | spice.onlytest | "
        "spice/onlytest.py"
    ),
    (
        "Study finding identity: symbol-reachability | python | function | "
        "spice.live | only_tested | spice/live.py"
    ),
    (
        "Study finding identity: assertion-free-tests | tests/test_quality.py | "
        "test_without_assertion"
    ),
    (
        "Study finding identity: private-internals | tests/test_private.py | "
        "test_private_contract | import | spice.live._private"
    ),
    (
        "Study finding identity: private-internals-stale-exception | "
        "tests/test_stale.py | test_old_contract | spice.old._private"
    ),
)


@pytest.fixture
def study_task_backend(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo_root = Path(__file__).parents[1]
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo_root)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-study-taskgen")
    config.set_backend(str(backend))
    try:
        yield backend
    finally:
        config.set_backend(None)


def test_all_study_generators_create_reusable_deferred_tasks_visible_by_project(
    study_task_backend, monkeypatch, capsys
):
    _patch_study_scans(monkeypatch)

    first = {
        action: _run_task_generating_study(action, capsys) for action in STUDY_ACTIONS
    }
    repeated = {action: _run_task_generating_study(action, capsys) for action in first}

    assert repeated == first
    handles = [handle for generated in first.values() for handle in generated]
    rows = [identity.resolve(handle) for handle in handles]
    _assert_task_metadata(rows)

    text_output = _run_task_generating_study_text("reachability", capsys)
    assert f"task reused: {first['reachability'][0]}" in text_output
    assert "reachability: 1 test-only finding(s)" in text_output

    exhaust_output = _run_waiting_list(study_task_backend, "tests.exhaust", capsys)
    quality_output = _run_waiting_list(study_task_backend, "tests.quality", capsys)
    assert [handle in exhaust_output for handle in handles[:2]] == [True, True]
    assert [handle in quality_output for handle in handles[2:]] == [True, True, True]
    assert exhaust_output.count("tests.exhaust") == 2
    assert quality_output.count("tests.quality") == 3


def test_completed_study_finding_recurs_with_traceable_lineage(study_task_backend):
    spec = _task_spec("Recurring study finding", ("path.py", "symbol"))
    first = create_study_tasks([spec], origin=ACK_ORIGIN, print_created=False)[0]
    first_row = identity.resolve(first)
    tw.run([identity.uuid_of(first_row), "done"])
    completed_row = identity.resolve(first)

    second = create_study_tasks([spec], origin=ACK_ORIGIN, print_created=False)[0]
    second_row = identity.resolve(second)
    notes = [
        str(annotation.get("description") or "")
        for annotation in second_row.get("annotations") or []
    ]

    assert completed_row["status"] == "completed"
    assert second_row["status"] == "pending"
    assert f"study finding recurred after completed task {first}" in notes
    assert str(second_row.get("origin") or "") == ACK_ORIGIN


def test_immediate_study_task_inherits_active_claim_origin(study_task_backend):
    parent = create.add(
        "Parent work for generated finding",
        project="task.unit",
        origin=ACK_ORIGIN,
        acceptance=["active work can originate study findings"],
    )
    parent_row = identity.resolve(parent)
    claimstate.do_claim(identity.uuid_of(parent_row), ACTOR)

    child = create_study_tasks(
        [_task_spec("Immediate inherited finding", ("module.py", "finding"))],
        print_created=False,
    )[0]
    child_row = identity.resolve(child)

    assert str(child_row.get("origin") or "") == f"task:{parent}"
    assert str(child_row.get("wait") or "") == ""


def _patch_study_scans(monkeypatch) -> None:
    monkeypatch.setattr(
        studies_cli.reachability,
        "scan_reachability",
        lambda root, *, allowlist: [MODULE_FINDING],
    )
    monkeypatch.setattr(
        studies_cli.reachability,
        "scan_symbol_reachability",
        lambda root: [SYMBOL_FINDING],
    )
    monkeypatch.setattr(studies_cli.testquality, "test_paths", lambda root: [])
    monkeypatch.setattr(
        studies_cli.testquality,
        "scan_assertion_free_tests",
        lambda paths, *, root: [ASSERTION_FINDING],
    )
    monkeypatch.setattr(
        studies_cli.testquality,
        "scan_private_internal_coupling",
        lambda paths, *, root: [COUPLING_FINDING],
    )
    monkeypatch.setattr(
        studies_cli.testquality,
        "unmanaged_private_internal_couplings",
        lambda findings, *, repo_root, built_in_couplings: (
            [COUPLING_FINDING],
            [STALE_COUPLING],
        ),
    )


def _assert_task_metadata(rows) -> None:
    assert tuple(str(row.get("project") or "") for row in rows) == EXPECTED_PROJECTS
    assert tuple(str(row.get("origin") or "") for row in rows) == (ACK_ORIGIN,) * len(
        rows
    )
    assert tuple(str(row.get("wait") or "")[:4] for row in rows) == ("2099",) * len(
        rows
    )
    assert tuple(str(row.get("acceptance") or "") for row in rows) == (
        EXPECTED_ACCEPTANCE
    )
    assert tuple(
        tags.issubset(set(row.get("tags") or []))
        for tags, row in zip(EXPECTED_TAGS, rows, strict=True)
    ) == (True,) * len(rows)
    assert tuple(
        sum(str(tag).startswith("study_finding_") for tag in row.get("tags") or [])
        for row in rows
    ) == (1,) * len(rows)
    assert tuple(str(row.get("task_description") or "") for row in rows) == (
        EXPECTED_DESCRIPTIONS
    )


def _run_task_generating_study(action: str, capsys) -> list[str]:
    args = build_parser().parse_args(
        [
            "study",
            action,
            "--json",
            "--create-tasks",
            "--deferred",
            "--origin",
            ACK_ORIGIN,
        ]
    )
    assert args.func(args) == 1
    payload = json.loads(capsys.readouterr().out)
    return [str(handle) for handle in payload["createdTasks"]]


def _run_waiting_list(backend: Path, project: str, capsys) -> str:
    args = build_parser().parse_args(
        [
            "task",
            "--backend",
            str(backend),
            "list",
            "--all",
            "--status",
            "waiting",
            "--project",
            project,
        ]
    )
    assert args.func(args) == 0
    return capsys.readouterr().out


def _run_task_generating_study_text(action: str, capsys) -> str:
    args = build_parser().parse_args(
        [
            "study",
            action,
            "--create-tasks",
            "--deferred",
            "--origin",
            ACK_ORIGIN,
        ]
    )
    assert args.func(args) == 1
    return capsys.readouterr().out


def _task_spec(title: str, finding_identity: tuple[str, ...]) -> StudyTaskSpec:
    return StudyTaskSpec(
        study="integration-study",
        finding_identity=finding_identity,
        title=title,
        project="tests.quality",
        tags=("test-quality", "decision"),
        acceptance=("the generated task preserves study metadata",),
    )
