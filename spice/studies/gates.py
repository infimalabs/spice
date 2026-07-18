"""Shared bounded-gate mechanics for flex limits and sticky latches."""

from __future__ import annotations

from collections.abc import Callable, Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Generic, TypeVar

from spice.flexstate import (
    FlexSliceClaim,
    claim_flex_slice_paths,
    flex_limit,
    git_state_path,
    load_sticky_items,
    save_sticky_items,
    sticky_function_keys_after_renames,
    sticky_paths_after_renames,
)
from spice.studies.walk import staged_renames

StickyKey = TypeVar("StickyKey")
FunctionKey = tuple[str, str]
GateErrors = tuple[type[BaseException], ...]


@dataclass(frozen=True)
class BoundedValue:
    """One measured value's base, flex, and unlimited policy."""

    base_limit: int
    flex_limit: int
    unlimited: bool = False

    @classmethod
    def from_base(
        cls,
        base_limit: int,
        flex_limit_value: int | None = None,
        *,
        unlimited: bool = False,
    ) -> BoundedValue:
        return cls(
            base_limit=base_limit,
            flex_limit=(
                flex_limit_value
                if flex_limit_value is not None
                else flex_limit(base_limit)
            ),
            unlimited=unlimited,
        )


@dataclass(frozen=True)
class BoundedDisposition:
    """The complete gate decision for one measured value."""

    limit: int
    over_limit: bool
    over_base: bool
    flex_breach: bool


def bounded_disposition(
    value: int, bounds: BoundedValue, *, latched: bool = False
) -> BoundedDisposition:
    active_limit = bounds.base_limit if latched else bounds.flex_limit
    enabled = not bounds.unlimited
    return BoundedDisposition(
        limit=active_limit,
        over_limit=enabled and value > active_limit,
        over_base=enabled and value > bounds.base_limit,
        flex_breach=enabled and value > bounds.flex_limit,
    )


@dataclass(frozen=True)
class StickyLedger(Generic[StickyKey]):
    """Typed storage contract for one sticky-latch set."""

    git_path: str
    entries_key: str
    decode: Callable[[Any], StickyKey | None]
    encode: Callable[[StickyKey], Any]
    remap: Callable[[set[StickyKey], dict[Path, Path]], set[StickyKey]]
    version: int = 1


@dataclass(frozen=True)
class StickyLatchState(Generic[StickyKey]):
    loaded: set[StickyKey]
    retained: set[StickyKey]
    updated: set[StickyKey]


def path_sticky_ledger(
    git_path: str, *, entries_key: str = "paths", version: int = 1
) -> StickyLedger[Path]:
    return StickyLedger(
        git_path=git_path,
        entries_key=entries_key,
        decode=lambda raw: Path(raw) if isinstance(raw, str) else None,
        encode=lambda path: path.as_posix(),
        remap=sticky_paths_after_renames,
        version=version,
    )


def function_sticky_ledger(
    git_path: str, *, entries_key: str = "functions", version: int = 1
) -> StickyLedger[FunctionKey]:
    return StickyLedger(
        git_path=git_path,
        entries_key=entries_key,
        decode=_decode_function_key,
        encode=list,
        remap=sticky_function_keys_after_renames,
        version=version,
    )


def load_sticky_ledger(
    ledger: StickyLedger[StickyKey],
    *,
    root: Path,
    renames: dict[Path, Path],
) -> set[StickyKey]:
    loaded = load_sticky_items(
        root=root,
        state_path=None,
        git_path=ledger.git_path,
        entries_key=ledger.entries_key,
        decode=ledger.decode,
        version=ledger.version,
    )
    return ledger.remap(loaded, renames)


def reconcile_sticky_latch(
    ledger: StickyLedger[StickyKey],
    *,
    root: Path,
    renames: dict[Path, Path],
    retain: Callable[[set[StickyKey]], set[StickyKey]],
    breach_keys: Iterable[StickyKey],
    persist: bool,
    load_errors: GateErrors = (),
    persist_errors: GateErrors = (),
) -> StickyLatchState[StickyKey]:
    try:
        loaded = load_sticky_ledger(ledger, root=root, renames=renames)
    except load_errors:
        loaded = set()
    retained = retain(set(loaded)) & loaded
    updated = retained | set(breach_keys)
    if persist and updated != loaded:
        try:
            persist_sticky_ledger(ledger, updated, root=root)
        except persist_errors:
            pass
    return StickyLatchState(loaded=loaded, retained=retained, updated=updated)


def persist_sticky_ledger(
    ledger: StickyLedger[StickyKey], items: set[StickyKey], *, root: Path
) -> None:
    if items:
        save_sticky_items(
            items,
            root=root,
            state_path=None,
            git_path=ledger.git_path,
            entries_key=ledger.entries_key,
            encode=ledger.encode,
            version=ledger.version,
        )
        return
    state_path = git_state_path(ledger.git_path, root=root)
    if state_path.exists():
        state_path.unlink()


def staged_gate_renames(root: Path, *, errors: GateErrors = ()) -> dict[Path, Path]:
    try:
        return staged_renames(root)
    except errors:
        return {}


def peer_flex_slice_claims(
    paths: set[Path],
    *,
    root: Path,
    actor: str,
    renames: dict[Path, Path],
    now: float | None,
) -> dict[Path, FlexSliceClaim]:
    decisions = claim_flex_slice_paths(
        paths,
        root=root,
        actor=actor,
        renames=renames,
        now=now,
    )
    return {
        path: decision.claim
        for path, decision in decisions.items()
        if decision.peer_held
    }


def _decode_function_key(raw: Any) -> FunctionKey | None:
    if (
        isinstance(raw, list)
        and len(raw) == 2
        and all(isinstance(item, str) for item in raw)
    ):
        return (raw[0], raw[1])
    return None
