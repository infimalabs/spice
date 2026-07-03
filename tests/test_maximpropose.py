"""ACK-ledger source records for maxim proposal mining."""

from __future__ import annotations

import re
import shutil
import subprocess
import tomllib

import pytest

from spice.agent.driver import DRIVER
from spice.agent import maximcli, maxims
from spice.agent.maximcli import (
    render_filed_maxim_proposal_tasks,
    render_maxim_proposals,
    render_maxim_sources,
    run_maxim_file_proposals_cli,
    run_maxim_proposals_cli,
    run_maxim_sources_cli,
)
from spice.agent.maxims import (
    MAXIM_PROPOSAL_TASK_CREATION_SURFACE,
    MaximProposalDispositionCount,
    MaximProposalEvidence,
    MaximProposalTheme,
    file_maxim_proposal_tasks,
    maxim_proposal_drafts,
    maxim_proposal_source_records,
    maxim_proposal_themes,
)
from spice.cli.parser import build_parser
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    AckStateWrite,
    record_acked_inbox_items,
)
from spice.mail.inbox import compose_inbox_text
from spice.tasks import alloc, config as task_config, identity, tw

KEY_A = "20260703T020000000000Z"
KEY_B = "20260703T020001000000Z"
KEY_C = "20260703T020002000000Z"
ARCHIVED_AT_OLDER = 100.0
ARCHIVED_AT_NEWER = 200.0
ARCHIVED_AT_NEWEST = 300.0
ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def _init_repo(path):
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path


@pytest.fixture
def maxim_task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-maxim-proposal")
    task_config.set_backend(str(backend))
    try:
        yield repo
    finally:
        task_config.set_backend(None)


def test_maxim_proposal_source_records_preserve_ack_ledger_evidence_order(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    older_text = compose_inbox_text(
        body="  Avoid fallback branches.\nUse one deterministic path. ",
        priority="urgent",
        stop=False,
    )
    newer_text = compose_inbox_text(
        body="Preserve the prompt boundary.\n\nDo not pass operator prose.",
        priority=None,
        stop=False,
    )

    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=KEY_A,
                inbox_name=f"{KEY_A}.txt",
                text=older_text,
                ack_text=f"ACK {KEY_A}: captured the fallback correction.",
                ack_content=" captured the fallback correction. ",
                disposition=ACK_DISPOSITION_ACKED,
            ),
        ],
        now=ARCHIVED_AT_OLDER,
    )
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=KEY_B,
                inbox_name=f"{KEY_B}.txt",
                text=newer_text,
                ack_text=f"NACK {KEY_B}: cannot safely import that worktree.",
                ack_content=" cannot safely import that worktree. ",
                disposition=ACK_DISPOSITION_REFUSED,
            ),
        ],
        now=ARCHIVED_AT_NEWER,
    )

    records = maxim_proposal_source_records(repo)

    assert [record.key for record in records] == [KEY_B, KEY_A]
    assert records[0].inbox_name == f"{KEY_B}.txt"
    assert (
        records[0].steering_text
        == "Preserve the prompt boundary. Do not pass operator prose."
    )
    assert records[0].ack_text == f"NACK {KEY_B}: cannot safely import that worktree."
    assert records[0].ack_content == "cannot safely import that worktree."
    assert records[0].disposition == ACK_DISPOSITION_REFUSED
    assert records[0].archived_at == ARCHIVED_AT_NEWER
    assert records[0].evidence == (
        MaximProposalEvidence(
            field="steering_text",
            text="Preserve the prompt boundary. Do not pass operator prose.",
        ),
        MaximProposalEvidence(
            field="ack_text",
            text=f"NACK {KEY_B}: cannot safely import that worktree.",
        ),
        MaximProposalEvidence(
            field="ack_content",
            text="cannot safely import that worktree.",
        ),
    )
    assert records[1].steering_text == (
        "Avoid fallback branches. Use one deterministic path."
    )
    assert records[1].disposition == ACK_DISPOSITION_ACKED


def test_render_maxim_sources_lists_source_evidence_fields(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    text = compose_inbox_text(
        body="Preserve the prompt boundary.",
        priority=None,
        stop=False,
    )
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=KEY_A,
                inbox_name=f"{KEY_A}.txt",
                text=text,
                ack_text=f"ACK {KEY_A}: captured.",
                ack_content="captured.",
            )
        ],
        now=ARCHIVED_AT_OLDER,
    )

    assert render_maxim_sources(maxim_proposal_source_records(repo)).splitlines() == [
        "maxim proposal sources: 1",
        "key disposition evidence",
        f"{KEY_A} {ACK_DISPOSITION_ACKED} steering_text,ack_text,ack_content",
    ]


