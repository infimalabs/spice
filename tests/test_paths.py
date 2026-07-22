from __future__ import annotations

import errno
import json
import os
import stat
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

import spice.paths as paths
from spice.errors import SpiceError
from spice.paths import (
    atomic_write_json,
    atomic_write_text,
    git_common_dir,
    git_dir,
    shared_state_path,
    shared_state_root,
    worktree_state_path,
    worktree_state_root,
)

NEW_STATE_FILE_MODE = 0o600
EXISTING_STATE_FILE_MODE = 0o640


def test_atomic_write_text_creates_parent_and_round_trips_unicode(tmp_path):
    path = tmp_path / "nested" / "state.txt"

    written = atomic_write_text(path, "jalapeño 🌶️\n")

    assert written == path
    assert path.read_text(encoding="utf-8") == "jalapeño 🌶️\n"
    assert stat.S_IMODE(path.stat().st_mode) == NEW_STATE_FILE_MODE


def test_atomic_write_text_matching_utf8_bytes_preserve_inode(tmp_path):
    path = tmp_path / "state.txt"
    text = "first line\njalapeño 🌶️\n"

    atomic_write_text(path, text, write_if_changed=True)
    before = path.stat()
    atomic_write_text(path, text, write_if_changed=True)
    after = path.stat()

    assert path.read_bytes() == text.encode("utf-8")
    assert (after.st_ino, after.st_mtime_ns) == (
        before.st_ino,
        before.st_mtime_ns,
    )


def test_atomic_write_json_supports_pretty_compact_and_matching_content(tmp_path):
    pretty = tmp_path / "pretty.json"
    compact = tmp_path / "compact.json"
    payload = {"z": "jalapeño", "a": [1, 2]}

    atomic_write_json(pretty, payload)
    atomic_write_json(compact, payload, compact=True, sort_keys=True)
    compact_before = compact.stat()
    atomic_write_json(
        compact,
        payload,
        compact=True,
        sort_keys=True,
        write_if_changed=True,
    )
    compact_after = compact.stat()

    assert json.loads(pretty.read_text(encoding="utf-8")) == payload
    assert pretty.read_text(encoding="utf-8").startswith('{\n  "a"')
    assert compact.read_text(encoding="utf-8") == ('{"a":[1,2],"z":"jalape\\u00f1o"}\n')
    assert (compact_after.st_ino, compact_after.st_mtime_ns) == (
        compact_before.st_ino,
        compact_before.st_mtime_ns,
    )


def test_atomic_write_text_preserves_existing_permissions(tmp_path):
    path = tmp_path / "state.txt"
    path.write_text("before\n", encoding="utf-8")
    path.chmod(EXISTING_STATE_FILE_MODE)

    atomic_write_text(path, "after\n")

    assert path.read_text(encoding="utf-8") == "after\n"
    assert stat.S_IMODE(path.stat().st_mode) == EXISTING_STATE_FILE_MODE


def test_atomic_write_text_fsyncs_file_then_replaces_then_fsyncs_directory(
    tmp_path, monkeypatch
):
    events: list[str] = []
    real_replace = os.replace

    def record_fsync(_descriptor):
        events.append("fsync")

    def record_replace(source, target):
        events.append("replace")
        real_replace(source, target)

    monkeypatch.setattr(paths.os, "fsync", record_fsync)
    monkeypatch.setattr(paths.os, "replace", record_replace)

    atomic_write_text(tmp_path / "state.txt", "complete\n")

    assert events == ["fsync", "replace", "fsync"]


def test_atomic_write_text_cleans_its_temp_and_preserves_target_on_replace_error(
    tmp_path, monkeypatch
):
    path = tmp_path / "state.txt"
    path.write_text("original\n", encoding="utf-8")

    def reject_replace(_source, _target):
        raise PermissionError("replace denied")

    monkeypatch.setattr(paths.os, "replace", reject_replace)

    with pytest.raises(PermissionError, match="replace denied"):
        atomic_write_text(path, "replacement\n")

    assert path.read_text(encoding="utf-8") == "original\n"
    assert list(tmp_path.glob(".state.txt.*.tmp")) == []


def test_atomic_write_text_surfaces_directory_fsync_failure_after_replacement(
    tmp_path, monkeypatch
):
    path = tmp_path / "state.txt"
    calls = 0
    real_fsync = os.fsync

    def fail_directory_fsync(descriptor):
        nonlocal calls
        calls += 1
        if calls == 2:
            raise OSError(errno.EIO, "directory fsync failed")
        real_fsync(descriptor)

    monkeypatch.setattr(paths.os, "fsync", fail_directory_fsync)

    with pytest.raises(OSError, match="directory fsync failed"):
        atomic_write_text(path, "complete\n")

    assert path.read_text(encoding="utf-8") == "complete\n"
    assert list(tmp_path.glob(".state.txt.*.tmp")) == []


def test_atomic_write_text_same_process_writers_publish_whole_values(tmp_path):
    path = tmp_path / "state.txt"
    values = [f"writer-{index}:" + str(index) * 8192 for index in range(8)]
    barrier = threading.Barrier(len(values))

    def write(value: str) -> None:
        barrier.wait()
        atomic_write_text(path, value)

    with ThreadPoolExecutor(max_workers=len(values)) as pool:
        list(pool.map(write, values))

    assert path.read_text(encoding="utf-8") in values
    assert list(tmp_path.glob(".state.txt.*.tmp")) == []


def test_atomic_write_text_cross_process_writers_publish_whole_values(tmp_path):
    path = tmp_path / "state.txt"
    values = [f"process-{index}:" + str(index) * 8192 for index in range(6)]
    script = (
        "import sys\n"
        "from pathlib import Path\n"
        "from spice.paths import atomic_write_text\n"
        "atomic_write_text(Path(sys.argv[1]), sys.argv[2])\n"
    )
    processes = [
        subprocess.Popen([sys.executable, "-c", script, str(path), value])
        for value in values
    ]

    return_codes = [process.wait(timeout=20) for process in processes]

    assert return_codes == [0] * len(processes)
    assert path.read_text(encoding="utf-8") in values
    assert list(tmp_path.glob(".state.txt.*.tmp")) == []


def test_canonical_state_roots_preserve_shared_and_linked_worktree_ownership(tmp_path):
    repo = _repo_with_linked_worktree(tmp_path)
    linked = tmp_path / "linked"

    assert shared_state_root(repo) == git_common_dir(repo) / ".spice"
    assert shared_state_root(linked) == shared_state_root(repo)
    assert worktree_state_root(repo) == git_dir(repo) / ".spice"
    assert worktree_state_root(linked) == git_dir(linked) / ".spice"
    assert shared_state_path(linked, "data/state.sqlite3") == (
        git_common_dir(repo) / ".spice" / "data" / "state.sqlite3"
    )
    assert worktree_state_path(linked, "agents/thread/state.json") == (
        git_dir(linked) / ".spice" / "agents" / "thread" / "state.json"
    )


def test_canonical_state_roots_support_bare_common_dir_ending_dot_git(tmp_path):
    source = _initialized_repo(tmp_path / "source")
    bare = tmp_path / "product.git"
    linked = tmp_path / "bare-linked"
    subprocess.run(["git", "clone", "-q", "--bare", str(source), str(bare)], check=True)
    subprocess.run(
        [
            "git",
            "--git-dir",
            str(bare),
            "worktree",
            "add",
            "-q",
            "--detach",
            str(linked),
            "HEAD",
        ],
        check=True,
    )

    assert git_common_dir(linked) == bare.resolve()
    assert shared_state_root(linked) == bare.resolve() / ".spice"
    assert worktree_state_root(linked) == git_dir(linked) / ".spice"
    assert shared_state_path(linked, "task/board.sqlite3") == (
        bare.resolve() / ".spice" / "task" / "board.sqlite3"
    )
    assert worktree_state_path(linked, "agents/thread/state.json") == (
        git_dir(linked) / ".spice" / "agents" / "thread" / "state.json"
    )


def _refuse_to_launch_git(monkeypatch) -> None:
    def unlaunchable(_command, **_kwargs):
        raise OSError(errno.ENOENT, "No such file or directory")

    monkeypatch.setattr(paths, "run_git_command", unlaunchable)


def _report_no_worktree(monkeypatch) -> None:
    def outside_any_worktree(command, **_kwargs):
        raise subprocess.CalledProcessError(
            128,
            list(command),
            stderr="fatal: not a git repository (or any parent): .git",
        )

    monkeypatch.setattr(paths, "run_git_command", outside_any_worktree)


def _report_failed_git(monkeypatch) -> None:
    def failed_git(command, **_kwargs):
        raise subprocess.CalledProcessError(
            128,
            list(command),
            stderr="fatal: Unable to create 'index.lock': File exists.",
        )

    monkeypatch.setattr(paths, "run_git_command", failed_git)


