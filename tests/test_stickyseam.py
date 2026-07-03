"""The flex+sticky study scans must be pure unless persistence is opt-in.

A reporting/study caller (the default) must not advance the on-disk sticky
floor; only a committing gate passes ``persist=True``. These tests assert the
sticky JSON in the git dir is untouched by a default scan and written by a
``persist=True`` scan, for both fileloc and complexity.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from spice.flexstate import (
    FLEX_SLICE_CLAIM_TTL_SECONDS,
    FLEX_SLICE_CLAIMS_GIT_PATH,
    FLEX_SLICE_CLAIMS_VERSION,
    FlexSliceClaim,
    git_state_path,
    load_flex_slice_claims,
    save_flex_slice_claims,
)
from spice.studies import complexity, fileloc


def test_fileloc_reporting_scan_does_not_persist_sticky(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")

    fileloc.scan_staged_loc_violations(
        [Path("big.py")], root=repo, limit=5, flex_limit_value=7
    )

    assert not git_state_path(
        fileloc.FILE_LOC_STICKY_STATE_GIT_PATH, root=repo
    ).exists()


def test_fileloc_gate_scan_persists_sticky(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")

    fileloc.scan_staged_loc_violations(
        [Path("big.py")], root=repo, limit=5, flex_limit_value=7, persist=True
    )

    assert git_state_path(fileloc.FILE_LOC_STICKY_STATE_GIT_PATH, root=repo).exists()


def test_complexity_reporting_scan_does_not_persist_sticky(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path)
    record = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=20, nloc=20
    )
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    complexity.scan_staged_complexity_violations(
        [Path("big.py")], root=repo, max_length=3, length_flex_limit_value=4
    )

    assert not git_state_path(
        complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH, root=repo
    ).exists()


def test_complexity_gate_scan_persists_sticky(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path)
    record = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=20, nloc=20
    )
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    assert git_state_path(
        complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH, root=repo
    ).exists()


def test_flex_slice_claim_round_trips_live_claim(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "src").mkdir()
    (repo / "src/app.py").write_text("x = 1\n", encoding="utf-8")
    claim = FlexSliceClaim(
        path=Path("./src\\app.py"),
        actor="actor-a",
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )
    loaded = FlexSliceClaim(
        path=Path("src/app.py"),
        actor="actor-a",
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )

    save_flex_slice_claims((claim,), root=repo)

    assert load_flex_slice_claims(root=repo, now=101.0) == (loaded,)
    assert _flex_slice_claims_payload(repo) == {
        "claims": [
            {
                "actor": "actor-a",
                "created_at": 100.0,
                "expires_at": 100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
                "path": "src/app.py",
            }
        ],
        "ttl_seconds": FLEX_SLICE_CLAIM_TTL_SECONDS,
        "version": FLEX_SLICE_CLAIMS_VERSION,
    }


def test_flex_slice_claim_prune_persists_only_active_claims(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "expired.py").write_text("old = 1\n", encoding="utf-8")
    (repo / "live.py").write_text("new = 1\n", encoding="utf-8")
    expired = FlexSliceClaim(
        path=Path("expired.py"),
        actor="actor-expired",
        created_at=10.0,
        expires_at=20.0,
    )
    live = FlexSliceClaim(
        path=Path("live.py"),
        actor="actor-live",
        created_at=30.0,
        expires_at=300.0,
    )
    save_flex_slice_claims((expired, live), root=repo)

    active = load_flex_slice_claims(root=repo, now=100.0)
    save_flex_slice_claims(active, root=repo)

    assert active == (live,)
    assert load_flex_slice_claims(root=repo, now=100.0) == (live,)
    assert _flex_slice_claims_payload(repo)["claims"] == [
        {
            "actor": "actor-live",
            "created_at": 30.0,
            "expires_at": 300.0,
            "path": "live.py",
        }
    ]


def test_flex_slice_claim_prune_follows_renames_and_existing_paths(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "live.py").write_text("keep = 1\n", encoding="utf-8")
    (repo / "new.py").write_text("moved = 1\n", encoding="utf-8")
    live = FlexSliceClaim(
        path=Path("live.py"),
        actor="actor-live",
        created_at=20.0,
        expires_at=300.0,
    )
    renamed = FlexSliceClaim(
        path=Path("old.py"),
        actor="actor-rename",
        created_at=10.0,
        expires_at=300.0,
    )
    missing = FlexSliceClaim(
        path=Path("missing.py"),
        actor="actor-missing",
        created_at=30.0,
        expires_at=300.0,
    )
    expected = (
        live,
        FlexSliceClaim(
            path=Path("new.py"),
            actor="actor-rename",
            created_at=10.0,
            expires_at=300.0,
        ),
    )
    renames = {Path("old.py"): Path("new.py")}
    save_flex_slice_claims((renamed, missing, live), root=repo)

    active = load_flex_slice_claims(root=repo, renames=renames, now=100.0)
    save_flex_slice_claims(active, root=repo)

    assert active == expected
    assert load_flex_slice_claims(root=repo, now=100.0) == expected
    assert _flex_slice_claims_payload(repo)["claims"] == [
        {
            "actor": "actor-live",
            "created_at": 20.0,
            "expires_at": 300.0,
            "path": "live.py",
        },
        {
            "actor": "actor-rename",
            "created_at": 10.0,
            "expires_at": 300.0,
            "path": "new.py",
        },
    ]


def _flex_slice_claims_payload(repo: Path) -> dict:
    return json.loads(
        git_state_path(FLEX_SLICE_CLAIMS_GIT_PATH, root=repo).read_text(
            encoding="utf-8"
        )
    )


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo
