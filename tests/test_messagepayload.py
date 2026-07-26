"""Message payload task-card rendering tests."""

import json
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.mail.ackarchive import summarize_nack_archival
from spice.mail.ackgrammar import extract_task_batch_lines_from_text
from spice.serve import lifecycle, taskboard
from spice.serve import messages as message_reader
from spice.serve.messagepresentation import (
    SEGMENT_DISPOSITION_WITHHELD,
    AssistantMessage,
)
from spice.serve.payload import identity, lane, message
from spice.serve.team.store import ServeTeamStore
from spice.serve.worktree import inventory
from spice.tasks import config as task_config

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="

FIVE_MINUTES_SECONDS = 300

# A board revision is the generation its authority minted, so this fixture
# carries a count rather than a label: the chrome producer publishes an epoch
# only where it could have counted forward from it.
FIXTURE_GENERATION = "1785044000000001"


class _EmptyOpenTaskBoard:
    task_filter_inventory: dict[str, object] = {}

    def active_claim(self, actor: str):
        return None

    def task_card_rows(self, actor: str):
        return ()

    def completed_review_rows(self, actors):
        return ()

    def open_review_followup_count(self, reviewed_uuid: str):
        return 0

    def drained_task_count(self, actor: str):
        return 0


def _task_board(rows):
    return taskboard.open_task_board_projection(
        taskboard.TaskBoardObservation(
            backend_identity="test",
            revision=FIXTURE_GENERATION,
            rows=tuple(rows),
        )
    )


@pytest.fixture(autouse=True)
def _stub_open_task_board(monkeypatch):
    monkeypatch.setattr(
        message,
        "open_task_board_projection",
        lambda: _EmptyOpenTaskBoard(),
    )


def _record_identity(
    store: ServeTeamStore,
    actor_id: str,
    *,
    target_id: str = "wt",
    thread_id: str = "",
) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id=target_id,
        thread_id=thread_id or actor_id.removeprefix("thread:"),
        actual_driver="codex",
        actual_model="actual-model",
        actual_effort="low",
        actual_service_tier="fast",
        desired_driver="codex",
        desired_model="desired-model",
        desired_effort="high",
        transcript_owner="codex",
    )


def _message(
    timestamp: str,
    *,
    kind: str = "assistant",
    ack_count: int = 0,
    ack_keys: list[str] | None = None,
    preview: str = "",
):
    return AssistantMessage(
        key=f"{timestamp}#0",
        index=0,
        timestamp=timestamp,
        text="hello",
        display_text="hello",
        display_html="<p>hello</p>",
        ack_count=ack_count,
        ack_keys=ack_keys or [],
        ack_utterances=[],
        kind=kind,
        preview=preview,
    )


def _message_read(
    items: list[AssistantMessage] | None = None,
    *,
    error: str | None = None,
    transcript: message_reader.TranscriptResolution | None = None,
) -> message_reader.AssistantMessageRead:
    return message_reader.AssistantMessageRead(
        items=items or [],
        error=error,
        transcript=transcript,
    )


def _stub_messages_payload(
    monkeypatch,
    items: list[AssistantMessage],
    *,
    thread_id: str = "thread-a",
) -> None:
    monkeypatch.setattr(
        message, "resolve_thread_id_for_target", lambda _state, _target: thread_id
    )
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(items),
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(running=False, started_at=""),
    )


@dataclass(frozen=True)
class _Status:
    running: bool
    started_at: str
    process_status: str = "idle"
    thread_id: str = ""
    model: str = ""
    reasoning_effort: str = ""
    service_tier: str = ""
    state_path: Path | None = None


@dataclass(frozen=True)
class _Target:
    id: str
    repo_root: Path | None = None
    name: str = "repo"
    display_name: str = "repo"
    branch: str = "main"


class _State:
    def __init__(
        self, sends: int = 0, team_store: ServeTeamStore | None = None
    ) -> None:
        self._sends = sends
        self.team_store = team_store or ServeTeamStore()

    def lane_send_count(self, target_id: str) -> int:
        return self._sends

    def rollout_cursor(self, thread_id: str):
        return None