def test_repo_root_from_cwd_separates_a_launch_failure_from_an_absent_worktree(
    tmp_path, monkeypatch
):
    """A git that never started said nothing about whether this is a worktree."""
    _report_no_worktree(monkeypatch)
    absent = paths.repo_root_from_cwd(tmp_path)
    _refuse_to_launch_git(monkeypatch)

    with pytest.raises(SpiceError) as unlaunchable:
        paths.repo_root_from_cwd(tmp_path)

    message = str(unlaunchable.value)
    assert absent is None
    assert "git command could not be launched" in message
    assert str(tmp_path) in message
    assert "No such file or directory" in message


def test_repo_root_from_cwd_names_a_failed_git_run(tmp_path, monkeypatch):
    _report_failed_git(monkeypatch)

    with pytest.raises(SpiceError) as failed:
        paths.repo_root_from_cwd(tmp_path)

    message = str(failed.value)
    assert "git command failed" in message
    assert "--show-toplevel" in message
    assert "exit 128" in message
    assert "index.lock" in message


def test_require_repo_root_names_the_launch_failure_rather_than_the_tree(
    tmp_path, monkeypatch
):
    """The two conditions reach the caller as two different sentences."""
    _report_no_worktree(monkeypatch)
    with pytest.raises(SpiceError) as absent:
        paths.require_repo_root(tmp_path)
    _refuse_to_launch_git(monkeypatch)

    with pytest.raises(SpiceError) as unlaunchable:
        paths.require_repo_root(tmp_path)

    absent_message = str(absent.value)
    launch_message = str(unlaunchable.value)
    assert absent_message == "not inside a git worktree"
    assert "git command could not be launched" in launch_message


def test_init_repo_root_surfaces_a_launch_failure_before_walking_for_markers(
    tmp_path, monkeypatch
):
    """The marker walk answers for a bare common dir, never for a broken git."""
    from spice.hooks import cli as hookcli

    (tmp_path / ".git").write_text(f"gitdir: {tmp_path / 'common'}\n", encoding="utf-8")
    _report_no_worktree(monkeypatch)
    walked = hookcli.init_repo_root(tmp_path)
    _refuse_to_launch_git(monkeypatch)

    with pytest.raises(SpiceError) as unlaunchable:
        hookcli.init_repo_root(tmp_path)

    assert walked == tmp_path.resolve()
    assert "git command could not be launched" in str(unlaunchable.value)


def test_git_common_dir_tells_an_absent_repository_from_a_failed_git_run(tmp_path):
    """Git exits 128 for both, so only its own words can carry the difference."""
    plain = tmp_path / "plain"
    plain.mkdir()
    missing = tmp_path / "missing"

    with pytest.raises(SpiceError) as absent:
        git_common_dir(plain)
    with pytest.raises(SpiceError) as failed:
        git_common_dir(missing)

    absent_message = str(absent.value)
    failed_message = str(failed.value)
    assert absent_message == "not inside a git worktree"
    assert "git command failed" in failed_message
    assert "--git-common-dir" in failed_message
    assert str(missing) in failed_message
    assert "No such file or directory" in failed_message


def test_git_dir_resolvers_carry_their_own_argv_and_name_a_launch_failure(
    tmp_path, monkeypatch
):
    """Each resolver reports the argv it ran; neither blames the tree for the host."""
    missing = tmp_path / "missing"

    with pytest.raises(SpiceError) as common_failure:
        git_common_dir(missing)
    with pytest.raises(SpiceError) as worktree_failure:
        git_dir(missing)
    _refuse_to_launch_git(monkeypatch)

    with pytest.raises(SpiceError) as unlaunchable:
        git_dir(missing)

    common_message = str(common_failure.value)
    worktree_message = str(worktree_failure.value)
    launch_message = str(unlaunchable.value)
    assert "--git-common-dir" in common_message
    assert "--git-dir" in worktree_message
    assert "git command could not be launched" in launch_message


def _repo_with_linked_worktree(tmp_path: Path) -> Path:
    repo = _initialized_repo(tmp_path / "repo")
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "linked", str(tmp_path / "linked")],
        cwd=repo,
        check=True,
    )
    return repo


def _initialized_repo(repo: Path) -> Path:
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"], cwd=repo, check=True
    )
    subprocess.run(["git", "config", "user.name", "Test"], cwd=repo, check=True)
    (repo / "README.md").write_text("test\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "init"], cwd=repo, check=True)
    return repo
