"""Shared serve team storage models."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from spice.serve.teamschema import (
    DEFAULT_LIFETIME,
    DEFAULT_SELECTED_VIEW,
    DEFAULT_SPEECH_MODE,
    RENEWAL_STATE_REQUESTED,
    TASK_FILTER_SOURCE_MANUAL,
)


@dataclass(frozen=True)
class TeamTaskFilter:
    project: str
    source: str = TASK_FILTER_SOURCE_MANUAL

    def to_payload(self) -> dict[str, str]:
        return {"project": self.project, "source": self.source}


@dataclass(frozen=True)
class TeamConfig:
    lifetime: str = DEFAULT_LIFETIME
    speech_mode: str = DEFAULT_SPEECH_MODE
    task_filters: tuple[str, ...] = ()
    task_filter_entries: tuple[TeamTaskFilter, ...] = ()
    selected_view: str = DEFAULT_SELECTED_VIEW
    shell_settings: dict[str, Any] = field(default_factory=dict)

    def to_payload(self, revision: int) -> dict[str, Any]:
        return {
            "lifetime": self.lifetime,
            "speechMode": self.speech_mode,
            "taskFilters": list(self.task_filters),
            "taskFilterEntries": [
                entry.to_payload() for entry in self.task_filter_entries
            ],
            "selectedView": self.selected_view,
            "shellSettings": dict(self.shell_settings),
            "revision": revision,
        }


@dataclass(frozen=True)
class TeamAgentIdentity:
    actor_id: str
    target_id: str = ""
    thread_id: str = ""
    actual_driver: str = ""
    actual_model: str = ""
    actual_effort: str = ""
    actual_service_tier: str = ""
    desired_driver: str = ""
    desired_model: str = ""
    desired_effort: str = ""
    transcript_owner: str = ""
    renewal_state: str = ""
    renewal_ancestor_thread_id: str = ""
    renewal_successor_thread_id: str = ""
    renewal_revision: int = 0
    updated_at: float = 0.0

    def to_payload(self) -> dict[str, Any]:
        return {
            "actorId": self.actor_id,
            "targetId": self.target_id,
            "threadId": self.thread_id,
            "driverName": self.actual_driver or self.desired_driver,
            "driverModel": self.actual_model or self.desired_model,
            "driverEffort": self.actual_effort or self.desired_effort,
            "actualDriver": self.actual_driver,
            "actualModel": self.actual_model,
            "actualEffort": self.actual_effort,
            "actualServiceTier": self.actual_service_tier,
            "desiredDriver": self.desired_driver,
            "desiredModel": self.desired_model,
            "desiredEffort": self.desired_effort,
            "transcriptOwner": self.transcript_owner,
            "renewalState": self.renewal_state,
            "renewalAncestorThreadId": self.renewal_ancestor_thread_id,
            "renewalSuccessorThreadId": self.renewal_successor_thread_id,
            "renewalRevision": self.renewal_revision,
            "updatedAt": self.updated_at,
        }


@dataclass(frozen=True)
class TeamRenewalState:
    agent_id: str
    team_id: str
    state: str
    ancestor_thread_id: str
    successor_agent_id: str
    revision: int
    successor_thread_id: str = ""
    team_slot: int | None = None
    predecessor_identity: dict[str, Any] = field(default_factory=dict)
    successor_identity: dict[str, Any] = field(default_factory=dict)

    @property
    def requested(self) -> bool:
        return self.state == RENEWAL_STATE_REQUESTED


@dataclass(frozen=True)
class TeamMember:
    agent_id: str
    agent_facts: dict[str, str] = field(default_factory=dict)
    renewal: TeamRenewalState | None = None

    def to_payload(self) -> dict[str, Any]:
        return {
            "agentId": self.agent_id,
            "agentFacts": dict(self.agent_facts),
            "renewalIntent": renewal_intent_payload(
                self.renewal, agent_id=self.agent_id
            ),
        }


@dataclass(frozen=True)
class TeamState:
    team_id: str
    status: str
    revision: int
    config_revision: int
    config: TeamConfig
    members: tuple[TeamMember, ...]
    split_back_available: bool = False
    split_back_member_count: int = 0

    def to_payload(self) -> dict[str, Any]:
        return {
            "teamId": self.team_id,
            "status": self.status,
            "revision": self.revision,
            "config": self.config.to_payload(self.config_revision),
            "members": [member.to_payload() for member in self.members],
            "splitBack": {
                "available": self.split_back_available,
                "memberCount": self.split_back_member_count,
            },
        }


@dataclass(frozen=True)
class TeamSnapshot:
    global_revision: int
    teams: tuple[TeamState, ...]

    def to_payload(self) -> dict[str, Any]:
        return {
            "globalRevision": self.global_revision,
            "teams": [team.to_payload() for team in self.teams],
        }


def renewal_intent_payload(
    renewal: TeamRenewalState | None, *, agent_id: str = ""
) -> dict[str, Any]:
    resolved_agent_id = renewal.agent_id if renewal is not None else agent_id
    return {
        "agentId": resolved_agent_id,
        "requested": bool(renewal and renewal.requested),
        "state": renewal.state if renewal is not None else "",
        "teamId": renewal.team_id if renewal is not None else "",
        "ancestorThreadId": renewal.ancestor_thread_id if renewal is not None else "",
        "successorAgentId": renewal.successor_agent_id if renewal is not None else "",
        "successorThreadId": (
            renewal.successor_thread_id if renewal is not None else ""
        ),
        "teamSlot": renewal.team_slot if renewal is not None else None,
        "predecessorIdentity": (
            dict(renewal.predecessor_identity) if renewal is not None else {}
        ),
        "successorIdentity": (
            dict(renewal.successor_identity) if renewal is not None else {}
        ),
        "revision": renewal.revision if renewal is not None else 0,
    }
