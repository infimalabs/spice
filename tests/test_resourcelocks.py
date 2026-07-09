"""Configured resource locks and shard pools."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

from spice.cli.parser import build_parser
from spice.locking import exclusive_lock, lock_fd_exclusive, unlock_fd
from spice.resourcelocks import handle_lock

LOCK_CONTENTION_CODE = 71
CHOSEN_SHARD_CONTENTION_CODE = 72
POOL_EXHAUSTION_CODE = 73
OVERRIDE_LOCK_CONTENTION_CODE = 79


def _repo_with_locks(tmp_path: Path, body: str) -> Path:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    (tmp_path / "pyproject.toml").write_text(
        f"[tool.spice.locks]\n{body}\n", encoding="utf-8"
    )
    return tmp_path


def _parse_lock_args(argv: list[str]) -> argparse.Namespace:
    return build_parser().parse_args(["lock", *argv])


def test_lock_parser_exposes_run_and_status_commands():
    parser = build_parser()
    args = parser.parse_args(["lock", "status"])
    choices = parser._subparsers._group_actions[0].choices
    help_text = choices["lock"].format_help()

    assert args.lock_action == "status"
    assert args.func == handle_lock
    assert "run" in help_text
    assert "status" in help_text
    assert "Hold configured resource locks" in help_text


def test_lock_run_holds_configured_named_lock_for_child_lifetime(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        '[tool.spice.locks.named.editor]\npath = "locks/editor.lock"\n',
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


def test_held_named_lock_returns_configured_contention_code(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        f"lock_contention_exit_code = {LOCK_CONTENTION_CODE}\n"
        '[tool.spice.locks.named.editor]\npath = "locks/editor.lock"\n',
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    lock_path = repo / "locks" / "editor.lock"

    with exclusive_lock(lock_path, blocking=True):
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


def test_lock_run_flags_override_configured_path_and_contention_code(
    tmp_path, monkeypatch
):
    repo = _repo_with_locks(
        tmp_path,
        "[tool.spice.locks.named.editor]\n"
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


def test_pool_chosen_shard_contention_uses_configured_code(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        (
            f"chosen_shard_contention_exit_code = {CHOSEN_SHARD_CONTENTION_CODE}\n"
            "[tool.spice.locks.pools.android]\n"
            'directory = "locks/android"\n'
            "shards = 2\n"
        ),
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    shard_path = repo / "locks" / "android" / "1.lock"

    with exclusive_lock(shard_path, blocking=True):
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


def test_pool_exhaustion_uses_configured_code(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        (
            f"pool_exhaustion_exit_code = {POOL_EXHAUSTION_CODE}\n"
            "[tool.spice.locks.pools.android]\n"
            'directory = "locks/android"\n'
            "shards = 2\n"
        ),
    )
    monkeypatch.setattr("spice.resourcelocks.require_repo_root", lambda: repo)
    shard_zero = repo / "locks" / "android" / "0.lock"
    shard_one = repo / "locks" / "android" / "1.lock"

    with exclusive_lock(shard_zero, blocking=True):
        with exclusive_lock(shard_one, blocking=True):
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


def test_pool_shard_count_flag_extends_configured_pool(tmp_path, monkeypatch):
    repo = _repo_with_locks(
        tmp_path,
        '[tool.spice.locks.pools.android]\ndirectory = "locks/android"\nshards = 1\n',
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
        '[tool.spice.locks.named.editor]\npath = "locks/editor.lock"\n',
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


def test_config_reference_documents_resource_locks():
    reference = Path("docs/config/reference.md").read_text(encoding="utf-8")

    assert "## `[tool.spice.locks]`" in reference
    assert "spice lock run editor -- project-tool edit" in reference
    assert "chosen_shard_contention_exit_code" in reference
    assert "pool_exhaustion_exit_code" in reference
