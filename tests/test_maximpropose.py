"""ACK-ledger source records for maxim proposal mining."""

from __future__ import annotations

import subprocess

from spice.agent.maximcli import render_maxim_sources, run_maxim_sources_cli
from spice.agent.maxims import (
    MaximProposalEvidence,
    maxim_proposal_source_records,
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
ARCHIVED_AT_OLDER = 100.0
ARCHIVED_AT_NEWER = 200.0


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
