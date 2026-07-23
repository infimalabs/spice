"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace


from spice.agent.driver import CLAUDE_DRIVER
from spice.agent.renewal import RENEWAL_HANDOFF_REQUEST_SUFFIX
from spice.mail.ackarchive import archive_ackd_inbox_items
from spice.mail.attachments import prepare_inbox_attachments
from spice.mail.inbox import compose_inbox_text, inbox_item_key, write_inbox_item
from spice.paths import shared_attachment_root
from spice.serve.agentapi import sent_steering_payload
from spice.serve.messages import AssistantMessage
from spice.serve import messages as message_reader
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.steering import submit_steering_message
from spice.serve.team.store import ServeTeamStore
from spice.tasks import config as task_config

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="

FIVE_MINUTES_SECONDS = 300


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
        message,
        "_ensure_work_tree_agent",
        lambda _state, _target, resolved_thread: (
            resolved_thread,
            "",
            False,
            None,
        ),
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
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})


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
        self.pending_agent_ensure_attempts: dict[str, float] = {}

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


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


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
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        if filters and f"origin_thread.is:{actor}" in filters:
            seen["filters"] = filters
            return [row]
        return []

    monkeypatch.setattr(message.tw, "export", fake_export)
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})
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
        inventory,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        inventory,
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

    assert seen["filters"] == [
        "status.any:",
        f"origin_thread.is:{actor}",
    ]
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

    monkeypatch.setattr(message.tw, "export", lambda _filters: [row])

    cards = message._task_card_messages_for_thread(actor, after=None, before=None)

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
    ]
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        seen["filters"] = filters
        return rows

    monkeypatch.setattr(message.tw, "export", fake_export)

    cards = message._task_card_messages_for_thread(actor, after=None, before=None)

    assert seen["filters"] == ["status.any:", f"origin_thread.is:{actor}"]
    assert [card.display_text for card in cards] == [
        f"Task capture: Oops task card ({task_config.OOPS_PROJECT})",
        f"Task capture: Private task card ({task_config.private_project(actor)})",
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


def test_messages_payload_after_cursor_preserves_transcript_delta(
    tmp_path, monkeypatch
):
    actor = "a" * 32
    row = {
        "id": 43,
        "uuid": "newer-task",
        "incepted": "1k4Yh6Ps",
        "description": "Later CLI follow-up",
        "project": "serve.ui",
        "origin_thread": actor,
        "creation_surface": "cli",
        "status": "pending",
    }
    boundary_key = "2026-06-10T12:00:01.001000Z#task-card:older-task"
    seen: dict[str, object] = {}

    def fake_messages(_thread_id: str, **kwargs) -> message_reader.AssistantMessageRead:
        seen["reader_kwargs"] = kwargs
        return _message_read([_message("2026-06-10T12:00:03.000000Z")])

    monkeypatch.setattr(message.tw, "export", lambda _filters: [row])
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})
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
        inventory,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        inventory,
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
        fake_messages,
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
        after=boundary_key,
    )

    assert seen["reader_kwargs"]["after"] == boundary_key
    assert [item["display_text"] for item in payload["messages"]] == [
        "hello",
        "Task capture: Later CLI follow-up (serve.ui)",
    ]
    assert payload["messages"][0]["timestamp"] == "2026-06-10T12:00:03.000000Z"
    assert payload["messages"][1]["source_kind"] == "cli_task_created"


def test_cli_review_followup_row_renders_standalone_task_card(monkeypatch):
    actor = "a" * 32
    row = {
        "id": 43,
        "uuid": "review-followup-43",
        "incepted": "1k4Yh6n5",
        "description": "CLI review follow-up",
        "project": "serve.ui",
        "acceptance": "Review follow-up appears as a card",
        "origin_thread": actor,
        "creation_surface": "cli",
        "depends": ["reviewed-task-uuid"],
        "status": "pending",
    }
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        seen["filters"] = filters
        return [row]

    monkeypatch.setattr(message.tw, "export", fake_export)

    cards = message._task_card_messages_for_thread(actor, after=None, before=None)

    assert seen["filters"] == [
        "status.any:",
        f"origin_thread.is:{actor}",
    ]
    assert len(cards) == 1
    card = cards[0]
    assert card.kind == "task_card"
    assert card.source_kind == "cli_task_created"
    assert card.display_text == "Task capture: CLI review follow-up (serve.ui)"
    assert '<blockquote class="task-directive-quote">' in card.display_html
    assert "<dt>title</dt><dd>CLI review follow-up</dd>" in card.display_html
    assert (
        "<dt>acceptance</dt><dd>Review follow-up appears as a card</dd>"
        in card.display_html
    )


