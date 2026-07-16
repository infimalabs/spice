"""Mutation studies run in an isolated disposable scratch checkout.

Every scenario asserts the caller's checkout byte-for-byte and the scratch
parent's exact contents: mutants are only ever observable inside the scratch
root, and every exit path -- success, survivor, timeout, baseline failure,
interruption -- retires the root, while abandoned roots from uncatchable
termination are scavenged on the next invocation without touching live runs.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from spice import paths
from spice.errors import SpiceError
from spice.procs import ProcessDeadlineExceeded
from spice.studies import mutations, scratch

SAMPLE_SOURCE = "def add(a, b):\n    return a + b\n"
MUTANT_MARKER = "return a - b"
KILLING_NODEID = "tests/test_sample.py::test_add"


def _git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    )
    return result.stdout


def _init_repo(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q", "-b", "main")
    _git(path, "config", "user.email", "spice@example.test")
    _git(path, "config", "user.name", "Spice Tests")
    return path


def _seed_project(path: Path) -> Path:
    _init_repo(path)
    (path / "pkg").mkdir()
    (path / "pkg" / "sample.py").write_text(SAMPLE_SOURCE, encoding="utf-8")
    _git(path, "add", ".")
    _git(path, "commit", "-q", "-m", "initial")
    return path


def _tree_bytes(root: Path) -> dict[str, bytes]:
    return {
        entry.relative_to(root).as_posix(): entry.read_bytes()
        for entry in sorted(root.rglob("*"))
        if entry.is_file() and ".git" not in entry.relative_to(root).parts
    }


def _caller_state(root: Path) -> tuple[dict[str, bytes], str, str]:
    return (
        _tree_bytes(root),
        _git(root, "status", "--porcelain"),
        _git(root, "ls-files", "-s"),
    )


def _dead_pid() -> int:
    child = subprocess.Popen(["true"])
    child.wait()
    return child.pid


def _killing_fake(on_mutant=None):
    """A pytest fake that reads the mutant from the scratch cwd it runs in."""

    def fake_run(command, **kwargs):
        if "--collect-only" in command:
            return subprocess.CompletedProcess(
                command, 0, stdout=f"{KILLING_NODEID}\n", stderr=""
            )
        scratch_source = Path(kwargs["cwd"]) / "pkg" / "sample.py"
        if MUTANT_MARKER in scratch_source.read_text(encoding="utf-8"):
            if on_mutant is not None:
                return on_mutant(command, kwargs)
            return subprocess.CompletedProcess(
                command,
                1,
                stdout=f"FAILED {KILLING_NODEID} - AssertionError\n",
                stderr="",
            )
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    return fake_run


def _patch_runners(monkeypatch, fake_run) -> None:
    monkeypatch.setattr(mutations, "run_bounded_process_group", fake_run)
    monkeypatch.setattr(mutations, "run_tool_command", fake_run)


def _run_study(root: Path) -> mutations.MutationStudy:
    return mutations.run_mutation_study(
        [Path("pkg/sample.py")],
        root=root,
        test_paths=[Path("tests/test_sample.py")],
        max_mutants_per_module=1,
        timeout_seconds=5,
    )


def test_seed_effective_snapshot_reproduces_effective_content(tmp_path):
    root = _init_repo(tmp_path / "repo")
    (root / ".gitignore").write_text("*.log\n", encoding="utf-8")
    (root / "committed.py").write_text("committed = 1\n", encoding="utf-8")
    (root / "edited.py").write_text("edited = 1\n", encoding="utf-8")
    (root / "deleted.py").write_text("deleted = 1\n", encoding="utf-8")
    _git(root, "add", ".")
    _git(root, "commit", "-q", "-m", "initial")
    (root / "edited.py").write_text("edited = 2\n", encoding="utf-8")
    (root / "staged.py").write_text("staged = 1\n", encoding="utf-8")
    _git(root, "add", "staged.py")
    (root / "staged.py").write_text("staged = 2\n", encoding="utf-8")
    (root / "untracked.py").write_text("untracked = 1\n", encoding="utf-8")
    (root / "ignored.log").write_text("ignored\n", encoding="utf-8")
    (root / "deleted.py").unlink()
    scratch_root = tmp_path / "scratch"
    scratch_root.mkdir()

    seeded = scratch.seed_effective_snapshot(root, scratch_root)

    expected = {
        ".gitignore": b"*.log\n",
        "committed.py": b"committed = 1\n",
        "edited.py": b"edited = 2\n",
        "staged.py": b"staged = 2\n",
        "untracked.py": b"untracked = 1\n",
    }
    scratch_files = {
        entry.relative_to(scratch_root).as_posix(): entry.read_bytes()
        for entry in sorted(scratch_root.rglob("*"))
        if entry.is_file()
    }
    assert scratch_files == expected
    assert sorted(path.as_posix() for path in seeded) == sorted(expected)


def test_seeding_outside_a_git_worktree_fails_explicitly(tmp_path):
    bare = tmp_path / "bare"
    bare.mkdir()
    scratch_root = tmp_path / "scratch"
    scratch_root.mkdir()

    with pytest.raises(SpiceError, match="requires a git worktree"):
        scratch.seed_effective_snapshot(bare, scratch_root)


def test_success_preserves_caller_and_removes_scratch(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    (root / "pkg" / "extra.py").write_text("extra = 1\n", encoding="utf-8")
    before = _caller_state(root)
    _patch_runners(monkeypatch, _killing_fake())

    study = _run_study(root)

    report = study.reports[0]
    assert report.path == "pkg/sample.py"
    assert report.killed == 1
    assert report.score == 1.0
    assert report.results[0].killed_by == (KILLING_NODEID,)
    assert report.zero_constraint_tests == ()
    assert _caller_state(root) == before
    assert [entry.name for entry in scratch.scratch_parent(root).iterdir()] == []


def test_survivor_preserves_caller_and_removes_scratch(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    before = _caller_state(root)

    def survive(command, kwargs):
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    _patch_runners(monkeypatch, _killing_fake(on_mutant=survive))

    study = _run_study(root)

    report = study.reports[0]
    assert report.survived == 1
    assert report.score == 0.0
    assert report.zero_constraint_tests == (KILLING_NODEID,)
    assert _caller_state(root) == before
    assert [entry.name for entry in scratch.scratch_parent(root).iterdir()] == []


def test_timeout_preserves_caller_and_removes_scratch(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    before = _caller_state(root)

    def time_out(command, kwargs):
        raise ProcessDeadlineExceeded(
            phase="tool.study",
            input_label="mutation baseline pytest",
            timeout_seconds=5,
            command=list(command),
        )

    _patch_runners(monkeypatch, _killing_fake(on_mutant=time_out))

    study = _run_study(root)

    report = study.reports[0]
    assert report.timed_out == 1
    assert report.score == 1.0
    assert _caller_state(root) == before
    assert [entry.name for entry in scratch.scratch_parent(root).iterdir()] == []


def test_baseline_failure_preserves_caller_and_removes_scratch(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    before = _caller_state(root)

    def failing_baseline(command, **kwargs):
        return subprocess.CompletedProcess(
            command, 1, stdout="FAILED tests/test_sample.py::test_add\n", stderr=""
        )

    _patch_runners(monkeypatch, failing_baseline)

    with pytest.raises(SpiceError, match="baseline pytest must pass"):
        _run_study(root)

    assert _caller_state(root) == before
    assert [entry.name for entry in scratch.scratch_parent(root).iterdir()] == []


def test_interruption_preserves_caller_and_removes_scratch(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    before = _caller_state(root)

    def interrupt(command, kwargs):
        raise KeyboardInterrupt

    _patch_runners(monkeypatch, _killing_fake(on_mutant=interrupt))

    with pytest.raises(KeyboardInterrupt):
        _run_study(root)

    assert _caller_state(root) == before
    assert [entry.name for entry in scratch.scratch_parent(root).iterdir()] == []


def test_caller_probes_report_real_state_during_mutation(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    (root / "wip.txt").write_text("work in progress\n", encoding="utf-8")
    before_status = _git(root, "status", "--porcelain")
    observed: list[str] = []

    def probe_caller(command, kwargs):
        # A concurrent control-plane probe while the mutant is live: the
        # caller's porcelain state must be its real, pre-study state.
        observed.append(_git(root, "status", "--porcelain"))
        return subprocess.CompletedProcess(
            command, 1, stdout=f"FAILED {KILLING_NODEID} - AssertionError\n", stderr=""
        )

    _patch_runners(monkeypatch, _killing_fake(on_mutant=probe_caller))

    study = _run_study(root)

    assert observed == [before_status]
    assert study.reports[0].killed == 1


def test_scavenge_removes_dead_roots_and_spares_live_runs(tmp_path):
    parent = tmp_path / "scratch"
    parent.mkdir()
    dead = _dead_pid()
    live = os.getpid()
    dead_marked = parent / f"run-{dead}-aaaaaaaa"
    dead_named = parent / f"run-{dead}-bbbbbbbb"
    live_marked = parent / f"run-{live}-cccccccc"
    live_named = parent / f"run-{live}-dddddddd"
    trash = parent / "trash-run-1-eeeeeeee"
    for entry in (dead_marked, dead_named, live_marked, live_named, trash):
        entry.mkdir()
        (entry / "leftover.py").write_text("leftover = 1\n", encoding="utf-8")
    paths.atomic_write_json(dead_marked / scratch.OWNER_MARKER_NAME, {"pid": dead})
    paths.atomic_write_json(live_marked / scratch.OWNER_MARKER_NAME, {"pid": live})

    recovery = scratch.scavenge_abandoned_roots(parent)

    assert sorted(recovery.removed) == sorted(
        [dead_marked.name, dead_named.name, trash.name]
    )
    assert sorted(entry.name for entry in parent.iterdir()) == sorted(
        [live_marked.name, live_named.name]
    )


def test_study_reports_recovered_abandoned_roots(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    parent = scratch.scratch_parent(root)
    parent.mkdir(parents=True, exist_ok=True)
    dead = _dead_pid()
    abandoned = parent / f"run-{dead}-deadbeef"
    abandoned.mkdir()
    (abandoned / "leftover.py").write_text("leftover = 1\n", encoding="utf-8")
    paths.atomic_write_json(abandoned / scratch.OWNER_MARKER_NAME, {"pid": dead})
    _patch_runners(monkeypatch, _killing_fake())

    study = _run_study(root)
    board = mutations.render_mutation_board(study)

    assert study.recovered_roots == (abandoned.name,)
    assert "recovered abandoned scratch roots" in board
    assert f"- {abandoned.name}" in board
    assert [entry.name for entry in parent.iterdir()] == []


def test_concurrent_live_run_root_survives_another_study(tmp_path, monkeypatch):
    root = _seed_project(tmp_path / "repo")
    parent = scratch.scratch_parent(root)
    parent.mkdir(parents=True, exist_ok=True)
    concurrent = parent / f"run-{os.getpid()}-feedface"
    concurrent.mkdir()
    (concurrent / "inflight.py").write_text("inflight = 1\n", encoding="utf-8")
    paths.atomic_write_json(
        concurrent / scratch.OWNER_MARKER_NAME, {"pid": os.getpid()}
    )
    _patch_runners(monkeypatch, _killing_fake())

    study = _run_study(root)

    assert study.recovered_roots == ()
    assert [entry.name for entry in parent.iterdir()] == [concurrent.name]
    assert (concurrent / "inflight.py").read_bytes() == b"inflight = 1\n"