class _InventoryState(_State):
    def __init__(self, target: _Target) -> None:
        super().__init__()
        self._target = target

    def worktree_targets(self) -> list[_Target]:
        return [self._target]

    def targets_discovery_errors(self) -> list[str]:
        return []


def _stamp(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _write_response_item(
    path: Path, timestamp: str, payload: dict[str, object]
) -> None:
    path.write_text(
        json.dumps(
            {"timestamp": timestamp, "type": "response_item", "payload": payload},
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )


def _pending_identity(count: int = 0) -> dict[str, object]:
    return {
        "pendingInboxCount": count,
        "pendingInboxLabel": str(count),
        "pendingInboxKeys": [],
        "pendingInboxRevision": f"test-revision-{count}",
        "pendingInboxVersion": 100 + count,
    }


def _identity_status(
    repo: Path,
    *,
    driver: str = "codex",
    thread_id: str = "",
    model: str = "",
    effort: str = "",
    service_tier: str = "",
    started_at: str = "",
) -> SimpleNamespace:
    return SimpleNamespace(
        running=bool(thread_id),
        process_status="running" if thread_id else "idle",
        thread_id=thread_id,
        model=model,
        reasoning_effort=effort,
        service_tier=service_tier,
        started_at=started_at,
        driver=driver,
        state_path=repo / ".git" / ".spice" / "agents" / "state.json",
    )


def test_inline_task_directive_renders_quote_like_block_in_message(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "Queued the follow-up.\n"
                        "TASK title=Inline follow-up | project=task.unit | "
                        "acceptance=Tracked from UI | "
                        "acceptance=Rendered on another row\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.kind == "assistant"
    assert item.task_card_count == 1
    assert item.to_payload()["task_card_count"] == 1
    assert item.display_text == (
        "Queued the follow-up.\nTask capture: Inline follow-up (task.unit)\nContinuing."
    )
    assert "TASK title" not in item.display_text
    assert "TASK title" not in item.display_html
    assert '<blockquote class="task-directive-quote">' in item.display_html
    assert '<div class="task-directive-kicker">Task capture</div>' in item.display_html
    assert "<dt>title</dt><dd>Inline follow-up</dd>" in item.display_html
    assert "<dt>project</dt><dd>task.unit</dd>" in item.display_html
    assert (
        "<dt>acceptance</dt><dd>Tracked from UI</dd></div>"
        '<div class="task-directive-property">'
        "<dt>acceptance</dt><dd>Rendered on another row</dd>" in item.display_html
    )


def test_inline_task_directive_without_acceptance_renders_card(tmp_path):
    # UI-1kD6hDJ6 regression: the supervisor converts a directive that carries
    # only title+project (acceptance omitted -> plan phase), so the UI must
    # render it as a capture card exactly like an acceptance-bearing one --
    # matching the attached example, which stayed raw with an ACK badge.
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "Captured as a separate planning task.\n"
                        "TASK title=Enable and verify the default AFM judge for Spice "
                        "| project=hooks.maxims\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.task_card_count == 1
    assert item.to_payload()["task_card_count"] == 1
    assert item.display_text == (
        "Captured as a separate planning task.\n"
        "Task capture: Enable and verify the default AFM judge for Spice "
        "(hooks.maxims)\n"
        "Continuing."
    )
    assert '<blockquote class="task-directive-quote">' in item.display_html
    assert '<div class="task-directive-kicker">Task capture</div>' in item.display_html
    assert (
        "<dt>title</dt><dd>Enable and verify the default AFM judge for Spice</dd>"
        in item.display_html
    )
    assert "<dt>project</dt><dd>hooks.maxims</dd>" in item.display_html


def test_malformed_task_like_progress_update_remains_plain_message(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = (
        "TASK badges now use the plum task accent with the count after the label. "
        "I am validating the focused tests next."
    )
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    payload = item.to_payload()
    expected_html = f"<p>{text}</p>"
    assert item.task_card_count == 0
    assert payload["task_card_count"] == item.task_card_count
    assert item.display_text == text
    assert item.display_html == expected_html
    assert payload["display_text"] == text
    assert payload["display_html"] == expected_html
    assert payload["preview"] == text
    assert payload["text"] == text


def test_ordinary_assistant_message_display_replaces_only_terminal_colon(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "Status: ready:"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]

    assert item.text == text
    assert item.display_text == "Status: ready."
    assert item.display_html == "<p>Status: ready.</p>"
    assert item.preview == "Status: ready."


def test_ack_display_replaces_terminal_colon_and_keeps_spoken_text(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "ACK 1k4YggTX: follow-up: queued:"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]

    assert item.text == text
    assert item.display_text == "Follow-up: queued."
    assert item.ack_segments[0]["html"] == "<p>Follow-up: queued.</p>"
    assert item.ack_utterances == ["follow-up: queued:"]
    assert item.preview == "Follow-up: queued."


def test_assistant_message_display_preserves_nonterminal_colon(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "Status: ready."
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]

    assert item.display_text == text
    assert item.display_html == f"<p>{text}</p>"
    assert item.preview == text


def test_inline_task_directive_renders_inside_ack_segment_at_written_position(
    tmp_path,
):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "ACK 1k4YggTX: captured.\n"
                        "TASK: title=ACK follow-up | project=serve.ui | "
                        "acceptance=Inline block appears\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    segment_html = item.ack_segments[0]["html"]

    assert item.ack_count == 1
    assert item.task_card_count == 1
    assert item.ack_utterances == ["captured.\nContinuing."]
    assert item.display_text == (
        "Captured.\nTask capture: ACK follow-up (serve.ui)\nContinuing."
    )
    assert "TASK:" not in segment_html
    assert segment_html.index("<p>Captured.</p>") < segment_html.index(
        '<blockquote class="task-directive-quote">'
    )
    assert segment_html.index('<blockquote class="task-directive-quote">') < (
        segment_html.index("<p>Continuing.</p>")
    )
    assert "<dt>title</dt><dd>ACK follow-up</dd>" in segment_html
    assert "<dt>project</dt><dd>serve.ui</dd>" in segment_html


def test_assistant_message_payload_splits_ack_and_nack_polarity(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "ACK 1k4YggTX: shipped the doctor rollup.\n"
                        "NACK 1k4Yggrm: refusing — that weakens "
                        "the gate."
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    payload = item.to_payload()

    # ack_keys is the polarity-agnostic union of responded keys, in source order;
    # the counts and nack_keys carry the positive/negative split for tinting.
    assert payload["ack_keys"] == [
        "1k4YggTX",
        "1k4Yggrm",
    ]
    assert payload["ack_count"] == 1
    assert payload["nack_count"] == 1
    assert payload["nack_keys"] == ["1k4Yggrm"]
    assert [seg["disposition"] for seg in payload["ack_segments"]] == [
        "acked",
        "refused",
    ]


def test_assistant_message_payload_marks_a_pure_nack_without_ack_count(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": ("NACK 1k4YggTX: cannot comply with that."),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    payload = item.to_payload()

    # A NACK-led message must not spill its refusal into the preamble, and it
    # must not read as an acknowledgment (no acked tint / ACK chip).
    assert payload["ack_count"] == 0
    assert payload["nack_count"] == 1
    assert payload["nack_keys"] == ["1k4YggTX"]
    assert payload["preamble_html"] == ""
    assert [seg["disposition"] for seg in payload["ack_segments"]] == ["refused"]
    assert "Cannot comply with that." in payload["ack_segments"][0]["html"]


def test_assistant_message_payload_does_not_honor_a_reasonless_nack(tmp_path):
    """A refusal that said nothing renders nothing, not an empty segment.

    A withheld refusal keeps whatever body it carried, so this is the case that
    holds the other edge: with no body there is nothing to keep, and the segment
    stays absent rather than reaching the browser as a blank one to draw.
    """
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = "NACK 1k4YggTX:"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    summary = summarize_nack_archival(None, text)
    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert summary.refused == []
    assert summary.reasonless == ["1k4YggTX"]
    assert payload["ack_count"] == 0
    assert payload["ack_keys"] == []
    assert payload["nack_count"] == 0
    assert payload["nack_keys"] == []
    assert [segment["disposition"] for segment in payload["ack_segments"]] == []


_ACK_KEY = "1k4YggTX"
_NACK_KEY = "1k4Yggrm"
_TASK_DIRECTIVE = (
    "TASK title=Follow up | project=session.transcript | acceptance=Tracked"
)
_APP_DIRECTIVE = '::git-commit{"sha":"abc"}'


def test_a_nack_whose_only_body_is_a_directive_is_not_honored(tmp_path):
    """A refusal that captured a task but gave no reason still needs one.

    Archival strips control lines before it looks for a reason, so this body
    reads as empty to the supervisor and the item stays pending. The display
    keeps the directive whole so it can still become a card, and sharing only
    the predicate was not enough to make the two agree: the same refusal read
    as reasonless to the supervisor and refused on the wire, closing the
    operator's submission while the supervisor was still asking for a reason.

    Withholding the refusal must not withhold the record of what it captured.
    The keys stay pending and the polarity stays unclaimed, so the segment names
    no keys and takes neither ACK-state disposition; the body it carried still
    renders, because the task in it did reach the board. All four hold at once,
    which is the whole contract: an operator sees the capture without the wire
    ever saying this message answered anything.
    """
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = f"NACK {_NACK_KEY}:\n{_TASK_DIRECTIVE}"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    summary = summarize_nack_archival(None, text)
    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert summary.reasonless == [_NACK_KEY]
    assert payload["ack_keys"] == []
    assert payload["nack_keys"] == []
    assert payload["nack_count"] == 0
    assert payload["ack_count"] == 0
    assert [segment["disposition"] for segment in payload["ack_segments"]] == [
        SEGMENT_DISPOSITION_WITHHELD
    ]
    assert [segment["keys"] for segment in payload["ack_segments"]] == [[]]
    assert payload["task_card_count"] == 1
    assert "Follow up" in payload["ack_segments"][0]["html"]
    assert extract_task_batch_lines_from_text(text) == [_TASK_DIRECTIVE]


_SUPPRESSED_DIRECTIVE_SHAPES = {
    "fenced": "Use this form:\n```\n{directive}\n```\nNothing was captured.",
    "indented": "Use this form:\n\n    {directive}\n\nNothing was captured.",
    "quoted": "They wrote:\n> {directive}\nNothing was captured.",
    "source-context": "spice/serve/taskdirectives.py:88: {directive}",
}


@pytest.mark.parametrize("shape", sorted(_SUPPRESSED_DIRECTIVE_SHAPES))
def test_a_directive_the_supervisor_suppresses_renders_no_card(shape, tmp_path):
    """Serve calls a line a directive exactly when the supervisor would.

    Serve recognized directives with its own line walk, so a TASK line shown
    inside a fence or an indented block — prose documenting the form rather
    than asking for a task — rendered a card for a capture that would never
    happen, and rewrote the line to a summary that destroyed the example it
    was quoting. The quoted and source-context shapes already agreed by
    accident, because their prefix survives into the strip; comparing every
    shape against the supervisor's own reading makes the agreement principled
    instead of coincidental.
    """
    text = _SUPPRESSED_DIRECTIVE_SHAPES[shape].format(directive=_TASK_DIRECTIVE)
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert extract_task_batch_lines_from_text(text) == []
    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert _TASK_DIRECTIVE in payload["display_text"]


_OPENING_DIRECTIVE_SEGMENTS = {
    "preamble": "    {directive}\n\nNothing was captured.",
    "ack-body": f"ACK {_ACK_KEY}:\n" + "    {directive}\n\nNothing was captured.",
    "nack-body": f"NACK {_NACK_KEY}:\n" + "    {directive}\n\nNo capture happened.",
}


@pytest.mark.parametrize("position", sorted(_OPENING_DIRECTIVE_SEGMENTS))
def test_a_directive_opening_its_segment_indented_renders_no_card(position, tmp_path):
    """Indentation is read where the segment starts, not only after it.

    Every reader that trimmed a segment stripped it whole, which took the
    indentation off its first content line and left a bare directive for the
    next reader to believe. The shape only reached production from the opening
    position, because a directive further in kept a preceding line to anchor
    its indentation, so that is the position under test here: the same example
    is written first in a preamble, in an ACK body, and in a NACK body.
    """
    text = _OPENING_DIRECTIVE_SEGMENTS[position].format(directive=_TASK_DIRECTIVE)
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert extract_task_batch_lines_from_text(text) == []
    assert payload["task_card_count"] == len(extract_task_batch_lines_from_text(text))
    assert _TASK_DIRECTIVE in payload["display_text"]


def test_an_ack_body_on_the_header_line_still_drops_the_separator_space(tmp_path):
    """Keeping indentation must not mean keeping the marker's own padding.

    The space after `ACK <key>:` belongs to the marker, so the body that
    continues that line still opens flush. Reading it any other way would make
    every ordinary ACK present with a leading space, which is what stripping
    the segment whole used to hide.
    """
    text = f"ACK {_ACK_KEY}: Acknowledged the ask\nand did the work."
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert payload["ack_keys"] == [_ACK_KEY]
    assert payload["display_text"] == "Acknowledged the ask\nand did the work."


def test_a_fenced_app_directive_survives_into_displayed_prose(tmp_path):
    """A shown app directive is prose, and deleting it empties its own fence.

    App-directive lines are dropped from prose rather than rewritten, so
    filtering one that suppression had already excused left the fence around
    it standing with nothing inside.
    """
    text = f"The commit line looks like:\n```\n{_APP_DIRECTIVE}\n```\nThat is all."
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC)),
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    payload = message_reader.read_assistant_messages(transcript, limit=5)[
        0
    ].to_payload()

    assert _APP_DIRECTIVE in payload["display_text"]
    assert payload["display_text"].endswith("That is all.")


@pytest.mark.parametrize(
    ("text", "expected", "expected_dispositions"),
    [
        pytest.param(
            f"NACK {_NACK_KEY}:\n{_TASK_DIRECTIVE}\ncannot comply with that.",
            {
                "ack_count": 0,
                "ack_keys": [_NACK_KEY],
                "nack_count": 1,
                "nack_keys": [_NACK_KEY],
                "task_card_count": 1,
            },
            ["refused"],
            id="nack-opens-with-task-directive",
        ),
        pytest.param(
            f"NACK {_NACK_KEY}:\n{_APP_DIRECTIVE}\ncannot comply with that.",
            {
                "ack_count": 0,
                "ack_keys": [_NACK_KEY],
                "nack_count": 1,
                "nack_keys": [_NACK_KEY],
                "task_card_count": 0,
            },
            ["refused"],
            id="nack-opens-with-app-directive",
        ),
        pytest.param(
            f"ACK {_ACK_KEY}:\n{_TASK_DIRECTIVE}\nshipped the doctor rollup.",
            {
                "ack_count": 1,
                "ack_keys": [_ACK_KEY],
                "nack_count": 0,
                "nack_keys": [],
                "task_card_count": 1,
            },
            ["acked"],
            id="ack-opens-with-task-directive",
        ),
    ],
)
def test_keyed_response_opening_with_a_directive_keeps_its_polarity(
    tmp_path,
    text: str,
    expected: dict[str, object],
    expected_dispositions: list[str],
):
    """A control line belongs to the run it sits in and cannot retint it.

    Here the directive is the first thing the run classifies. While the run's
    polarity had to be inferred for a line that carried none, a refusal that
    captured a task first was published as an acknowledgment: one extra acked
    segment, and a key acked and refused at once vanished from `nack_keys`.
    Each shape stays one segment, and the task card still counts.
    """
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    payload = item.to_payload()

    assert {field: payload[field] for field in expected} == expected
    assert [
        seg["disposition"] for seg in payload["ack_segments"]
    ] == expected_dispositions


def test_inline_task_directive_counts_multiple_task_cards(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "TASK title=First follow-up | project=serve.ui | "
                        "acceptance=First card\n"
                        "TASK title=Second follow-up | project=task.unit | "
                        "acceptance=Second card"
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]

    assert item.task_card_count == 2
    assert item.to_payload()["task_card_count"] == 2
    assert '<div class="task-directive-stack">' in item.display_html
    assert item.display_html.count('class="task-directive-quote"') == 2


def test_cli_created_task_row_renders_standalone_task_card(tmp_path, monkeypatch):
    actor = "a" * 32
    row = {
        "id": 42,
        "uuid": "task-uuid-42",
        "incepted": "1k4Yh62d",
        "description": "CLI follow-up",
        "project": "serve.ui",
        "acceptance": ("Task card comes from the backend | Second backend criterion"),
        "origin_thread": actor,
        "creation_surface": "cli",
        "status": "pending",
    }
    projection = _task_board([row])
    monkeypatch.setattr(message, "open_task_board_projection", lambda: projection)
    monkeypatch.setattr(
        message,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        inventory,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message, "resolve_thread_id_for_target", lambda _state, _target: actor
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(message, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(),
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    item = payload["messages"][0]
    assert item["kind"] == "task_card"
    assert item["source_kind"] == "cli_task_created"
    assert item["task_card_count"] == 1
    assert item["timestamp"] == "2026-06-10T12:00:01.001000Z"
    assert item["display_text"] == "Task capture: CLI follow-up (serve.ui)"
    assert item["preview"] == "Task capture: CLI follow-up (serve.ui)"
    assert '<blockquote class="task-directive-quote">' in item["display_html"]
    assert (
        '<div class="task-directive-kicker">Task capture</div>' in item["display_html"]
    )
    assert "<dt>title</dt><dd>CLI follow-up</dd>" in item["display_html"]
    assert "<dt>project</dt><dd>serve.ui</dd>" in item["display_html"]
    assert "<dt>status</dt><dd>pending</dd>" in item["display_html"]
    assert (
        "<dt>acceptance</dt><dd>Task card comes from the backend</dd></div>"
        '<div class="task-directive-property">'
        "<dt>acceptance</dt><dd>Second backend criterion</dd>" in item["display_html"]
    )
    assert "<dt>handle</dt><dd>UI-1k4Yh62d</dd>" in item["display_html"]


def test_task_card_renders_origin_priority_flow_and_tags_in_order(monkeypatch):
    actor = "a" * 32
    row = {
        "id": 77,
        "uuid": "task-uuid-77",
        "incepted": "1k4Yh62d",
        "description": "Surface task origin",
        "task_description": "Origin and metadata reach the card.",
        "project": "serve.taskcards",
        "origin": "ack:1kF7MMCS",
        "priority": "M",
        "status": "pending",
        "phase": "todo",
        "phase_0": "plan",
        "phase_1": "todo",
        "phase_2": "review",
        "phase_i": 1,
        "tags": ["cards", "origin"],
        "acceptance": "Origin renders on the card.",
        "origin_thread": actor,
    }

    cards = message._task_card_messages_for_thread(
        actor,
        after=None,
        before=None,
        task_board=_task_board([row]),
    )

    card = cards[0]
    # The card fronts title/project/acceptance, then surfaces the provenance and
    # phase metadata as its own contiguous, ordered <dd> rows: the stored origin
    # spelling verbatim, priority, status, current phase, the full flow pipeline
    # (claimstate.phases_of -> "plan, todo, review"), and the joined tags.
    metadata_rows = (
        '<div class="task-directive-property">'
        "<dt>origin</dt><dd>ack:1kF7MMCS</dd></div>"
        '<div class="task-directive-property">'
        "<dt>priority</dt><dd>M</dd></div>"
        '<div class="task-directive-property">'
        "<dt>status</dt><dd>pending</dd></div>"
        '<div class="task-directive-property">'
        "<dt>phase</dt><dd>todo</dd></div>"
        '<div class="task-directive-property">'
        "<dt>flow</dt><dd>plan, todo, review</dd></div>"
        '<div class="task-directive-property">'
        "<dt>tags</dt><dd>cards, origin</dd></div>"
    )
    assert metadata_rows in card.display_html
    assert "<dt>title</dt><dd>Surface task origin</dd>" in card.display_html
    assert "<dt>handle</dt><dd>TASKCAR-1k4Yh62d</dd>" in card.display_html


def test_shared_task_card_index_is_lazy_for_unbound_lane_and_reuses_rows(
    tmp_path, monkeypatch
):
    rows = [
        {
            "uuid": "agent-a-card",
            "incepted": "1k4Yh62d",
            "description": "Agent A card",
            "project": "serve.ui",
            "origin_thread": "agenta",
        },
        {
            "uuid": "agent-b-card",
            "incepted": "1k4Yh6Ps",
            "description": "Agent B card",
            "project": "serve.ui",
            "origin_thread": "agentb",
        },
    ]
    projection = _task_board(rows)
    monkeypatch.setattr(
        taskboard.tw,
        "export",
        lambda *_args, **_kwargs: pytest.fail("shared row queries must not export"),
    )
    assert message.target_activity_items(
        _Target(id="wt", repo_root=tmp_path),
        "",
        task_board=projection,
    ) == ([], None, None)

    first = message._task_card_messages_for_thread(
        "agent-a",
        after=None,
        before=None,
        task_board=projection,
    )
    second = message._task_card_messages_for_thread(
        "agent-b",
        after=None,
        before=None,
        task_board=projection,
    )

    assert [item.display_text for item in first] == [
        "Task capture: Agent A card (serve.ui)"
    ]
    assert [item.display_text for item in second] == [
        "Task capture: Agent B card (serve.ui)"
    ]
    assert projection.task_card_rows("agent-a") is projection.task_card_rows("agent-a")


def test_agent_created_hidden_oops_and_private_rows_render_full_task_cards(
    monkeypatch,
):
    actor = "a" * 32
    rows = [
        {
            "id": 42,
            "uuid": "oops-task-42",
            "incepted": "1k4Yh62d",
            "description": "Oops task card",
            "task_description": "Full oops diagnostic stays visible.",
            "project": task_config.OOPS_PROJECT,
            "status": "waiting",
            "phase": "plan",
            "origin_thread": actor,
        },
        {
            "id": 43,
            "uuid": "private-task-43",
            "incepted": "1k4Yh6Ps",
            "description": "Private task card",
            "task_description": "Private details stay visible.",
            "project": task_config.private_project(actor),
            "status": "pending",
            "phase": "todo",
            "origin_thread": actor,
            "acceptance": "Private acceptance renders.",
        },
        {
            "id": 44,
            "uuid": "completed-task-44",
            "entry": "20260610T120003Z",
            "description": "Completed task card",
            "project": "serve.cards",
            "status": "completed",
            "phase": "review",
            "origin_thread": actor,
        },
        {
            "id": 45,
            "uuid": "different-origin-45",
            "incepted": "1k4Yh7AC",
            "description": "Different origin",
            "project": "serve.cards",
            "status": "pending",
            "origin_thread": f"thread:{actor}",
        },
    ]
    projection = _task_board(rows)
    cards = message._task_card_messages_for_thread(
        actor,
        after=None,
        before=None,
        task_board=projection,
    )
    expected = [
        card
        for row in rows[:3]
        if (card := message._task_card_message_from_row(row)) is not None
    ]

    assert [card.to_payload() for card in cards] == [
        card.to_payload() for card in expected
    ]
    assert [card.display_text for card in cards] == [
        f"Task capture: Oops task card ({task_config.OOPS_PROJECT})",
        f"Task capture: Private task card ({task_config.private_project(actor)})",
        "Task capture: Completed task card (serve.cards)",
    ]
    oops_card = cards[0]
    private_card = cards[1]
    assert oops_card.source_kind == "task_created"
    assert (
        'class="task-directive-quote task-directive-quote--oops '
        'task-directive-quote--hidden"'
    ) in oops_card.display_html
    assert '<div class="task-directive-kicker">Oops task</div>' in (
        oops_card.display_html
    )
    assert "<dt>description</dt><dd>Full oops diagnostic stays visible.</dd>" in (
        oops_card.display_html
    )
    assert "<dt>status</dt><dd>waiting</dd>" in oops_card.display_html
    assert "<dt>phase</dt><dd>plan</dd>" in oops_card.display_html
    assert private_card.source_kind == "task_created"
    assert 'class="task-directive-quote task-directive-quote--private"' in (
        private_card.display_html
    )
    assert '<div class="task-directive-kicker">Private task</div>' in (
        private_card.display_html
    )
    assert "<dt>acceptance</dt><dd>Private acceptance renders.</dd>" in (
        private_card.display_html
    )
