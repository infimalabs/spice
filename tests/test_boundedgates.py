"""Shared bounded-gate decisions and sticky-latch lifecycle."""

import subprocess
from pathlib import Path

import pytest

from spice.studies import gates


@pytest.mark.parametrize(
    ("value", "bounds", "latched", "expected"),
    [
        (
            11,
            gates.BoundedValue(base_limit=10, flex_limit=15),
            False,
            gates.BoundedDisposition(
                limit=15,
                over_limit=False,
                over_base=True,
                flex_breach=False,
            ),
        ),
        (
            11,
            gates.BoundedValue(base_limit=10, flex_limit=15),
            True,
            gates.BoundedDisposition(
                limit=10,
                over_limit=True,
                over_base=True,
                flex_breach=False,
            ),
        ),
        (
            16,
            gates.BoundedValue(base_limit=10, flex_limit=15),
            False,
            gates.BoundedDisposition(
                limit=15,
                over_limit=True,
                over_base=True,
                flex_breach=True,
            ),
        ),
        (
            100,
            gates.BoundedValue(base_limit=10, flex_limit=15, unlimited=True),
            True,
            gates.BoundedDisposition(
                limit=10,
                over_limit=False,
                over_base=False,
                flex_breach=False,
            ),
        ),
    ],
)
def test_bounded_disposition_is_complete(
    value: int,
    bounds: gates.BoundedValue,
    latched: bool,
    expected: gates.BoundedDisposition,
):
    assert gates.bounded_disposition(value, bounds, latched=latched) == expected


def test_path_sticky_latch_remaps_retains_breaches_and_heals(tmp_path):
    repo = _init_repo(tmp_path)
    ledger = gates.path_sticky_ledger("bounded-paths.json")
    old_path = Path("old.py")
    new_path = Path("new.py")
    breached_path = Path("breached.py")
    gates.persist_sticky_ledger(ledger, {old_path}, root=repo)

    state = gates.reconcile_sticky_latch(
        ledger,
        root=repo,
        renames={old_path: new_path},
        retain=lambda paths: paths & {new_path},
        breach_keys={breached_path},
        persist=True,
    )

    assert state == gates.StickyLatchState(
        loaded={old_path, new_path},
        retained={new_path},
        updated={new_path, breached_path},
    )
    assert gates.load_sticky_ledger(ledger, root=repo, renames={}) == {
        new_path,
        breached_path,
    }

    healed = gates.reconcile_sticky_latch(
        ledger,
        root=repo,
        renames={},
        retain=lambda _paths: set(),
        breach_keys=set(),
        persist=True,
    )

    assert healed.updated == set()
    assert gates.load_sticky_ledger(ledger, root=repo, renames={}) == set()


def test_function_sticky_ledger_preserves_schema_and_rename_identity(tmp_path):
    repo = _init_repo(tmp_path)
    ledger = gates.function_sticky_ledger("bounded-functions.json")
    gates.persist_sticky_ledger(ledger, {("old.py", "run")}, root=repo)

    loaded = gates.load_sticky_ledger(
        ledger,
        root=repo,
        renames={Path("old.py"): Path("new.py")},
    )

    assert loaded == {("old.py", "run"), ("new.py", "run")}


def test_peer_claims_return_only_the_existing_owner(tmp_path):
    repo = _init_repo(tmp_path)
    path = Path("shared.py")
    (repo / path).write_text("value = 1\n", encoding="utf-8")

    gates.peer_flex_slice_claims(
        {path},
        root=repo,
        actor="owner",
        renames={},
        now=100.0,
    )
    peer_claims = gates.peer_flex_slice_claims(
        {path},
        root=repo,
        actor="peer",
        renames={},
        now=101.0,
    )

    assert peer_claims[path].actor == "owner"
    assert peer_claims[path].path == path


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    return repo
