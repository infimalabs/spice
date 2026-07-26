"""Message payload task-card rendering, and the helpers sibling suites share."""

import json
from dataclasses import dataclass, replace
from datetime import datetime
from pathlib import Path

import pytest

from spice.agent.lifecycle import AgentStatus
from spice.serve import lifecycle, taskboard
from spice.serve import messages as message_reader
from spice.serve.messagepresentation import (
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
        lambda repo: _status(repo_root=repo),
    )


def _status(
    *,
    process_status: str = "idle",
    started_at: str = "",
    thread_id: str = "",
    model: str = "",
    reasoning_effort: str = "",
    repo_root: Path | None = None,
) -> AgentStatus:
    """The real status type, so a message reader meets production's shape.

    ``running`` is derived from ``process_status`` here exactly as it is in
    production, and every remaining field is present because the dataclass
    requires it.
    """
    root = repo_root if repo_root is not None else Path.cwd()
    return AgentStatus(
        repo_root=root,
        state_path=root / "state.json",
        process_status=process_status,
        pid=None,
        process_group_id=None,
        thread_id=thread_id,
        driver="",
        model=model,
        reasoning_effort=reasoning_effort,
        started_at=started_at,
        ready_at="",
        startup_failure="",
        log_path=None,
        prompt_skill_path=None,
        command=(),
    )


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
    started_at: str = "",
) -> AgentStatus:
    return replace(
        _status(
            process_status="running" if thread_id else "idle",
            thread_id=thread_id,
            model=model,
            reasoning_effort=effort,
            started_at=started_at,
            repo_root=repo,
        ),
        driver=driver,
        state_path=repo / ".git" / ".spice" / "agents" / "state.json",
    )


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
        lambda _repo: _status(
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _status(
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _status(
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


def test_task_card_renders_description_before_acceptance_and_keeps_field_order(
    monkeypatch,
):
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
    # The card reads from identity and context into its success criteria, then
    # preserves the provenance and phase metadata as contiguous ordered rows:
    # the stored origin spelling verbatim, priority, status, current phase, the
    # full flow pipeline (claimstate.phases_of -> "plan, todo, review"), tags,
    # and finally the stable handle.
    ordered_rows = (
        '<div class="task-directive-property">'
        "<dt>title</dt><dd>Surface task origin</dd></div>"
        '<div class="task-directive-property">'
        "<dt>project</dt><dd>serve.taskcards</dd></div>"
        '<div class="task-directive-property">'
        "<dt>description</dt><dd>Origin and metadata reach the card.</dd></div>"
        '<div class="task-directive-property">'
        "<dt>acceptance</dt><dd>Origin renders on the card.</dd></div>"
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
        '<div class="task-directive-property">'
        "<dt>handle</dt><dd>TASKCAR-1k4Yh62d</dd></div>"
    )
    assert ordered_rows in card.display_html


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
