"""Read-only serve targets backed directly by foreign session transcripts."""

from __future__ import annotations

import hashlib
import os
import re
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.agent.driver import AgentDriver, BUILTIN_DRIVERS, driver_for_transcript
from spice.errors import SpiceError
from spice.agent.identity import canonical_thread_id
from spice.serve.livebus import LaneSignature
from spice.serve.messages import TranscriptResolution, read_assistant_messages
from spice.serve.payload.lane import lane_chrome_payload
from spice.serve.payload.wire import validate_emitter_payload
from spice.serve.worktree.target import WorktreeTarget

_THREAD_ID_RE = re.compile(
    r"([0-9a-f]{32}|[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})\.jsonl$",
    re.IGNORECASE,
)
_OBSERVER_ID_CHARS = 10
OBSERVER_PRIMARY_PRECEDENCE = ("codex", "claude")
OBSERVER_SIGNAL_PRECEDENCE = ("session-root", "config", "cli")


@dataclass(frozen=True)
class ObserverSession:
    target: WorktreeTarget
    thread_id: str
    transcript: TranscriptResolution


@dataclass(frozen=True)
class ObserverRegistry:
    sessions: tuple[ObserverSession, ...]
    errors: tuple[str, ...]

    @property
    def targets(self) -> list[WorktreeTarget]:
        return [session.target for session in self.sessions]

    def session_for_target(self, target: WorktreeTarget) -> ObserverSession:
        for session in self.sessions:
            if session.target.id == target.id:
                return session
        raise KeyError(target.id)

    def session_for_thread(self, thread_id: str) -> ObserverSession | None:
        canonical = canonical_thread_id(thread_id)
        for session in self.sessions:
            if session.thread_id == canonical:
                return session
        return None

    def transcript_for_thread(self, thread_id: str) -> TranscriptResolution | None:
        session = self.session_for_thread(thread_id)
        return session.transcript if session is not None else None

    def match(self, selector: str | None) -> WorktreeTarget | None:
        raw = str(selector or "")
        for target in self.targets:
            if raw in {target.id, target.name, str(target.repo_root)}:
                return target
        return None

    def targets_payload(self) -> dict[str, Any]:
        targets = [observer_target_payload(session) for session in self.sessions]
        payload = {
            "workTrees": targets,
            "defaultTargetId": targets[0]["id"] if targets else "",
            "taskFilterInventory": {},
            "observerErrors": list(self.errors),
        }
        return validate_emitter_payload("observer.targets_payload", payload)

    def team_snapshot_payload(self) -> dict[str, Any]:
        teams = []
        for index, session in enumerate(self.sessions, start=1):
            teams.append(
                {
                    "teamId": f"observer-{session.target.id}",
                    "revision": 1,
                    "config": {
                        "revision": 1,
                        "lifetime": "Steer",
                        "taskFilters": [],
                        "taskFilterEntries": [],
                        "effectiveTaskFilters": [],
                    },
                    "members": [
                        {
                            "agentId": f"target:{session.target.id}",
                            "agentAliases": [],
                            "renewalIntent": {},
                        }
                    ],
                    "splitBack": {"available": False, "memberCount": 1},
                    "order": index,
                }
            )
        snapshot = {
            "globalRevision": 1,
            "globalSettings": {"fastMode": False, "observerMode": True},
            "teams": teams,
        }
        payload = {"revision": 1, "changed": True, "snapshot": snapshot}
        return validate_emitter_payload("observer.team_snapshot_payload", payload)


@dataclass(frozen=True)
class ObserverProviderDetection:
    name: str
    roots: tuple[Path, ...]
    config_present: bool
    cli_path: str | None

    @property
    def signals(self) -> tuple[str, ...]:
        signals = []
        if self.roots:
            signals.append("session-root")
        if self.config_present:
            signals.append("config")
        if self.cli_path is not None:
            signals.append("cli")
        return tuple(signals)

    @property
    def rank(self) -> int:
        for rank, signal in enumerate(reversed(OBSERVER_SIGNAL_PRECEDENCE), start=1):
            if signal in self.signals:
                return rank
        return 0


@dataclass(frozen=True)
class ObserverPrimaryDetection:
    classification: str
    primary: str
    basis: str
    providers: tuple[ObserverProviderDetection, ...]
    roots: tuple[Path, ...]

    @property
    def precedence(self) -> str:
        signals = ">".join(OBSERVER_SIGNAL_PRECEDENCE)
        providers = ">".join(OBSERVER_PRIMARY_PRECEDENCE)
        return f"{signals};{providers}"

    @property
    def signal_summary(self) -> str:
        rendered = []
        for provider in self.providers:
            signals = ",".join(provider.signals) or "none"
            rendered.append(f"{provider.name}[{signals}]")
        return ";".join(rendered)


