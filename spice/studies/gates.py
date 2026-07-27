"""Shared bounded-gate mechanics for flex limits and sticky latches."""

from __future__ import annotations

from collections.abc import Callable, Iterable, Iterator
from contextlib import contextmanager
from contextvars import ContextVar
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


def held_at_base_reason(reason: str, *, held: bool, ledger_label: str) -> str:
    """Tag a failing reason that is over base only because a latch holds it.

    Names the ledger file carrying the latch so the otherwise-invisible git-dir
    state is reachable; a current flex breach (``held`` is false) is left as-is.
    """
    if held:
        return f"{reason} (held at base by {ledger_label})"
    return reason


def render_latch_held_guidance(noun: str) -> str:
    """The shared remedy line for a value held to base by a sticky latch.

    A held-at-base value is within its flex band now and fails only because an
    earlier breach latched it in the named ledger; the fix is to drop back under
    the base limit (or let a peer worktree's collateral latch heal on the shared
    baseline), never to split a file that is already within flex. Both the
    file-shape and complexity boards render this through one seam so the two
    reconcile_sticky_latch consumers cannot drift apart.
    """
    return (
        f"  a held-at-base {noun} is within flex now but latched by an earlier "
        "breach recorded in the named ledger; it clears when any scan sees it "
        "back under its base limit, so a latch left by a peer worktree heals "
        "once the fix lands on the shared baseline"
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


class DeferredStickyWrites:
    """Sticky-ledger writes held until the run that produced them is accepted.

    A gate scan computes its latch set well before anything decides whether the
    commit lands. Holding the write here keeps a rejected run from latching a
    breach that never entered the tree, while the in-memory latch state each
    scan returns is untouched, so the current run's findings are unchanged.
    """

    def __init__(self) -> None:
        self._held: list[tuple[StickyLedger[Any], set[Any], Path, GateErrors]] = []

    def hold(
        self,
        ledger: StickyLedger[StickyKey],
        items: set[StickyKey],
        *,
        root: Path,
        errors: GateErrors,
    ) -> None:
        self._held.append((ledger, set(items), root, errors))

    def commit(self) -> None:
        """Persist every held write, each under its own call site's tolerances."""
        held, self._held = self._held, []
        for ledger, items, root, errors in held:
            try:
                persist_sticky_ledger(ledger, items, root=root)
            except errors:
                pass


_DEFERRED_STICKY_WRITES: ContextVar[DeferredStickyWrites | None] = ContextVar(
    "spice_deferred_sticky_writes", default=None
)


@contextmanager
def deferred_sticky_writes() -> Iterator[DeferredStickyWrites]:
    """Hold every sticky-ledger write of one run for the caller to commit."""
    pending = DeferredStickyWrites()
    token = _DEFERRED_STICKY_WRITES.set(pending)
    try:
        yield pending
    finally:
        _DEFERRED_STICKY_WRITES.reset(token)


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
        _record_sticky_update(ledger, updated, root=root, persist_errors=persist_errors)
    return StickyLatchState(loaded=loaded, retained=retained, updated=updated)


def _record_sticky_update(
    ledger: StickyLedger[StickyKey],
    updated: set[StickyKey],
    *,
    root: Path,
    persist_errors: GateErrors,
) -> None:
    """Write the latch now, or hold it for whoever accepts the run."""
    pending = _DEFERRED_STICKY_WRITES.get()
    if pending is not None:
        pending.hold(ledger, updated, root=root, errors=persist_errors)
        return
    try:
        persist_sticky_ledger(ledger, updated, root=root)
    except persist_errors:
        pass


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