def test_maxim_sources_cli_uses_ack_ledger_reader(tmp_path, monkeypatch, capsys):
    repo = _init_repo(tmp_path / "repo")
    text = compose_inbox_text(
        body="Avoid fallbacks.",
        priority=None,
        stop=False,
    )
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=KEY_A,
                inbox_name=f"{KEY_A}.txt",
                text=text,
                ack_text=f"ACK {KEY_A}: captured.",
                ack_content="captured.",
            )
        ],
        now=ARCHIVED_AT_OLDER,
    )
    monkeypatch.chdir(repo)

    args = build_parser().parse_args(["maxim", "sources"])

    assert args.func is run_maxim_sources_cli
    assert args.func(args) == 0
    assert capsys.readouterr().out.splitlines() == [
        "maxim proposal sources: 1",
        "key disposition evidence",
        f"{KEY_A} {ACK_DISPOSITION_ACKED} steering_text,ack_text,ack_content",
    ]


def test_maxim_proposal_source_records_handle_missing_and_empty_ledgers(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    assert maxim_proposal_source_records(repo) == ()

    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=KEY_A,
                inbox_name=f"{KEY_A}.txt",
                text="",
                ack_text="",
                ack_content="",
            )
        ],
        now=ARCHIVED_AT_OLDER,
    )

    assert maxim_proposal_source_records(repo) == ()