def test_task_card_cursor_keeps_append_window_to_transcript_items(monkeypatch):
    actor = "a" * 32
    rows = [
        {
            "id": 1,
            "uuid": "older-task",
            "incepted": "1k4Yh62d",
            "description": "Older CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
        {
            "id": 2,
            "uuid": "newer-task",
            "incepted": "1k4Yh6Ps",
            "description": "Later CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
    ]
    boundary_key = "2026-06-10T12:00:01.001000Z#task-card:older-task"

    monkeypatch.setattr(message.tw, "export", lambda _filters: rows)

    merged = message._merge_task_card_messages(
        actor,
        [_message("2026-06-10T12:00:03.000000Z")],
        limit=5,
        after=boundary_key,
    )

    assert [item.display_text for item in merged] == [
        "hello",
        "Task capture: Later CLI follow-up (serve.ui)",
    ]
    boundary = message_reader.parse_timestamp("2026-06-10T12:00:01.001000Z")
    assert boundary is not None
    assert all(
        (timestamp := message_reader.parse_timestamp(item.timestamp)) is not None
        and timestamp > boundary
        for item in merged
    )


def test_task_card_tail_merge_drops_cards_older_than_visible_window(monkeypatch):
    actor = "a" * 32
    rows = [
        {
            "id": 1,
            "uuid": "stale-task",
            "incepted": "1k4VkTVn",
            "description": "Stale CLI follow-up",
            "project": "serve.docs",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
        {
            "id": 2,
            "uuid": "fresh-task",
            "incepted": "1k4Yh62d",
            "description": "Fresh CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
    ]
    monkeypatch.setattr(message.tw, "export", lambda _filters: rows)

    merged = message._merge_task_card_messages(
        actor,
        [_message("2026-06-10T12:00:00.000000Z")],
        limit=5,
    )

    assert [item.display_text for item in merged] == [
        "Task capture: Fresh CLI follow-up (serve.ui)",
        "hello",
    ]
    assert all("Stale CLI follow-up" not in item.display_text for item in merged)


def test_messages_payload_reports_transcript_owner_in_serve_identity(
    tmp_path, monkeypatch
):
    thread_id = "agent-a"
    transcript = message_reader.TranscriptResolution(
        thread_id=thread_id,
        path=tmp_path / "claude.jsonl",
        owner_driver=CLAUDE_DRIVER,
    )
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "desired-model", "effort": "high"},
    )
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})
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
        inventory,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        inventory,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message,
        "resolve_thread_id_for_target",
        lambda _state, _target: thread_id,
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(message, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(transcript=transcript),
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["serveAgentIdentity"]["driver"]["transcriptOwner"] == "claude"
    assert payload["serveAgentIdentity"]["driver"]["actual"] == "claude"
    assert payload["serveAgentIdentity"]["driver"]["desired"] == "codex"
    assert payload["laneInfo"]["summaryRows"][:7] == [
        {"key": "agent", "value": "-", "span": False},
        {"key": "driver actual", "value": "claude", "span": False},
        {"key": "driver desired", "value": "codex", "span": False},
        {"key": "model actual", "value": "actual-model", "span": False},
        {"key": "model desired", "value": "desired-model", "span": False},
        {"key": "effort actual", "value": "low", "span": False},
        {"key": "effort desired", "value": "high", "span": False},
    ]
    assert {"key": "session", "value": "claude", "span": False} in payload["laneInfo"][
        "summaryRows"
    ]


def test_messages_payload_reports_agent_renewal_intent(monkeypatch, tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    _record_identity(store, "thread:agent-a", thread_id="agent-a")
    store.set_agent_renewal_request("thread:agent-a", requested=True)
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})
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
        inventory,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        inventory,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message,
        "resolve_thread_id_for_target",
        lambda _state, _target: "agent-a",
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(),
    )

    payload = message.messages_payload_for_worktree(
        _State(team_store=store),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["renewalIntent"]["agentId"] == "thread:agent-a"
    assert payload["renewalIntent"]["requested"] is True
    assert payload["renewalIntent"]["state"] == "requested"
    assert payload["renewalIntent"]["teamSlot"] == 0
    assert payload["renewalIntent"]["predecessorIdentity"]["threadId"] == "agent-a"
    assert payload["renewalIntent"]["successorIdentity"]["desiredModel"] == (
        "desired-model"
    )


def test_sent_steering_payload_includes_image_attachments(tmp_path):
    _init_repo(tmp_path)
    sent = submit_steering_message(
        text="inspect this",
        priority=None,
        stop=False,
        attachments=[
            {
                "name": "paste.png",
                "contentType": "image/png",
                "dataUrl": IMAGE_DATA_URL,
            }
        ],
        target_repo_root=tmp_path,
    )

    payload = sent_steering_payload(sent, target=_Target(id="wt", repo_root=tmp_path))

    assert payload["attachments"][0]["name"] == "paste.png"
    assert payload["attachments"][0]["contentType"] == "image/png"
    attachment_path = Path(payload["attachments"][0]["path"])
    assert attachment_path.is_absolute()
    assert shared_attachment_root(tmp_path) in attachment_path.parents
    assert payload["attachments"][0]["url"].startswith(
        "/api/work/trees/wt/files/image?path="
    )


def test_messages_payload_round_trips_ack_context_attachments(monkeypatch, tmp_path):
    _init_repo(tmp_path)
    name = "1jNmXPHm.txt"
    key = inbox_item_key(name)
    composed = compose_inbox_text(
        body=f"look here\n{RENEWAL_HANDOFF_REQUEST_SUFFIX}",
        priority=None,
        stop=False,
    )
    attachments = prepare_inbox_attachments(
        [
            {
                "name": "upload.png",
                "contentType": "image/png",
                "dataUrl": IMAGE_DATA_URL,
            }
        ]
    )
    write_inbox_item(tmp_path, name, composed, attachments=attachments)
    _stub_messages_payload(
        monkeypatch,
        [_message("2026-01-04T00:00:01.000000Z", ack_count=1, ack_keys=[key])],
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    context = payload["ackContexts"][0]
    attachment = context["attachments"][0]
    assert context["key"] == key
    assert context["found"] is True
    assert context["text"] == "look here"
    assert context["html"] == "<p>look here</p>"
    assert attachment["name"] == "upload.png"
    assert attachment["contentType"] == "image/png"
    attachment_path = Path(attachment["path"])
    assert attachment_path.is_absolute()
    assert shared_attachment_root(tmp_path) in attachment_path.parents
    assert attachment["url"].startswith("/api/work/trees/wt/files/image?path=")


def test_messages_payload_reports_inbox_status_without_streaming_requests(
    monkeypatch, tmp_path
):
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    pending_name = "1jNmXPHp.txt"
    archived_name = "1jNmXPHq.txt"
    write_inbox_item(
        repo,
        pending_name,
        compose_inbox_text(body="pending request", priority="urgent", stop=False),
    )
    write_inbox_item(
        repo,
        archived_name,
        compose_inbox_text(body="archived request", priority=None, stop=False),
    )
    archive_ackd_inbox_items(repo, [inbox_item_key(archived_name)])
    monkeypatch.setattr(
        message, "resolve_thread_id_for_target", lambda _state, _target: ""
    )
    monkeypatch.setattr(
        inventory,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        inventory,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(running=False, started_at=""),
    )
    monkeypatch.setattr(message, "task_filter_inventory", lambda: {})

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=repo),
        limit=5,
    )
    assert set(payload) == {
        "messages",
        "ackContexts",
        "targetWorktreeName",
        "targetBranch",
        "targetIdentity",
        "serveAgentIdentity",
        "taskFilters",
        "taskFilterEntries",
        "effectiveTaskFilters",
        "laneFilterVersion",
        "teamIdentity",
        "lifetime",
        "renewalIntent",
        "taskFilterInventory",
        "laneInfo",
        "agentProcessStatus",
        "error",
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "pendingInboxVersion",
        "agentEnsure",
        "statusLine",
    }
    assert payload["messages"] == []
    assert payload["targetIdentity"]["thread"] == {"state": "unbound"}
    assert payload["targetIdentity"]["agent"] == {"state": "unconfigured"}
    assert payload["serveAgentIdentity"]["actorId"] == "target:wt"
    assert payload["serveAgentIdentity"]["renewal"]["revision"] == 0
    assert payload["teamIdentity"] == {"state": "none"}
    assert payload["pendingInboxCount"] == 1
    assert payload["pendingInboxLabel"] == "1"
    assert payload["pendingInboxKeys"] == [inbox_item_key(pending_name)]
    assert payload["pendingInboxRevision"]
    assert payload["pendingInboxVersion"] > 0
    assert payload["statusLine"]["pendingInboxCount"] == 1
    assert payload["statusLine"]["pendingInboxLabel"] == "1"
    assert payload["statusLine"]["pendingInboxKeys"] == [inbox_item_key(pending_name)]
    assert (
        payload["statusLine"]["pendingInboxRevision"] == payload["pendingInboxRevision"]
    )
    assert (
        payload["statusLine"]["pendingInboxVersion"] == payload["pendingInboxVersion"]
    )


def test_messages_payload_finds_ack_context_by_collision_suffixed_key(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    name = "1jNmXPHn-2.txt"
    suffixed_key = "1jNmXPHn-2"
    composed = compose_inbox_text(body="operator original", priority=None, stop=False)
    write_inbox_item(tmp_path, name, composed)
    archive_ackd_inbox_items(tmp_path, [suffixed_key])
    _stub_messages_payload(
        monkeypatch,
        [
            _message(
                "2026-01-04T00:00:01.000000Z",
                ack_count=1,
                ack_keys=[suffixed_key],
            )
        ],
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["ackContexts"][0]["key"] == suffixed_key
    assert payload["ackContexts"][0]["found"] is True
    assert payload["ackContexts"][0]["text"] == "operator original"


def test_messages_payload_does_not_quote_assistant_ack_when_inbox_missing(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    key = "1jNmXPHn"
    _stub_messages_payload(
        monkeypatch,
        [_message("2026-01-04T00:00:01.000000Z", ack_count=1, ack_keys=[key])],
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["ackContexts"] == [{"key": key, "found": False}]
