"""Configured resource locks and shard pools."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
from pathlib import Path

from spice import resourcelocks
from spice.cli.parser import build_parser
from spice.locking import exclusive_lock, lock_fd_exclusive, unlock_fd
from spice.resourcelocks import configured_lock_settings, handle_lock

LOCK_CONTENTION_CODE = 71
CHOSEN_SHARD_CONTENTION_CODE = 72
POOL_EXHAUSTION_CODE = 73
OVERRIDE_LOCK_CONTENTION_CODE = 79
THREAD_EVENT_TIMEOUT_SECONDS = 5


def _repo_with_locks(tmp_path: Path, body: str) -> Path:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    (tmp_path / "spice.toml").write_text(f"[locks]\n{body}\n", encoding="utf-8")
    return tmp_path


def _parse_lock_args(argv: list[str]) -> argparse.Namespace:
    return build_parser().parse_args(["lock", *argv])


def test_lock_parser_exposes_run_and_status_commands():
    parser = build_parser()
    args = parser.parse_args(["lock", "status"])
    choices = parser._subparsers._group_actions[0].choices
    help_text = choices["lock"].format_help()
    run_help_text = (
        choices["lock"]._subparsers._group_actions[0].choices["run"].format_help()
    )

    assert args.lock_action == "status"
    assert args.func == handle_lock
    assert "run" in help_text
    assert "status" in help_text
    assert "Hold configured resource locks" in help_text
    assert "COMMAND" in run_help_text
    assert "Child command argv to run while the lock is held." in run_help_text


def test_packaged_lock_state_root_reaches_default_resource_paths(tmp_path):
    repo = _repo_with_locks(
        tmp_path,
        'state_root = "configured-locks"\n'
        "[locks.named.editor]\n"
        "[locks.pools.browser]\n"
        "shards = 3\n",
    )

    settings = configured_lock_settings(repo)

    assert settings.locks["editor"].path == repo / "configured-locks" / "editor.lock"
    assert settings.pools["browser"].directory == (
        repo / "configured-locks" / "browser"
    )
    assert settings.pools["browser"].shards == 3


def test_lock_run_holds_configured_named_lock_for_child_lifetime(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        '[locks.named.editor]\npath = "locks/editor.lock"\n',
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    lock_path = repo / "locks" / "editor.lock"
    child = (
        "import sys\n"
        "from pathlib import Path\n"
        "from spice.locking import FileLockUnavailable, exclusive_lock\n"
        "try:\n"
        "    with exclusive_lock(Path(sys.argv[1]), blocking=False):\n"
        "        raise SystemExit(88)\n"
        "except FileLockUnavailable:\n"
        "    raise SystemExit(0)\n"
    )

    result = handle_lock(
        _parse_lock_args(
            [
                "run",
                "editor",
                "--",
                sys.executable,
                "-c",
                child,
                str(lock_path),
            ]
        )
    )

    assert result == 0
    with exclusive_lock(lock_path, blocking=False):
        reacquired = True
    assert reacquired


def test_held_named_lock_names_holder_on_stderr(tmp_path, monkeypatch, capsys):
    repo = _repo_with_locks(
        tmp_path,
        f"lock_contention_exit_code = {LOCK_CONTENTION_CODE}\n"
        '[locks.named.editor]\npath = "locks/editor.lock"\n',
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    lock_path = repo / "locks" / "editor.lock"
    holder = {
        "pid": os.getpid(),
        "cwd": str(tmp_path),
        "started_at": "2026-07-28T00:00:00Z",
    }

    with resourcelocks._metadata_lock(lock_path, holder):
        result = handle_lock(
            _parse_lock_args(
                [
                    "run",
                    "editor",
                    "--",
                    sys.executable,
                    "-c",
                    "raise SystemExit(12)",
                ]
            )
        )

    assert result == LOCK_CONTENTION_CODE
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "lock 'editor' is already held" in captured.err
    assert f"pid={holder['pid']}" in captured.err
    assert f"cwd={holder['cwd']}" in captured.err
    assert f"started_at={holder['started_at']}" in captured.err


def test_status_observation_window_does_not_contend_with_a_run(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        '[locks.named.editor]\npath = "locks/editor.lock"\n',
    )
    settings = resourcelocks.configured_lock_settings(repo)
    lock_path = settings.locks["editor"].path
    lock_path.parent.mkdir(parents=True)
    lock_path.touch()
    observation_started = threading.Event()
    release_observation = threading.Event()
    status_records: list[list[dict[str, object]]] = []
    status_errors: list[BaseException] = []
    read_metadata = resourcelocks._read_lock_metadata

    def hold_observation(path):
        text = read_metadata(path)
        observation_started.set()
        assert release_observation.wait(THREAD_EVENT_TIMEOUT_SECONDS)
        return text

    def observe_status():
        try:
            status_records.append(resourcelocks.lock_status_records(settings))
        except BaseException as exc:
            status_errors.append(exc)

    monkeypatch.setattr(resourcelocks, "_read_lock_metadata", hold_observation)
    observer = threading.Thread(target=observe_status)
    observer.start()
    assert observation_started.wait(THREAD_EVENT_TIMEOUT_SECONDS)
    try:
        result = resourcelocks._run_named_lock(
            settings.locks["editor"],
            [sys.executable, "-c", "raise SystemExit(0)"],
        )
    finally:
        release_observation.set()
        observer.join(THREAD_EVENT_TIMEOUT_SECONDS)

    assert not observer.is_alive()
    assert status_errors == []
    assert result == 0
    assert status_records[0][0]["state"] == "free"


def test_lock_run_flags_override_configured_path_and_contention_code(
    tmp_path, monkeypatch
):
    repo = _repo_with_locks(
        tmp_path,
        "[locks.named.editor]\n"
        'path = "locks/configured.lock"\n'
        f"contention_exit_code = {LOCK_CONTENTION_CODE}\n",
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    override_path = repo / "locks" / "override.lock"

    with exclusive_lock(override_path, blocking=True):
        result = handle_lock(
            _parse_lock_args(
                [
                    "run",
                    "editor",
                    "--path",
                    "locks/override.lock",
                    "--lock-contention-exit-code",
                    str(OVERRIDE_LOCK_CONTENTION_CODE),
                    "--",
                    sys.executable,
                    "-c",
                    "raise SystemExit(12)",
                ]
            )
        )

    assert result == OVERRIDE_LOCK_CONTENTION_CODE


def test_pool_chosen_shard_contention_names_holder_on_stderr(
    tmp_path, monkeypatch, capsys
):
    repo = _repo_with_locks(
        tmp_path,
        (
            f"chosen_shard_contention_exit_code = {CHOSEN_SHARD_CONTENTION_CODE}\n"
            "[locks.pools.android]\n"
            'directory = "locks/android"\n'
            "shards = 2\n"
        ),
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    shard_path = repo / "locks" / "android" / "1.lock"
    holder = {
        "pid": os.getpid(),
        "cwd": str(tmp_path),
        "started_at": "2026-07-28T00:00:00Z",
    }

    with resourcelocks._metadata_lock(shard_path, holder):
        result = handle_lock(
            _parse_lock_args(
                [
                    "run",
                    "android",
                    "--pool",
                    "--shard",
                    "1",
                    "--",
                    sys.executable,
                    "-c",
                    "raise SystemExit(12)",
                ]
            )
        )

    assert result == CHOSEN_SHARD_CONTENTION_CODE
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "pool 'android' shard 1 is already held" in captured.err
    assert f"holder pid={holder['pid']}" in captured.err


def test_pool_exhaustion_names_each_holder_on_stderr(tmp_path, monkeypatch, capsys):
    repo = _repo_with_locks(
        tmp_path,
        (
            f"pool_exhaustion_exit_code = {POOL_EXHAUSTION_CODE}\n"
            "[locks.pools.android]\n"
            'directory = "locks/android"\n'
            "shards = 2\n"
        ),
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    shard_zero = repo / "locks" / "android" / "0.lock"
    shard_one = repo / "locks" / "android" / "1.lock"
    holder = {
        "pid": os.getpid(),
        "cwd": str(tmp_path),
        "started_at": "2026-07-28T00:00:00Z",
    }

    with resourcelocks._metadata_lock(shard_zero, holder):
        with resourcelocks._metadata_lock(shard_one, holder):
            result = handle_lock(
                _parse_lock_args(
                    [
                        "run",
                        "android",
                        "--pool",
                        "--",
                        sys.executable,
                        "-c",
                        "raise SystemExit(12)",
                    ]
                )
            )

    assert result == POOL_EXHAUSTION_CODE
    captured = capsys.readouterr()
    assert captured.out == ""
    assert "pool 'android' has no free shards" in captured.err
    assert f"shard 0 holder pid={holder['pid']}" in captured.err
    assert f"shard 1 holder pid={holder['pid']}" in captured.err


def test_pool_shard_count_flag_extends_configured_pool(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        '[locks.pools.android]\ndirectory = "locks/android"\nshards = 1\n',
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    shard_zero = repo / "locks" / "android" / "0.lock"

    with exclusive_lock(shard_zero, blocking=True):
        result = handle_lock(
            _parse_lock_args(
                [
                    "run",
                    "android",
                    "--pool",
                    "--shards",
                    "2",
                    "--",
                    sys.executable,
                    "-c",
                    "raise SystemExit(0)",
                ]
            )
        )

    assert result == 0


def test_lock_status_json_surfaces_holder_metadata(tmp_path, monkeypatch, capsys):
    repo = _repo_with_locks(
        tmp_path,
        '[locks.named.editor]\npath = "locks/editor.lock"\n',
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    lock_path = repo / "locks" / "editor.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    holder = {
        "pid": os.getpid(),
        "cwd": str(tmp_path),
        "started_at": "2026-07-09T00:00:00Z",
    }
    handle = lock_path.open("a+")
    try:
        lock_fd_exclusive(handle.fileno(), blocking=True)
        handle.seek(0)
        handle.truncate()
        handle.write(json.dumps(holder))
        handle.flush()

        result = handle_lock(_parse_lock_args(["status", "--json"]))
    finally:
        unlock_fd(handle.fileno())
        handle.close()

    assert result == 0
    assert json.loads(capsys.readouterr().out) == [
        {
            "holder": holder,
            "kind": "lock",
            "name": "editor",
            "path": str(lock_path),
            "state": "held",
        }
    ]


def test_lock_status_marks_every_nonempty_malformed_metadata_unknown(tmp_path):
    states = []
    for index, payload in enumerate((b"{malformed", b" \n", b"\xff")):
        lock_path = tmp_path / f"{index}.lock"
        lock_path.write_bytes(payload)
        states.append(resourcelocks._lock_state(lock_path))

    assert states == [("unknown", None)] * 3


def test_config_reference_documents_resource_locks():
    reference = Path("docs/config/reference.md").read_text(encoding="utf-8")

    assert "## `[locks]`" in reference
    assert "spice lock run editor -- project-tool edit" in reference
    assert "chosen_shard_contention_exit_code" in reference
    assert "pool_exhaustion_exit_code" in reference
