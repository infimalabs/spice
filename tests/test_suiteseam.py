"""The whole-suite gate on task landings that reach the whole suite."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from spice.cli import entry as cli_entry
from spice.errors import SpiceError
from spice.studies.suiteseam import (
    SUITE_SEAM_PACKAGE,
    UNTOUCHED_REASON,
    run_suite_seam_gate,
    suite_seam_plan,
    suite_seam_reach,
)
from spice.tasks.git import boundaries, plumbing
from tests.test_taskgitsync import _configure_git_identity, _git, _init_repo, _run

SEAM = "core/tw.py"
UNCHANGED_SEAM_SOURCE = (
    "UDA_FIELDS = ()\n"
    "\n"
    "\n"
    "def taskwarrior_argv(*arguments):\n"
    '    return ("task", *UDA_FIELDS, *arguments)\n'
)
# The CLAIMS-1kG4mBMp change: bind the UDA schema to every invocation.
SCHEMA_BOUND_SEAM_SOURCE = UNCHANGED_SEAM_SOURCE.replace(
    "UDA_FIELDS = ()", 'UDA_FIELDS = ("rc.uda.phase.type=string",)'
)

# What the lane ran: the tests that name the module it changed. They stay green
# through the change, which is exactly why the subset looked like enough.
NEAR_TESTS = (
    "from core.tw import taskwarrior_argv\n"
    "\n"
    "\n"
    "def test_argv_ends_with_the_caller_arguments():\n"
    '    assert taskwarrior_argv("export")[-1] == "export"\n'
)
# What the lane could not have run: a timeout diagnostic asserting the exact
# command line, which a peer lands on the baseline while the lane is working.
FAR_TESTS = (
    "from core.tw import taskwarrior_argv\n"
    "\n"
    "\n"
    "def test_timeout_diagnostic_reports_the_exact_command_line():\n"
    '    assert " ".join(taskwarrior_argv("export")) == "task export"\n'
)


def _write_seam_project(root: Path, source: str, *, seconds: int = 3) -> None:
    (root / "spice.toml").write_text(
        "[policy.suite_seam]\n"
        f"seconds = {seconds}\n"
        f'run = ["{sys.executable}", "-m", "pytest", "-q", "tests"]\n'
        f'paths = ["{SEAM}"]\n',
        encoding="utf-8",
    )
    (root / "core").mkdir(exist_ok=True)
    (root / "core" / "tw.py").write_text(source, encoding="utf-8")
    (root / "core" / "notes.py").write_text("SUMMARY = 'notes'\n", encoding="utf-8")
    (root / "tests").mkdir(exist_ok=True)
    (root / "tests" / "test_near.py").write_text(NEAR_TESTS, encoding="utf-8")


def _run_suite(root: Path) -> str:
    completed = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "tests"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    return completed.stdout


def _lane_on_a_baseline(tmp_path: Path) -> tuple[Path, Path]:
    """A lane whose pinned baseline holds the seam and the tests that name it."""
    remote = tmp_path / "remote.git"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    repo = _init_repo(tmp_path / "agent")
    _write_seam_project(repo, UNCHANGED_SEAM_SOURCE)
    _run(repo, "git", "add", "-A")
    _run(repo, "git", "commit", "-m", "seam and the tests that name it")
    _run(repo, "git", "remote", "add", "origin", str(remote))
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    return repo, remote


def _peer_lands_the_far_test(tmp_path: Path, remote: Path) -> str:
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "tests" / "test_far.py").write_text(FAR_TESTS, encoding="utf-8")
    _run(peer, "git", "add", "-A")
    _run(peer, "git", "commit", "-m", "assert the exact taskwarrior command line")
    _run(peer, "git", "push", "origin", "main")
    return _git(peer, "rev-parse", "HEAD")


def test_a_landing_that_reddens_the_merged_tree_never_reaches_the_branch(tmp_path):
    """The incident, exactly: every tree is green and the branch still goes red.

    The lane's whole suite passes on the lane's own baseline, so no gate the
    lane could run on its own tree would have refused this. The peer's suite
    passes on the peer's tree too. Only the merged tree is red, and the merged
    tree exists for the first time inside the landing.
    """
    repo, remote = _lane_on_a_baseline(tmp_path)
    (repo / "core" / "tw.py").write_text(SCHEMA_BOUND_SEAM_SOURCE, encoding="utf-8")
    _run(repo, "git", "add", "-A")
    _run(repo, "git", "commit", "-m", "bind the UDA schema to every invocation")
    lane_suite = _run_suite(repo)
    upstream_head = _peer_lands_the_far_test(tmp_path, remote)

    with pytest.raises(SpiceError) as refused:
        boundaries.integrate_and_publish("TASK-1kG4y9Pn", repo_root=repo)

    message = str(refused.value)
    assert "1 passed" in lane_suite
    assert "refusing to publish" in message
    assert f"{SEAM} is a declared suite seam" in message
    assert "test_far.py" in message
    assert 'spice task done TASK-1kG4y9Pn --validation "..."' in message
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        upstream_head
    )


def test_a_race_that_reddens_the_retried_tree_never_reaches_the_branch(
    tmp_path, monkeypatch
):
    """The gate runs again on the tree the race built, not the one it cleared.

    The first gate sees a merged tree that is green and clears the landing to
    push. The push then loses a race, and the retry merges the peer work that
    reddens the seam change, so the tree about to be pushed is one no gate has
    seen. Only the gate inside the retry stands between it and the branch.
    """
    repo, remote = _lane_on_a_baseline(tmp_path)
    (repo / "core" / "tw.py").write_text(SCHEMA_BOUND_SEAM_SOURCE, encoding="utf-8")
    _run(repo, "git", "add", "-A")
    _run(repo, "git", "commit", "-m", "bind the UDA schema to every invocation")
    real_run = plumbing.run
    pushes = 0
    raced_upstream = ""

    def racing_run(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        nonlocal pushes, raced_upstream
        if args and args[0] == "push" and repo_root == repo:
            pushes += 1
            if pushes == 1:
                raced_upstream = _peer_lands_the_far_test(tmp_path, remote)
        return real_run(repo_root, *args)

    monkeypatch.setattr(plumbing, "run", racing_run)

    with pytest.raises(SpiceError) as refused:
        boundaries.integrate_and_publish("TASK-1kG5WJNY", repo_root=repo)

    message = str(refused.value)
    assert pushes == 1
    assert "refusing to publish" in message
    assert f"{SEAM} is a declared suite seam" in message
    assert "test_far.py" in message
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == (
        raced_upstream
    )


def test_a_green_merged_tree_publishes_the_landing(tmp_path):
    """The same seam landing over a peer whose work keeps the suite green."""
    repo, remote = _lane_on_a_baseline(tmp_path)
    (repo / "core" / "notes.py").write_text("SUMMARY = 'lane'\n", encoding="utf-8")
    (repo / "core" / "tw.py").write_text(SCHEMA_BOUND_SEAM_SOURCE, encoding="utf-8")
    _run(repo, "git", "add", "-A")
    _run(repo, "git", "commit", "-m", "bind the UDA schema to every invocation")
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _configure_git_identity(peer)
    (peer / "baseline.txt").write_text("peer work\n", encoding="utf-8")
    _run(peer, "git", "add", "-A")
    _run(peer, "git", "commit", "-m", "peer work")
    _run(peer, "git", "push", "origin", "main")

    result = boundaries.integrate_and_publish("TASK-1kG4y9Pq", repo_root=repo)

    merge_head = _git(repo, "rev-parse", "HEAD")
    assert result.uda_args
    assert _git(repo, "ls-remote", "origin", "refs/heads/main").split()[0] == merge_head
    assert "1 passed" in _run_suite(repo)


def test_a_landing_off_the_seam_costs_the_task_nothing(tmp_path, monkeypatch):
    """An ordinary landing keeps its own wall clock beside the very same suite."""
    root = tmp_path / "repo"
    root.mkdir()
    _write_seam_project(root, SCHEMA_BOUND_SEAM_SOURCE)
    monkeypatch.chdir(root)

    routine = run_suite_seam_gate(root, [Path("core/notes.py")], label="TASK-1kG4y9Pr")
    seam = suite_seam_plan(root, [Path(SEAM)])

    assert routine.plan.reason == UNTOUCHED_REASON
    assert routine.elapsed_seconds == 0.0
    assert routine.plan.argv == ()
    assert seam.argv != routine.plan.argv


def test_the_suite_command_keeps_repeated_argv_tokens(tmp_path):
    """A deduping reader would silently drop the second `-p`, so this one does not."""
    root = tmp_path / "repo"
    root.mkdir()
    _write_seam_project(root, UNCHANGED_SEAM_SOURCE)
    (root / "spice.toml").write_text(
        "[policy.suite_seam]\n"
        'run = ["pytest", "-p", "no:randomly", "-p", "no:cacheprovider"]\n'
        f'paths = ["{SEAM}"]\n',
        encoding="utf-8",
    )

    plan = suite_seam_plan(root, [Path(SEAM)])

    assert plan.argv == ("pytest", "-p", "no:randomly", "-p", "no:cacheprovider")


def test_the_suite_runs_as_its_own_top_level_command(tmp_path, monkeypatch):
    """The re-exec marker must not follow the gate into the suite command.

    Inheriting it makes the suite skip its own re-exec and start from whatever
    interpreter owns the ambient entry point, which refuses to run at all.
    """
    root = tmp_path / "repo"
    root.mkdir()
    _write_seam_project(root, UNCHANGED_SEAM_SOURCE)
    env_name = cli_entry.SELFEXEC_ENV
    probe = (
        "import os;"
        # The child must read the marker to prove the gate cleared it.
        f"print('marker=' + os.environ.get({env_name!r}, 'cleared'));"  # env-policy: allow
        "raise SystemExit(1)"
    )
    (root / "spice.toml").write_text(
        "[policy.suite_seam]\n"
        f'run = ["{sys.executable}", "-c", "{probe}"]\n'
        f'paths = ["{SEAM}"]\n',
        encoding="utf-8",
    )
    monkeypatch.setenv(cli_entry.SELFEXEC_ENV, str(root))

    with pytest.raises(SpiceError) as refused:
        run_suite_seam_gate(root, [Path(SEAM)], label="TASK-1kG4y9Ps")

    assert "marker=cleared" in str(refused.value)
    assert os.environ[cli_entry.SELFEXEC_ENV] == str(root)  # env-policy: allow


def test_this_repository_declares_exactly_the_widest_reaching_modules():
    """The declared list is the top of the measured ordering, not a taste call.

    The comment above the declaration says membership is measured, so this is
    where that claim is settled on the live tree. Ranking every package module
    by the share of the suite that reaches it must place the declared paths in
    the leading slots, and the boundary between the last declared module and
    the first undeclared one must be a strict break -- a tie across it means
    the list no longer names a distinguishable band and the next path to add
    is a coin flip.
    """
    repo_root = Path(__file__).resolve().parents[1]

    report = suite_seam_reach(repo_root, SUITE_SEAM_PACKAGE)

    declared = report.declared
    assert report.ranked[: len(declared)] == declared
    assert report.declared_floor > report.widest_undeclared.reached_by
