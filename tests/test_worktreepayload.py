"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace


from spice.serve.messages import AssistantMessage
from spice.serve import messages as message_reader
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.team.store import ServeTeamStore

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

    monkeypatch.setattr(inventory, "task_filter_inventory", lambda: {})
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
    monkeypatch.setattr(inventory, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        inventory, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        inventory, "ensure_agent_for_available_work", lambda *_a, **_k: None
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


def test_work_trees_payload_exports_review_rows_once_per_build(tmp_path, monkeypatch):
    # review_pressure filters two GLOBAL taskwarrior exports -- completed reviews
    # and open follow-ups -- that carry no per-target argument, so their result is
    # identical for every lane. The inventory build loads one shared snapshot and
    # threads it through every lane, so the build spawns the export pair exactly
    # once regardless of target count while each lane still filters that shared
    # data down to its own actor.
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
            "review_author": "agent-a",
            "review_finding": "changes",
            "review_by": "peer-a",
            "review_at": "2026-06-10T00:00:00Z",
        },
        {
            "uuid": "review-b",
            "review_author": "agent-b",
            "review_finding": "blocked",
            "review_by": "peer-b",
            "review_at": "2026-06-11T00:00:00Z",
        },
    ]
    export_calls: list[list[str]] = []

    def counting_export(filters: list[str] | None = None, **_kwargs: object):
        recorded = list(filters or [])
        export_calls.append(recorded)
        if recorded == ["status:completed"]:
            return [dict(row) for row in completed]
        return []

    monkeypatch.setattr(lane.tw, "export", counting_export)
    monkeypatch.setattr(inventory, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        inventory, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        inventory, "ensure_agent_for_pending_inbox", lambda *_a, **_k: None
    )
    monkeypatch.setattr(
        inventory, "ensure_agent_for_available_work", lambda *_a, **_k: None
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

    # Two targets, two lanes -- but the review-pressure export pair runs exactly
    # once each across the whole build (claimed-task resolution issues its own
    # per-lane +ACTIVE exports, which this pins away from).
    assert export_calls.count(["status:completed"]) == 1
    assert export_calls.count(["(", "status:pending", "or", "status:waiting", ")"]) == 1
    pressures = [tree["laneInfo"]["reviewPressure"] for tree in payload["workTrees"]]
    # The shared snapshot is still filtered per lane: each actor sees only its own
    # completed review, so the two lanes carry distinct pressure.
    assert pressures[0]["count"] == 1
    assert pressures[1]["count"] == 1
    assert pressures[0]["items"][0]["reviewedTask"] == "review-a"
    assert pressures[1]["items"][0]["reviewedTask"] == "review-b"
    assert pressures[0] != pressures[1]


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

    def resolve_claimed_task(candidate: str) -> dict[str, str]:
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