def detect_observer_primary(
    primary_override: str | None = None,
) -> ObserverPrimaryDetection:
    """Classify local agent providers and select one deterministic primary."""
    drivers = _observer_drivers_in_precedence_order()
    providers = tuple(_detect_observer_provider(driver) for driver in drivers)
    detected = tuple(provider for provider in providers if provider.signals)
    if not detected:
        raise SpiceError(
            "spice watch: no Codex or Claude installation detected from session "
            "roots, config directories, or installed CLIs; manual usage: spice "
            "watch <session-dir> [<session-dir> ...]"
        )

    override = str(primary_override or "").strip().lower()
    if override:
        selected = next(
            (provider for provider in providers if provider.name == override), None
        )
        expected = ", ".join(OBSERVER_PRIMARY_PRECEDENCE)
        if selected is None:
            raise SpiceError(
                f"spice watch --primary: unknown provider {override!r}; "
                f"expected one of: {expected}"
            )
        if not selected.signals:
            raise SpiceError(
                f"spice watch --primary {override}: no {override} session root, "
                "config directory, or installed CLI was detected"
            )
        basis = "override"
    else:
        selected = max(detected, key=lambda provider: provider.rank)
        basis = selected.signals[0]

    if not selected.roots:
        signals = ",".join(selected.signals)
        raise SpiceError(
            "create a session or pass an explicit session directory; spice "
            f"watch: primary={selected.name} was detected via {signals} but "
            "has no existing session root to watch"
        )

    roots: list[Path] = []
    seen: set[Path] = set()
    ordered_providers = (selected,) + tuple(
        provider for provider in providers if provider.name != selected.name
    )
    for provider in ordered_providers:
        for root in provider.roots:
            if root in seen:
                continue
            seen.add(root)
            roots.append(root)

    classification = "both" if len(detected) > 1 else f"{selected.name.title()}-primary"
    return ObserverPrimaryDetection(
        classification=classification,
        primary=selected.name,
        basis=basis,
        providers=providers,
        roots=tuple(roots),
    )


def _observer_drivers_in_precedence_order() -> tuple[AgentDriver, ...]:
    by_name = {driver.name: driver for driver in BUILTIN_DRIVERS}
    if len(BUILTIN_DRIVERS) != len(OBSERVER_PRIMARY_PRECEDENCE) or set(by_name) != set(
        OBSERVER_PRIMARY_PRECEDENCE
    ):
        actual = ", ".join(sorted(by_name))
        expected = ", ".join(OBSERVER_PRIMARY_PRECEDENCE)
        raise SpiceError(
            "observer primary precedence does not match built-in drivers: "
            f"expected {expected}; found {actual}"
        )
    return tuple(by_name[name] for name in OBSERVER_PRIMARY_PRECEDENCE)


def _detect_observer_provider(driver: AgentDriver) -> ObserverProviderDetection:
    roots = tuple(
        path
        for candidate in driver.observer_roots()
        if (path := candidate.expanduser().resolve(strict=False)).is_dir()
    )
    config_dir = driver.home().expanduser().resolve(strict=False)
    return ObserverProviderDetection(
        name=driver.name,
        roots=roots,
        config_present=config_dir.is_dir(),
        cli_path=shutil.which(driver.default_bin),
    )


def discover_observer_sessions(paths: list[Path]) -> ObserverRegistry:
    sessions: list[ObserverSession] = []
    errors: list[str] = []
    seen: set[Path] = set()
    for supplied in paths:
        path = supplied.expanduser().resolve(strict=False)
        if path.is_file():
            candidates = [path]
        elif path.is_dir():
            candidates = _directory_jsonl_files(path, errors)
        else:
            errors.append(f"path does not exist: {path}")
            continue
        for candidate in candidates:
            resolved = candidate.resolve(strict=False)
            if resolved in seen:
                continue
            seen.add(resolved)
            session = _observer_session(resolved, errors)
            if session is not None:
                sessions.append(session)
    sessions.sort(key=lambda session: (session.target.name.lower(), session.thread_id))
    if not sessions and not errors:
        errors.append("no recognizable Codex or Claude transcripts found")
    return ObserverRegistry(tuple(sessions), tuple(errors))


def observer_messages_payload(
    state: Any,
    target: WorktreeTarget,
    *,
    limit: int,
    after: str | None = None,
    before: str | None = None,
    append_only: bool = False,
    client_id: str | None = None,
    **_kwargs: Any,
) -> dict[str, Any]:
    session = state.observer.session_for_target(target)
    cursor = (
        state.rollout_cursor(client_id, session.thread_id)
        if client_id and not before
        else None
    )
    try:
        items = read_assistant_messages(
            session.transcript.path,
            limit=limit,
            after=after,
            before=before,
            append_only=append_only,
            cursor=cursor,
            worktree_id=target.id,
            driver=session.transcript.owner_driver,
        )
        error = ""
    except OSError as exc:
        items = []
        error = f"could not read observed transcript: {exc}"
    payload = _observer_lane_payload(session)
    payload.update(
        {
            "messages": [item.to_payload() for item in items],
            "ackContexts": [],
            "error": error,
        }
    )
    if cursor is not None and cursor.removed_keys:
        payload["removedMessageKeys"] = list(cursor.removed_keys)
    return validate_emitter_payload("observer.observer_messages_payload", payload)


def observer_target_payload(session: ObserverSession) -> dict[str, Any]:
    payload = _observer_lane_payload(session)
    return {
        "id": session.target.id,
        "repoRoot": str(session.target.repo_root),
        "displayName": session.target.display_name,
        "branch": session.target.branch,
        **payload,
        "pendingCount": 0,
        "privateTaskCount": 0,
        "agentProcessStatus": "observer",
    }


def observer_agent_status_payload(session: ObserverSession) -> dict[str, Any]:
    payload = {
        "ok": True,
        "provider": session.transcript.owner_driver.name,
        "workTreeId": session.target.id,
        "status": "observer",
        "pid": 0,
        "processGroupId": 0,
        "threadId": session.thread_id,
        "model": "observer",
        "effort": "observer",
        "serviceTier": "",
        "launchable": False,
        "bindingStatus": "bound",
        "bindingError": "",
    }
    return validate_emitter_payload("observer.observer_agent_status_payload", payload)


def observer_lane_signature(session: ObserverSession) -> LaneSignature:
    try:
        stat = session.transcript.path.stat()
        signature: Any = (stat.st_size, stat.st_mtime_ns)
    except OSError as exc:
        signature = ("error", str(exc))
    return LaneSignature(transcript=signature, inbox=None, other=None)


def _observer_lane_payload(session: ObserverSession) -> dict[str, Any]:
    driver_name = session.transcript.owner_driver.name
    team_id = f"observer-{session.target.id}"
    target_identity = {
        "branch": session.target.branch,
        "agent": {"state": "configured", "name": session.target.name},
        "driver": {"name": driver_name, "model": "observer", "effort": "observer"},
        "thread": {"state": "bound", "threadId": session.thread_id},
    }
    serve_identity = {
        "driver": {
            "desired": driver_name,
            "actual": driver_name,
            "transcriptOwner": driver_name,
        },
        "launch": {
            "desired": {"model": "observer", "effort": "observer"},
            "actual": {"model": "observer", "effort": "observer"},
        },
        "thread": {"state": "bound", "threadId": session.thread_id},
    }
    # A read-only lane observes each chrome fact exactly once and never again:
    # no inbox to fill, no team to reconfigure, no board to claim from. Naming
    # the facts here and projecting them through the shared assembler keeps the
    # observer answering in the same contract as a live lane, at a standing
    # order that nothing later supersedes.
    pending_identity = {
        "pendingInboxCount": 0,
        "pendingInboxLabel": "0",
        "pendingInboxKeys": [],
        "pendingInboxRevision": "observer",
        "pendingInboxVersion": 1,
    }
    team_identity = {
        "state": "member",
        "teamId": team_id,
        "teamRevision": 1,
        "configRevision": 1,
    }
    team_facts = {
        "taskFilters": [],
        "taskFilterEntries": [],
        "effectiveTaskFilters": [],
        "lifetime": "Steer",
        "renewalIntent": {},
    }
    return {
        **pending_identity,
        "targetIdentity": target_identity,
        "serveAgentIdentity": serve_identity,
        **team_facts,
        "laneFilterVersion": "",
        "teamIdentity": team_identity,
        "taskFilterInventory": {},
        "laneInfo": {"summaryRows": [], "members": []},
        "statusLine": {
            "agentVisualStatus": "idle",
            "preview": f"read-only {driver_name} transcript",
            **pending_identity,
        },
        "agentEnsure": {},
        "chrome": lane_chrome_payload(
            target_id=session.target.id,
            team_identity=team_identity,
            team_facts=team_facts,
            renewal_intent=team_facts["renewalIntent"],
            task_filter_inventory={},
            pending_identity=pending_identity,
        ),
    }


def _directory_jsonl_files(root: Path, errors: list[str]) -> list[Path]:
    files: list[Path] = []

    def onerror(exc: OSError) -> None:
        errors.append(f"could not scan {exc.filename or root}: {exc.strerror or exc}")

    for directory, _names, filenames in os.walk(root, onerror=onerror):
        base = Path(directory)
        files.extend(base / name for name in filenames if name.endswith(".jsonl"))
    return files


def _observer_session(path: Path, errors: list[str]) -> ObserverSession | None:
    match = _THREAD_ID_RE.search(path.name)
    owner = driver_for_transcript(path)
    if match is None or not owner.owns_transcript(path):
        return None
    try:
        mode = path.stat().st_mode
        if mode & 0o444 == 0:
            raise PermissionError("file has no read permission bits")
        with path.open("rb") as handle:
            handle.read(1)
    except OSError as exc:
        errors.append(f"could not read {path}: {exc}")
        return None
    thread_id = canonical_thread_id(match.group(1))
    digest = hashlib.sha1(str(path).encode("utf-8")).hexdigest()[:_OBSERVER_ID_CHARS]
    target = WorktreeTarget(
        id=f"session-{digest}",
        repo_root=path.parent,
        name=f"{owner.name} {thread_id[:8]}",
        branch="observer",
    )
    return ObserverSession(
        target=target,
        thread_id=thread_id,
        transcript=TranscriptResolution(thread_id, path, owner),
    )