def test_maxim_proposal_themes_cluster_recurring_corrections_with_evidence(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _record_ack_source(
        repo,
        key=KEY_A,
        body="Avoid fallback branches. Use one deterministic path.",
        ack_text=f"ACK {KEY_A}: captured fallback correction.",
        ack_content="captured fallback correction.",
        disposition=ACK_DISPOSITION_ACKED,
        archived_at=ARCHIVED_AT_OLDER,
    )
    _record_ack_source(
        repo,
        key=KEY_B,
        body="Fallback branches hide the deterministic path.",
        ack_text=f"NACK {KEY_B}: refusing fallback path.",
        ack_content="refusing fallback path.",
        disposition=ACK_DISPOSITION_REFUSED,
        archived_at=ARCHIVED_AT_NEWER,
    )
    _record_ack_source(
        repo,
        key=KEY_C,
        body="Preserve the prompt boundary.",
        ack_text=f"ACK {KEY_C}: captured prompt boundary.",
        ack_content="captured prompt boundary.",
        disposition=ACK_DISPOSITION_ACKED,
        archived_at=ARCHIVED_AT_NEWEST,
    )

    themes = maxim_proposal_themes(maxim_proposal_source_records(repo))

    assert len(themes) == 1
    assert themes[0].name == "branches/deterministic/fallback/path"
    assert themes[0].recurring_terms == (
        "branches",
        "deterministic",
        "fallback",
        "path",
    )
    assert themes[0].evidence_count == 2
    assert themes[0].source_keys == (KEY_B, KEY_A)
    assert themes[0].dispositions == (
        MaximProposalDispositionCount(disposition=ACK_DISPOSITION_ACKED, count=1),
        MaximProposalDispositionCount(disposition=ACK_DISPOSITION_REFUSED, count=1),
    )
    assert {item.text for item in themes[0].evidence} >= {
        "Fallback branches hide the deterministic path.",
        "Avoid fallback branches. Use one deterministic path.",
    }
    assert (
        maxim_proposal_themes(maxim_proposal_source_records(repo), min_recurrence=3)
        == ()
    )


def test_render_maxim_proposals_prints_valid_toml_stanza_with_evidence(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _record_ack_source(
        repo,
        key=KEY_A,
        body="Avoid fallback branches. Use one deterministic path.",
        ack_text=f"ACK {KEY_A}: captured fallback correction.",
        ack_content="captured fallback correction.",
        disposition=ACK_DISPOSITION_ACKED,
        archived_at=ARCHIVED_AT_OLDER,
    )
    _record_ack_source(
        repo,
        key=KEY_B,
        body="Fallback branches hide the deterministic path.",
        ack_text=f"NACK {KEY_B}: refusing fallback path.",
        ack_content="refusing fallback path.",
        disposition=ACK_DISPOSITION_REFUSED,
        archived_at=ARCHIVED_AT_NEWER,
    )

    rendered = render_maxim_proposals(
        maxim_proposal_themes(maxim_proposal_source_records(repo)),
        existing_bags=maxims.resolved_maxim_bags(repo),
    )

    assert rendered.splitlines() == [
        "# maxim proposals: 1",
        "# theme = branches/deterministic/fallback/path",
        "# evidence_count = 2",
        "# dispositions = acked=1,refused=1",
        f"# source_keys = {KEY_B},{KEY_A}",
        "# evidence 1 steering_text: Fallback branches hide the deterministic path.",
        f"# evidence 2 ack_text: NACK {KEY_B}: refusing fallback path.",
        "# evidence 3 ack_content: refusing fallback path.",
        "# evidence 4 steering_text: Avoid fallback branches. Use one deterministic path.",
        f"# evidence 5 ack_text: ACK {KEY_A}: captured fallback correction.",
        "# evidence 6 ack_content: captured fallback correction.",
        "[tool.spice.maxims.fallbacks]",
        'words = ["branches", "deterministic", "fallback", "path"]',
        'message = "Avoid fallback branches. Use one deterministic path."',
    ]
    parsed = tomllib.loads(rendered)
    assert parsed["tool"]["spice"]["maxims"]["fallbacks"] == {
        "words": ["branches", "deterministic", "fallback", "path"],
        "message": "Avoid fallback branches. Use one deterministic path.",
    }
    (repo / "pyproject.toml").write_text(rendered, encoding="utf-8")
    assert maxims.resolved_maxim_bags(repo)["fallbacks"].words == frozenset(
        {"branches", "deterministic", "fallback", "path"}
    )


def test_maxim_proposal_drafts_drop_or_normalize_invalid_trigger_candidates(
    tmp_path,
):
    repo = _init_repo(tmp_path / "repo")
    theme = MaximProposalTheme(
        name="raw-theme",
        recurring_terms=(
            "Quiet-Route",
            "route2",
            "soft   landing",
            "spice",
            "task",
            "!!!",
        ),
        evidence_count=1,
        source_keys=(KEY_A,),
        dispositions=(
            MaximProposalDispositionCount(
                disposition=ACK_DISPOSITION_ACKED,
                count=1,
            ),
        ),
        evidence=(
            MaximProposalEvidence(
                field="steering_text",
                text="Avoid quiet routes across contexts",
            ),
        ),
    )

    drafts = maxim_proposal_drafts((theme,), existing_bags={})
    rendered = render_maxim_proposals((theme,), existing_bags={})

    assert drafts[0].bag_name == "proposal-quiet-route-soft-landing"
    assert drafts[0].words == ("quiet route", "soft landing")
    assert drafts[0].message == "Avoid quiet routes across contexts."
    parsed = tomllib.loads(rendered)
    assert parsed["tool"]["spice"]["maxims"]["proposal-quiet-route-soft-landing"] == {
        "words": ["quiet route", "soft landing"],
        "message": "Avoid quiet routes across contexts.",
    }
    (repo / "pyproject.toml").write_text(rendered, encoding="utf-8")
    assert maxims.resolved_maxim_bags(repo)[
        "proposal-quiet-route-soft-landing"
    ].words == frozenset({"quiet route", "soft landing"})


def test_maxim_proposals_cli_does_not_pre_screen_with_judge(
    tmp_path, monkeypatch, capsys
):
    repo = _init_repo(tmp_path / "repo")
    _record_ack_source(
        repo,
        key=KEY_A,
        body="Avoid fallback branches. Use one deterministic path.",
        ack_text=f"ACK {KEY_A}: captured fallback correction.",
        ack_content="captured fallback correction.",
        archived_at=ARCHIVED_AT_OLDER,
    )
    _record_ack_source(
        repo,
        key=KEY_B,
        body="Fallback branches hide the deterministic path.",
        ack_text=f"ACK {KEY_B}: captured fallback correction.",
        ack_content="captured fallback correction.",
        archived_at=ARCHIVED_AT_NEWER,
    )
    monkeypatch.chdir(repo)

    def fail_if_called(*_args, **_kwargs):
        raise AssertionError("proposal mining must not call the maxim judge")

    monkeypatch.setattr(maximcli, "evaluate_maxim", fail_if_called)
    monkeypatch.setattr(maxims, "evaluate_maxim_any_violation", fail_if_called)
    args = build_parser().parse_args(["maxim", "proposals"])

    assert args.func is run_maxim_proposals_cli
    assert args.func(args) == 0
    assert capsys.readouterr().out.splitlines() == [
        "# maxim proposals: 1",
        "# theme = branches/deterministic/fallback/path",
        "# evidence_count = 2",
        "# dispositions = acked=2",
        f"# source_keys = {KEY_B},{KEY_A}",
        "# evidence 1 steering_text: Fallback branches hide the deterministic path.",
        f"# evidence 2 ack_text: ACK {KEY_B}: captured fallback correction.",
        "# evidence 3 ack_content: captured fallback correction.",
        "# evidence 4 steering_text: Avoid fallback branches. Use one deterministic path.",
        f"# evidence 5 ack_text: ACK {KEY_A}: captured fallback correction.",
        "# evidence 6 ack_content: captured fallback correction.",
        "[tool.spice.maxims.fallbacks]",
        'words = ["branches", "deterministic", "fallback", "path"]',
        'message = "Avoid fallback branches. Use one deterministic path."',
    ]
    assert not (repo / "pyproject.toml").exists()


def test_maxim_file_proposals_cli_creates_deferred_hidden_triage_task(
    maxim_task_repo, capsys
):
    _record_ack_source(
        maxim_task_repo,
        key=KEY_A,
        body="Avoid fallback branches. Use one deterministic path.",
        ack_text=f"ACK {KEY_A}: captured fallback correction.",
        ack_content="captured fallback correction.",
        archived_at=ARCHIVED_AT_OLDER,
    )
    _record_ack_source(
        maxim_task_repo,
        key=KEY_B,
        body="Fallback branches hide the deterministic path.",
        ack_text=f"ACK {KEY_B}: captured fallback correction.",
        ack_content="captured fallback correction.",
        archived_at=ARCHIVED_AT_NEWER,
    )
    args = build_parser().parse_args(["maxim", "file-proposals"])

    assert args.func is run_maxim_file_proposals_cli
    assert args.func(args) == 0
    output = capsys.readouterr().out.splitlines()
    assert output[0] == "filed maxim proposal tasks: 1"
    handle = re.match(
        rf"(\S+) {re.escape(task_config.MAXIM_PROPOSAL_PROJECT)} fallbacks",
        output[1],
    ).group(1)
    row = identity.resolve(handle)
    normal_list = build_parser().parse_args(["task", "list"])
    hidden_list = build_parser().parse_args(
        [
            "task",
            "list",
            "--project",
            task_config.MAXIM_PROPOSAL_PROJECT,
            "--status",
            "waiting",
        ]
    )
    normal_list.backend = str(task_config.backend_root())
    hidden_list.backend = str(task_config.backend_root())

    assert row["project"] == task_config.MAXIM_PROPOSAL_PROJECT
    assert row[task_config.PROJECT_HIDDEN_UDA] == "1"
    assert row[task_config.TASK_CREATION_SURFACE_UDA] == (
        MAXIM_PROPOSAL_TASK_CREATION_SURFACE
    )
    assert row["phase"] == "todo"
    assert str(row.get("wait") or "").startswith("2099")
    assert sorted(row["tags"]) == [
        "hidden",
        "maxim_proposal",
    ]
    assert "Human triage decides whether to merge" in row["acceptance"]
    assert "[tool.spice.maxims.fallbacks]" in row["task_description"]
    assert (
        'words = ["branches", "deterministic", "fallback", "path"]'
        in row["task_description"]
    )
    assert "Fallback branches hide the deterministic path." in row["task_description"]
    assert (
        render_filed_maxim_proposal_tasks(file_maxim_proposal_tasks(()))
        == "filed maxim proposal tasks: 0"
    )
    assert alloc.visible_ready_rows(tw.current_actor()) == []

    assert normal_list.func(normal_list) == 0
    assert capsys.readouterr().out.strip() == "no tasks"
    assert hidden_list.func(hidden_list) == 0
    assert "Triage maxim proposal: fallbacks" in capsys.readouterr().out
    assert maxims.resolved_maxim_bags(maxim_task_repo) == maxims.BUILTIN_MAXIM_BAGS


def _record_ack_source(
    repo,
    *,
    key: str,
    body: str,
    ack_text: str,
    ack_content: str,
    archived_at: float,
    disposition: str = ACK_DISPOSITION_ACKED,
) -> None:
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=key,
                inbox_name=f"{key}.txt",
                text=compose_inbox_text(body=body, priority=None, stop=False),
                ack_text=ack_text,
                ack_content=ack_content,
                disposition=disposition,
            )
        ],
        now=archived_at,
    )
