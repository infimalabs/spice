"""Shared bounded-gate decisions and sticky-latch lifecycle."""

import ast
import inspect
import subprocess
from collections import Counter
from pathlib import Path

import pytest

from spice.studies import complexity, fileloc, gates, repodocs


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


_BOUNDED_STUDY_CONTRACT = {
    "fileloc": {
        "flexstate_imports": [
            "FlexSliceClaim",
            "flex_limit",
            "render_flex_slice_claim_redirect",
        ],
        "kernel_call_counts": {
            "BoundedValue": 2,
            "bounded_disposition": 4,
            "held_at_base_reason": 2,
            "path_sticky_ledger": 2,
            "peer_flex_slice_claims": 1,
            "reconcile_sticky_latch": 1,
            "render_latch_held_guidance": 1,
            "staged_gate_renames": 1,
        },
        "module_definitions": [
            "_breach_paths",
            "_file_shape_breach_sets",
            "_file_shape_scan_config",
            "_file_shape_sticky_state",
            "_is_file_shape_candidate",
            "_is_text_blob",
            "_repo_path",
            "_resolved_file_shape_bounds",
            "_retained_file_shape_sticky",
            "_scan_file_shape_findings",
            "_scan_staged_file_shape",
            "count_file_bytes",
            "count_file_lines",
            "is_generated_lockfile_path",
            "render_loc_board",
            "scan_loc_violations",
            "scan_staged_loc_violations",
        ],
    },
    "complexity": {
        "flexstate_imports": [
            "FlexSliceClaim",
            "flex_limit",
            "render_flex_slice_claim_redirect",
        ],
        "kernel_call_counts": {
            "BoundedValue": 2,
            "bounded_disposition": 5,
            "function_sticky_ledger": 2,
            "held_at_base_reason": 2,
            "peer_flex_slice_claims": 1,
            "reconcile_sticky_latch": 2,
            "render_latch_held_guidance": 1,
            "staged_gate_renames": 1,
        },
        "module_definitions": [
            "_complexity_breach_sets",
            "_complexity_findings",
            "_complexity_input_label",
            "_resolved_complexity_bounds",
            "_retained_complexity_sticky",
            "collect_complexity_records",
            "complexity_hotspot_rows",
            "render_complexity_board",
            "render_complexity_hotspots",
            "require_lizard",
            "scan_staged_complexity_violations",
        ],
    },
    "repodocs": {
        "flexstate_imports": [
            "FlexSliceClaim",
            "render_flex_slice_claim_redirect",
        ],
        "kernel_call_counts": {
            "BoundedValue": 1,
            "bounded_disposition": 2,
            "path_sticky_ledger": 1,
            "peer_flex_slice_claims": 1,
            "reconcile_sticky_latch": 1,
            "staged_gate_renames": 1,
        },
        "module_definitions": [
            "_doc_char_count",
            "_repo_doc_bounds",
            "_repo_doc_disposition",
            "_tracked_paths_or_empty",
            "render_repo_truth_doc_guard_error",
            "render_repo_truth_doc_lines",
            "repo_truth_doc_candidate_paths",
            "repo_truth_doc_findings",
            "repo_truth_docs",
        ],
    },
}


def _bounded_study_contract(module):
    tree = ast.parse(inspect.getsource(module))
    call_counts = Counter(
        node.func.attr
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "gates"
    )
    return {
        "flexstate_imports": sorted(
            alias.name
            for node in ast.walk(tree)
            if isinstance(node, ast.ImportFrom) and node.module == "spice.flexstate"
            for alias in node.names
        ),
        "kernel_call_counts": dict(sorted(call_counts.items())),
        "module_definitions": sorted(
            node.name
            for node in tree.body
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        ),
    }


def test_bounded_studies_delegate_one_exact_kernel_contract():
    actual = {
        module.__name__.rsplit(".", 1)[-1]: _bounded_study_contract(module)
        for module in (fileloc, complexity, repodocs)
    }

    assert actual == _BOUNDED_STUDY_CONTRACT


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    return repo
