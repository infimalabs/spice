"""Identifier normalization helpers for serve teams."""

from __future__ import annotations

from typing import Iterable

from spice.agent.identity import canonical_thread_id
from spice.errors import SpiceError

TARGET_ACTOR_PREFIX = "target:"
THREAD_ACTOR_PREFIX = "thread:"


def normalized_id(value: str, field_name: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise SpiceError(f"{field_name} must be non-empty")
    return normalized


def target_actor_id(target_id: str) -> str:
    return f"{TARGET_ACTOR_PREFIX}{normalized_id(target_id, 'target_id')}"


def thread_actor_id(thread_id: str) -> str:
    thread = canonical_thread_id(thread_id)
    if not thread:
        raise SpiceError("thread_id must be non-empty")
    return f"{THREAD_ACTOR_PREFIX}{thread}"


def actor_value(actor_id: str) -> str:
    actor = normalized_id(actor_id, "actor_id")
    if actor.startswith((TARGET_ACTOR_PREFIX, THREAD_ACTOR_PREFIX)):
        return normalized_id(actor.split(":", 1)[1], "actor_id")
    return actor


def thread_id_for_actor(actor_id: str) -> str:
    actor = normalized_id(actor_id, "actor_id")
    if actor.startswith(THREAD_ACTOR_PREFIX):
        return canonical_thread_id(actor.split(":", 1)[1])
    if actor.startswith(TARGET_ACTOR_PREFIX):
        return ""
    return canonical_thread_id(actor)


def normalize_actor_id(actor_id: str, *, target_ids: Iterable[str] = ()) -> str:
    actor = normalized_id(actor_id, "actor_id")
    if actor.startswith(TARGET_ACTOR_PREFIX):
        return target_actor_id(actor.split(":", 1)[1])
    if actor.startswith(THREAD_ACTOR_PREFIX):
        return thread_actor_id(actor.split(":", 1)[1])
    if actor in set(target_ids):
        return target_actor_id(actor)
    return thread_actor_id(actor)


def agent_alias_ids(agent_id: str, aliases: Iterable[str]) -> list[str]:
    ids = [normalized_id(agent_id, "agent_id")]
    for alias in aliases:
        normalized = normalized_id(alias, "agent_alias")
        if normalized not in ids:
            ids.append(normalized)
    return ids
