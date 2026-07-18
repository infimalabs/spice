"""Session briefing inbox-key parsing tests."""

from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    AckStateWrite,
    record_acked_inbox_items,
)
from spice.mail.inbox import compose_inbox_text, write_inbox_item
from spice.sessions.briefing import render_briefing, render_sweep
from tests.test_sessionbriefing import _init_git_repo, _section_lines


def test_briefing_accepts_collision_suffixed_ask_keys(tmp_path, monkeypatch):
    repo = _init_git_repo(tmp_path / "repo")
    pending_key = "1jN54zJK-2"
    ack_key = "1jN54zJK-3"
    write_inbox_item(
        repo,
        f"{pending_key}.txt",
        compose_inbox_text(body="pending collision", priority=None, stop=False),
    )
    record_acked_inbox_items(
        repo,
        [
            AckStateWrite(
                key=ack_key,
                inbox_name=f"{ack_key}.txt",
                text=compose_inbox_text(
                    body="acked collision", priority=None, stop=False
                ),
                disposition=ACK_DISPOSITION_ACKED,
            )
        ],
        now=0.0,
    )
    monkeypatch.chdir(repo)

    briefing = render_briefing(
        [],
        end="2026-01-01T00:10:00.000Z",
        max_lines=200,
        max_bytes=20000,
    )
    sweep = render_sweep(
        [],
        count=1,
        start="2026-01-01T00:00:00.000Z",
        end="2026-01-01T00:10:00.000Z",
    )

    assert _section_lines(briefing, "Steering") == [
        "Steering",
        "  2026-01-01T00:00:00.001Z disposition=pending "
        "key=1jN54zJK-2 text=pending collision",
        "  2026-01-01T00:00:00.001Z disposition=acked "
        "key=1jN54zJK-3 text=acked collision",
    ]
    assert _section_lines(sweep, "Window 0 (from 2026-01-01T00:00:00.000Z)") == [
        "Window 0 (from 2026-01-01T00:00:00.000Z)",
        "  ask pending 2026-01-01T00:00:00.001Z key=1jN54zJK-2 pending collision",
        "  ask acked 2026-01-01T00:00:00.001Z key=1jN54zJK-3 acked collision",
    ]
