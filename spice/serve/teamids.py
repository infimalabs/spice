"""Identifier normalization helpers for serve teams."""

from __future__ import annotations

from typing import Iterable

from spice.errors import SpiceError


def normalized_id(value: str, field_name: str) -> str:
    normalized = str(value or "").strip()
    if not normalized:
        raise SpiceError(f"{field_name} must be non-empty")
    return normalized


def agent_alias_ids(agent_id: str, aliases: Iterable[str]) -> list[str]:
    ids = [normalized_id(agent_id, "agent_id")]
    for alias in aliases:
        normalized = normalized_id(alias, "agent_alias")
        if normalized not in ids:
            ids.append(normalized)
    return ids
