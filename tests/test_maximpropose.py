"""ACK-ledger source records for maxim proposal mining."""

from __future__ import annotations

import subprocess

from spice.agent import maximcli, maxims
from spice.agent.maximcli import (
    render_maxim_proposals,
    render_maxim_sources,
    run_maxim_proposals_cli,
    run_maxim_sources_cli,
)
from spice.agent.maxims import (
    MaximProposalDispositionCount,
    MaximProposalEvidence,
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

KEY_A = "20260703T020000000000Z"
KEY_B = "20260703T020001000000Z"
KEY_C = "20260703T020002000000Z"
ARCHIVED_AT_OLDER = 100.0
ARCHIVED_AT_NEWER = 200.0
ARCHIVED_AT_NEWEST = 300.0


def _init_repo(path):
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path


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


def test_render_maxim_proposals_lists_counts_dispositions_and_keys(tmp_path):
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

    assert render_maxim_proposals(
        maxim_proposal_themes(maxim_proposal_source_records(repo))
    ).splitlines() == [
        "maxim proposals: 1",
        "theme evidence dispositions source_keys terms",
        (
            "branches/deterministic/fallback/path 2 acked=1,refused=1 "
            f"{KEY_B},{KEY_A} branches,deterministic,fallback,path"
        ),
    ]


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
        "maxim proposals: 1",
        "theme evidence dispositions source_keys terms",
        (
            "branches/deterministic/fallback/path 2 acked=2 "
            f"{KEY_B},{KEY_A} branches,deterministic,fallback,path"
        ),
    ]


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
