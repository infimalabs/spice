"""The flex+sticky study scans must be pure unless persistence is opt-in.

A reporting/study caller (the default) must not advance the on-disk sticky
floor; only a committing gate passes ``persist=True``. These tests assert the
sticky JSON in the git dir is untouched by a default scan and written by a
``persist=True`` scan, for both fileloc and complexity.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.flexstate import git_state_path
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


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo
