"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace

import pytest

from spice.serve.messages import AssistantMessage
from spice.serve import messages as message_reader
from spice.serve import lifecycle, taskboard
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.team.store import ServeTeamStore

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="

FIVE_MINUTES_SECONDS = 300


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


@pytest.fixture(autouse=True)
def _stub_open_task_board(monkeypatch):
    monkeypatch.setattr(
        inventory,
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
        ack_keys=[],
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


def test_work_trees_payload_includes_latest_activity_for_global_menu(
    tmp_path, monkeypatch
):
    latest = _stamp(datetime(2026, 6, 10, 12, 1, tzinfo=UTC))
    target = _Target(id="wt", repo_root=tmp_path)
    calls: list[dict[str, object]] = []

    def fake_assistant_messages_for_thread_id(
        thread_id: str, **kwargs: object
    ) -> message_reader.AssistantMessageRead:
        calls.append({"thread_id": thread_id, **kwargs})
        return _message_read(
            [_message(latest, kind="presence:reasoning", preview="thinking")]
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
        inventory,
        "resolve_thread_id_for_target",
        lambda _state, _target: "agent-a",
    )
    monkeypatch.setattr(
        inventory,
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
    monkeypatch.setattr(inventory, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(identity, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        fake_assistant_messages_for_thread_id,
    )

    payload = inventory.work_trees_payload(_InventoryState(target))

    work_tree = payload["workTrees"][0]
    assert work_tree["lastAssistantAt"] == latest
    assert work_tree["serveAgentIdentity"]["actorId"] == "thread:agent-a"
    assert work_tree["statusLine"]["lastAssistantAt"] == latest
    assert work_tree["statusLine"]["preview"] == "thinking"
    assert calls == [
        {
            "thread_id": "agent-a",
            "limit": 1,
            "worktree_id": "wt",
            "repo_root": tmp_path,
        }
    ]


class _MultiInventoryState(_State):
    def __init__(self, targets: list[_Target]) -> None:
        super().__init__()
        self._targets = targets

    def worktree_targets(self) -> list[_Target]:
        return list(self._targets)

    def targets_discovery_errors(self) -> list[str]:
        return []


def test_work_trees_payload_resolves_agent_config_once_per_target(
    tmp_path, monkeypatch
):
    # The identity, driver, and lane-info builders each consume this target's
    # effective_agent_config and say-voice name. The inventory pass resolves both
    # once and threads them down, so a payload build resolves each exactly once
    # per target: two targets produce exactly two config resolutions and two
    # voice resolutions, in target order.
    targets = [
        _Target(id="wt-a", repo_root=tmp_path / "a"),
        _Target(id="wt-b", repo_root=tmp_path / "b"),
    ]
    config_calls: list[Path] = []
    voice_calls: list[Path] = []

    def counting_config(repo_root: Path) -> dict[str, str]:
        config_calls.append(repo_root)
        return {"driver": "codex", "model": "desired-model", "effort": "high"}

    def counting_voice(repo_root: Path) -> str:
        voice_calls.append(repo_root)
        return "Ava (Premium)"

    idle = _Status(running=False, started_at="", process_status="idle", thread_id="")
    monkeypatch.setattr(inventory, "effective_agent_config", counting_config)
    monkeypatch.setattr(identity, "effective_agent_config", counting_config)
    monkeypatch.setattr(identity, "configured_say_voice", counting_voice)
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_available_work", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        inventory, "resolve_thread_id_for_target", lambda _state, _target: ""
    )
    monkeypatch.setattr(inventory, "agent_status", lambda _repo: idle)
    monkeypatch.setattr(identity, "agent_status", lambda _repo: idle)
    monkeypatch.setattr(inventory, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message, "target_activity_items", lambda *_a, **_k: ([], None, None)
    )

    payload = inventory.work_trees_payload(_MultiInventoryState(targets))

    assert len(payload["workTrees"]) == 2
    assert config_calls == [targets[0].repo_root, targets[1].repo_root]
    assert voice_calls == [targets[0].repo_root, targets[1].repo_root]


def test_work_trees_payload_indexes_shared_review_rows_per_lane(tmp_path, monkeypatch):
    targets = [
        _Target(id="wt-a", repo_root=tmp_path / "a"),
        _Target(id="wt-b", repo_root=tmp_path / "b"),
    ]
    threads = {targets[0].repo_root: "agent-a", targets[1].repo_root: "agent-b"}

    def running_status(repo_root: Path) -> _Status:
        return _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=threads[repo_root],
        )

    completed = [
        {
            "uuid": "review-a",
            "status": "completed",
            "claim_by": "agent-a",
            "review_author": "agent-a",
            "review_finding": "changes",
            "review_by": "peer-a",
            "review_at": "2026-06-10T00:00:00Z",
        },
        {
            "uuid": "review-b",
            "status": "completed",
            "claim_by": "agent-b",
            "review_author": "agent-b",
            "review_finding": "blocked",
            "review_by": "peer-b",
            "review_at": "2026-06-11T00:00:00Z",
        },
    ]
    task_board = taskboard.open_task_board_projection(
        taskboard.TaskBoardObservation(
            backend_identity="test",
            revision="reviews",
            rows=tuple(completed),
        )
    )
    monkeypatch.setattr(inventory, "open_task_board_projection", lambda: task_board)
    monkeypatch.setattr(
        taskboard.tw,
        "export",
        lambda *_args, **_kwargs: pytest.fail("shared row queries must not export"),
    )
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_available_work", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        inventory,
        "resolve_thread_id_for_target",
        lambda _state, target: threads[target.repo_root],
    )
    monkeypatch.setattr(inventory, "agent_status", running_status)
    monkeypatch.setattr(identity, "agent_status", running_status)
    monkeypatch.setattr(inventory, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(identity, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(
        message, "target_activity_items", lambda *_a, **_k: ([], None, None)
    )

    payload = inventory.work_trees_payload(_MultiInventoryState(targets))

    pressures = [tree["laneInfo"]["reviewPressure"] for tree in payload["workTrees"]]
    # The shared projection is still filtered per lane: each actor sees only its own
    # completed review, so the two lanes carry distinct pressure.
    assert pressures[0]["count"] == 1
    assert pressures[1]["count"] == 1
    assert pressures[0]["items"][0]["reviewedTask"] == "review-a"
    assert pressures[1]["items"][0]["reviewedTask"] == "review-b"
    assert pressures[0]["items"][0]["findingSeverity"] == "changes"
    assert pressures[1]["items"][0]["findingSeverity"] == "blocked"


def test_work_trees_payload_projects_active_claims_without_an_export(
    tmp_path, monkeypatch
):
    targets = [
        _Target(id="wt-a", repo_root=tmp_path / "a"),
        _Target(id="wt-b", repo_root=tmp_path / "b"),
    ]
    threads = {targets[0].repo_root: "agent-a", targets[1].repo_root: "agent-b"}

    def running_status(repo_root: Path) -> _Status:
        return _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=threads[repo_root],
        )

    # render_handle returns the row uuid when `incepted` is absent, so each
    # lane's claimedTask is a predictable per-actor value. claim_by holds the
    # canonicalised actor (agent-a -> agenta).
    active = [
        {
            "uuid": "claim-a",
            "claim_by": "agenta",
            "claim_at": "2026-06-10T00:00:00Z",
            "phase": "todo",
            "description": "task a",
            "start": "20260610T000000Z",
        },
        {
            "uuid": "claim-b",
            "claim_by": "agentb",
            "claim_at": "2026-06-11T00:00:00Z",
            "phase": "review",
            "description": "task b",
            "start": "20260611T000000Z",
        },
    ]
    observation = taskboard.TaskBoardObservation(
        backend_identity="test",
        revision="active",
        rows=tuple(active),
    )
    task_board = taskboard.open_task_board_projection(observation)
    monkeypatch.setattr(
        taskboard.tw,
        "export",
        lambda *_args, **_kwargs: pytest.fail("shared row queries must not export"),
    )
    monkeypatch.setattr(inventory, "open_task_board_projection", lambda: task_board)
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_available_work", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        inventory,
        "resolve_thread_id_for_target",
        lambda _state, target: threads[target.repo_root],
    )
    monkeypatch.setattr(inventory, "agent_status", running_status)
    monkeypatch.setattr(identity, "agent_status", running_status)
    monkeypatch.setattr(inventory, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(identity, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(
        message, "target_activity_items", lambda *_a, **_k: ([], None, None)
    )

    payload = inventory.work_trees_payload(_MultiInventoryState(targets))

    claimed = [tree["statusLine"]["claimedTask"] for tree in payload["workTrees"]]
    # The shared snapshot is still filtered per lane: each actor resolves only
    # its own claim.
    assert claimed[0] == {"handle": "claim-a", "phase": "todo", "title": "task a"}
    assert claimed[1] == {"handle": "claim-b", "phase": "review", "title": "task b"}


def test_work_trees_payload_indexes_shared_task_cards_for_each_lane(
    tmp_path, monkeypatch
):
    targets = [
        _Target(id="wt-a", repo_root=tmp_path / "a"),
        _Target(id="wt-b", repo_root=tmp_path / "b"),
    ]
    threads = {targets[0].repo_root: "agent-a", targets[1].repo_root: "agent-b"}

    def running_status(repo_root: Path) -> _Status:
        return _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=threads[repo_root],
        )

    task_rows = [
        {
            "id": 1,
            "uuid": "task-a",
            "entry": "20260610T120001Z",
            "description": "Lane A follow-up",
            "project": "serve.alpha",
            "origin_thread": "agenta",
            "creation_surface": "cli",
            "status": "pending",
        },
        {
            "id": 2,
            "uuid": "task-b",
            "entry": "20260610T120002Z",
            "description": "Lane B follow-up",
            "project": "serve.beta",
            "origin_thread": "agentb",
            "creation_surface": "cli",
            "status": "pending",
        },
    ]
    task_board = taskboard.open_task_board_projection(
        taskboard.TaskBoardObservation(
            backend_identity="test",
            revision="task-cards",
            rows=tuple(task_rows),
        )
    )
    monkeypatch.setattr(inventory, "open_task_board_projection", lambda: task_board)
    monkeypatch.setattr(
        taskboard.tw,
        "export",
        lambda *_args, **_kwargs: pytest.fail("shared row queries must not export"),
    )
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        lifecycle, "ensure_agent_for_available_work", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        inventory,
        "resolve_thread_id_for_target",
        lambda _state, target: threads[target.repo_root],
    )
    monkeypatch.setattr(inventory, "agent_status", running_status)
    monkeypatch.setattr(identity, "agent_status", running_status)
    monkeypatch.setattr(inventory, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(identity, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_a, **_k: _message_read(),
    )

    payload = inventory.work_trees_payload(_MultiInventoryState(targets))

    task_activity = [
        {
            "kind": tree["statusLine"]["latestActivityKind"],
            "timestamp": tree["statusLine"]["lastAssistantAt"],
            "preview": tree["statusLine"]["preview"],
        }
        for tree in payload["workTrees"]
    ]
    assert task_activity == [
        {
            "kind": "task_card",
            "timestamp": "2026-06-10T12:00:01.000000Z",
            "preview": "Task capture: Lane A follow-up (serve.alpha)",
        },
        {
            "kind": "task_card",
            "timestamp": "2026-06-10T12:00:02.000000Z",
            "preview": "Task capture: Lane B follow-up (serve.beta)",
        },
    ]


def test_inventory_and_lane_status_share_claimed_task_resolution(tmp_path, monkeypatch):
    thread_id = "019f6eddab8c7ab2870af6b81dfc5b7f"
    target = _Target(id="wt", repo_root=tmp_path)
    status = _Status(
        running=True,
        started_at="",
        process_status="running",
        thread_id=thread_id,
    )
    active_task = {
        "handle": "UI-1kF5xdSM",
        "phase": "todo",
        "title": "Keep task context through target refresh",
    }
    resolved_task = active_task
    resolver_calls: list[str] = []

    def resolve_claimed_task(
        candidate: str, *, claims: object | None = None
    ) -> dict[str, str]:
        resolver_calls.append(candidate)
        return resolved_task

    monkeypatch.setattr(lane, "_claimed_task_payload", resolve_claimed_task)
    monkeypatch.setattr(
        inventory,
        "serve_agent_identity_payload",
        lambda *_args, **_kwargs: {},
    )
    monkeypatch.setattr(
        message,
        "target_activity_items",
        lambda *_args, **_kwargs: ([], None, None),
    )
    monkeypatch.setattr(lane, "agent_status", lambda _repo: status)
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )

    _, inventory_active = inventory._work_tree_status_payloads(
        _State(),
        target,
        thread_id=thread_id,
        binding_status="bound",
        binding_error="",
        status=status,
        pending_identity=_pending_identity(),
    )
    subscription_active = lane.status_line_payload(
        _State(), target, items=[], error=None
    )

    assert inventory_active["claimedTask"] == active_task
    assert subscription_active["claimedTask"] == active_task
    assert resolver_calls == [thread_id, thread_id]

    resolved_task = {}
    _, inventory_released = inventory._work_tree_status_payloads(
        _State(),
        target,
        thread_id=thread_id,
        binding_status="bound",
        binding_error="",
        status=status,
        pending_identity=_pending_identity(),
    )
    subscription_released = lane.status_line_payload(
        _State(), target, items=[], error=None
    )

    assert inventory_released["claimedTask"] == {}
    assert subscription_released["claimedTask"] == {}
    assert resolver_calls == [thread_id, thread_id, thread_id, thread_id]


def test_pending_inbox_identity_version_is_positive_without_inbox_activity(tmp_path):
    from spice.serve.pending import pending_inbox_identity_payload

    payload = pending_inbox_identity_payload(tmp_path)

    assert payload["pendingInboxCount"] == 0
    assert payload["pendingInboxKeys"] == []
    # The UI rejects any identity payload without a positive version; a
    # worktree that has never seen inbox activity must still be importable.
    assert payload["pendingInboxVersion"] >= 1
