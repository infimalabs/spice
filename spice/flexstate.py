"""Sticky study state: once a file breaches a flex limit it stays watched.

Shape guards allow headroom (`flex_limit`) over a base limit, but a file that
ever breached keeps its base limit until it shrinks back under it. Sticky
latches persist in the current worktree's git dir so lane-local decisions
survive checkouts and rebases without touching the working tree. Live flex
claims instead persist in the shared git common dir so peer worktrees can
coordinate ownership of the same slice.
"""

from __future__ import annotations

import json
import time
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, TypeVar

from spice.paths import shared_state_path, worktree_state_path
from spice.policy import flex_limit as flex_limit  # single source of the ratio

StickyKey = TypeVar("StickyKey")
FLEX_SLICE_CLAIM_TTL_SECONDS = 6 * 60 * 60
FLEX_SLICE_CLAIMS_VERSION = 1
FLEX_SLICE_CLAIMS_GIT_PATH = "flex-slice-claims.json"
FLEX_SLICE_CLAIMED = "claimed"
FLEX_SLICE_OWNED = "owned"
FLEX_SLICE_PEER_HELD = "peer-held"


@dataclass(frozen=True)
class FlexSliceClaim:
    path: Path
    actor: str
    created_at: float
    expires_at: float


@dataclass(frozen=True)
class FlexSliceClaimDecision:
    path: Path
    actor: str
    claim: FlexSliceClaim
    status: str

    @property
    def peer_held(self) -> bool:
        return self.status == FLEX_SLICE_PEER_HELD


def git_state_path(git_path: str, *, root: Path) -> Path:
    return worktree_state_path(root, git_path)


def flex_slice_claims_state_path(
    *, root: Path, git_path: str = FLEX_SLICE_CLAIMS_GIT_PATH
) -> Path:
    return shared_state_path(root, git_path)


def load_sticky_items(
    *,
    root: Path,
    state_path: Path | None,
    git_path: str,
    entries_key: str,
    decode: Callable[[Any], StickyKey | None],
    version: int = 1,
) -> set[StickyKey]:
    path = state_path or git_state_path(git_path, root=root)
    if not path.exists():
        return set()
    payload = json.loads(path.read_text(encoding="utf-8"))
    if payload.get("version") != version:
        return set()
    items: set[StickyKey] = set()
    for raw in payload.get(entries_key, []):
        if (item := decode(raw)) is not None:
            items.add(item)
    return items


def save_sticky_items(
    items: set[StickyKey],
    *,
    root: Path,
    state_path: Path | None,
    git_path: str,
    entries_key: str,
    encode: Callable[[StickyKey], Any],
    version: int = 1,
) -> None:
    path = state_path or git_state_path(git_path, root=root)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "version": version,
        entries_key: [encode(item) for item in sorted(items, key=str)],
    }
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def load_flex_slice_claims(
    *,
    root: Path,
    state_path: Path | None = None,
    git_path: str = FLEX_SLICE_CLAIMS_GIT_PATH,
    renames: dict[Path, Path] | None = None,
    now: float | None = None,
) -> tuple[FlexSliceClaim, ...]:
    path = state_path or flex_slice_claims_state_path(root=root, git_path=git_path)
    if not path.exists():
        return ()
    payload = json.loads(path.read_text(encoding="utf-8"))
    if payload.get("version") != FLEX_SLICE_CLAIMS_VERSION:
        return ()
    active: list[FlexSliceClaim] = []
    cutoff = time.time() if now is None else float(now)
    for raw in payload.get("claims", []):
        claim = _decode_flex_slice_claim(raw)
        if claim is None:
            continue
        claim = _active_flex_slice_claim(
            claim,
            root=root,
            renames=renames or {},
            now=cutoff,
        )
        if claim is not None:
            active.append(claim)
    return _sorted_flex_slice_claims(active)


def save_flex_slice_claims(
    claims: tuple[FlexSliceClaim, ...] | list[FlexSliceClaim],
    *,
    root: Path,
    state_path: Path | None = None,
    git_path: str = FLEX_SLICE_CLAIMS_GIT_PATH,
) -> None:
    path = state_path or flex_slice_claims_state_path(root=root, git_path=git_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "version": FLEX_SLICE_CLAIMS_VERSION,
        "ttl_seconds": FLEX_SLICE_CLAIM_TTL_SECONDS,
        "claims": [
            _encode_flex_slice_claim(claim)
            for claim in _sorted_flex_slice_claims(claims)
        ],
    }
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def claim_flex_slice_paths(
    paths: Iterable[Path],
    *,
    root: Path,
    actor: str,
    state_path: Path | None = None,
    git_path: str = FLEX_SLICE_CLAIMS_GIT_PATH,
    renames: dict[Path, Path] | None = None,
    now: float | None = None,
) -> dict[Path, FlexSliceClaimDecision]:
    claimant = actor.strip()
    if not claimant:
        return {}
    timestamp = time.time() if now is None else float(now)
    requested = _unique_flex_slice_paths(paths)
    if not requested:
        return {}
    active = load_flex_slice_claims(
        root=root,
        state_path=state_path,
        git_path=git_path,
        renames=renames,
        now=timestamp,
    )
    updated = list(active)
    changed = False
    decisions: dict[Path, FlexSliceClaimDecision] = {}
    for path in requested:
        owner = _owning_flex_slice_claim(active, path)
        if owner is not None and owner.actor != claimant:
            decisions[path] = FlexSliceClaimDecision(
                path=path,
                actor=claimant,
                claim=owner,
                status=FLEX_SLICE_PEER_HELD,
            )
            continue
        claim = FlexSliceClaim(
            path=path,
            actor=claimant,
            created_at=owner.created_at if owner is not None else timestamp,
            expires_at=timestamp + FLEX_SLICE_CLAIM_TTL_SECONDS,
        )
        decisions[path] = FlexSliceClaimDecision(
            path=path,
            actor=claimant,
            claim=claim,
            status=FLEX_SLICE_OWNED if owner is not None else FLEX_SLICE_CLAIMED,
        )
        if owner != claim:
            updated = [
                existing
                for existing in updated
                if not (
                    _state_repo_path(existing.path) == path
                    and existing.actor == claimant
                )
            ]
            updated.append(claim)
            changed = True
    if changed:
        save_flex_slice_claims(
            updated,
            root=root,
            state_path=state_path,
            git_path=git_path,
        )
    return decisions


def render_flex_slice_claim_redirect(claim: FlexSliceClaim) -> str:
    return (
        f"live flex slice held by {claim.actor} until "
        f"{_format_claim_timestamp(claim.expires_at)}; keep this change "
        "append-only or move to another seam"
    )


def _decode_flex_slice_claim(raw: Any) -> FlexSliceClaim | None:
    if not isinstance(raw, dict):
        return None
    path = _decode_claim_path(raw.get("path"))
    actor = raw.get("actor")
    created_at = _decode_claim_timestamp(raw.get("created_at"))
    expires_at = _decode_claim_timestamp(raw.get("expires_at"))
    if path is None or not isinstance(actor, str) or not actor.strip():
        return None
    if created_at is None or expires_at is None:
        return None
    if expires_at < created_at:
        return None
    return FlexSliceClaim(
        path=path,
        actor=actor.strip(),
        created_at=created_at,
        expires_at=expires_at,
    )


def _decode_claim_path(raw: Any) -> Path | None:
    if not isinstance(raw, str):
        return None
    path = _state_repo_path(Path(raw))
    return path if path.as_posix() != "." else None


def _decode_claim_timestamp(raw: Any) -> float | None:
    if not isinstance(raw, int | float):
        return None
    return float(raw)


def _unique_flex_slice_paths(paths: Iterable[Path]) -> tuple[Path, ...]:
    return tuple(
        sorted(
            {
                path
                for raw_path in paths
                if (path := _state_repo_path(raw_path)).as_posix() != "."
            },
            key=lambda path: path.as_posix(),
        )
    )


def _owning_flex_slice_claim(
    claims: tuple[FlexSliceClaim, ...], path: Path
) -> FlexSliceClaim | None:
    owners = [
        claim
        for claim in claims
        if _state_repo_path(claim.path) == _state_repo_path(path)
    ]
    if not owners:
        return None
    return min(owners, key=lambda claim: (claim.created_at, claim.actor))


def _active_flex_slice_claim(
    claim: FlexSliceClaim,
    *,
    root: Path,
    renames: dict[Path, Path],
    now: float,
) -> FlexSliceClaim | None:
    if claim.expires_at <= now:
        return None
    path = _renamed_claim_path(claim.path, renames)
    if not (root / path).exists():
        return None
    if path == claim.path:
        return claim
    return FlexSliceClaim(
        path=path,
        actor=claim.actor,
        created_at=claim.created_at,
        expires_at=claim.expires_at,
    )


def _renamed_claim_path(path: Path, renames: dict[Path, Path]) -> Path:
    normalized = _state_repo_path(path)
    normalized_renames = {
        _state_repo_path(old_path): _state_repo_path(new_path)
        for old_path, new_path in renames.items()
    }
    return normalized_renames.get(normalized, normalized)


def _encode_flex_slice_claim(claim: FlexSliceClaim) -> dict[str, object]:
    return {
        "actor": claim.actor,
        "created_at": claim.created_at,
        "expires_at": claim.expires_at,
        "path": _state_repo_path(claim.path).as_posix(),
    }


def _sorted_flex_slice_claims(
    claims: tuple[FlexSliceClaim, ...] | list[FlexSliceClaim],
) -> tuple[FlexSliceClaim, ...]:
    return tuple(
        sorted(
            claims,
            key=lambda claim: (
                _state_repo_path(claim.path).as_posix(),
                claim.actor,
                claim.created_at,
                claim.expires_at,
            ),
        )
    )


def _state_repo_path(path: Path) -> Path:
    normalized = path.as_posix().replace("\\", "/").strip()
    while normalized.startswith("./"):
        normalized = normalized[2:]
    return Path(normalized or ".")


def _format_claim_timestamp(timestamp: float) -> str:
    return (
        datetime.fromtimestamp(timestamp, UTC)
        .isoformat(timespec="seconds")
        .replace("+00:00", "Z")
    )


def sticky_paths_after_renames(
    sticky_paths: set[Path],
    renames: dict[Path, Path],
) -> set[Path]:
    if not sticky_paths:
        return sticky_paths
    updated = set(sticky_paths)
    for old_path, new_path in renames.items():
        if old_path in sticky_paths:
            updated.add(new_path)
    return updated


def sticky_function_keys_after_renames(
    sticky_keys: set[tuple[str, str]],
    renames: dict[Path, Path],
) -> set[tuple[str, str]]:
    if not sticky_keys:
        return sticky_keys
    updated = set(sticky_keys)
    for old_path, new_path in renames.items():
        old_name = old_path.as_posix()
        new_name = new_path.as_posix()
        for path, symbol in sticky_keys:
            if path == old_name:
                updated.add((new_name, symbol))
    return updated
