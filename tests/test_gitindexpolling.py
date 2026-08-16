"""Python-owned Git observation must not rewrite a sealed worktree index."""

from __future__ import annotations

import os
import socket
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

from spice.agent.supervisorwatch import worktree_dirty

INDEX_MTIME_BACKDATE_NS = 5_000_000_000
SUPERVISOR_PROBE_REPETITIONS = 10
COORDINATION_TIMEOUT_SECONDS = 10.0
COMMIT_TIMEOUT_SECONDS = 20.0


def _git(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo_root), *args],
        check=True,
        capture_output=True,
        text=True,
    )


def _index_path(repo_root: Path) -> Path:
    raw = _git(repo_root, "rev-parse", "--git-path", "index").stdout.strip()
    path = Path(raw)
    return path if path.is_absolute() else repo_root / path


def _index_generation(path: Path) -> tuple[bytes, int, int, int]:
    observed = path.stat()
    return (
        path.read_bytes(),
        observed.st_ino,
        observed.st_mtime_ns,
        observed.st_ctime_ns,
    )


def _install_seal_hook(repo_root: Path, socket_path: Path) -> None:
    hook = repo_root / ".git" / "hooks" / "pre-commit"
    hook.write_text(
        "\n".join(
            [
                f"#!{os.fsdecode(sys.executable)}",
                "import socket",
                "import sys",
                "from pathlib import Path",
                "",
                f"index_path = Path({str(_index_path(repo_root))!r})",
                "",
                "def generation():",
                "    observed = index_path.stat()",
                "    return (",
                "        index_path.read_bytes(),",
                "        observed.st_ino,",
                "        observed.st_mtime_ns,",
                "        observed.st_ctime_ns,",
                "    )",
                "",
                "before = generation()",
                "channel = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)",
                f"channel.connect({str(socket_path)!r})",
                'stream = channel.makefile("rwb", buffering=0)',
                'stream.write(b"ready\\n")',
                'if stream.readline() != b"poll-complete\\n":',
                "    raise SystemExit(92)",
                "after = generation()",
                'stream.write(b"stable\\n" if after == before else b"changed\\n")',
                'if stream.readline() != b"release\\n":',
                "    raise SystemExit(93)",
                "stream.close()",
                "channel.close()",
                "if after != before:",
                '    print("sealed Git index generation changed", file=sys.stderr)',
                "    raise SystemExit(91)",
                "",
            ]
        ),
        encoding="utf-8",
    )
    hook.chmod(hook.stat().st_mode | stat.S_IXUSR)


@pytest.mark.skipif(not hasattr(socket, "AF_UNIX"), reason="requires Unix sockets")
def test_supervisor_git_polling_preserves_sealed_index_and_commit(
    tmp_path, monkeypatch
):
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    _git(repo_root, "init", "-q", "-b", "main")
    _git(repo_root, "config", "user.email", "spice@example.test")
    _git(repo_root, "config", "user.name", "Spice Tests")
    tracked = repo_root / "tracked.txt"
    tracked.write_text("baseline\n", encoding="utf-8")
    _git(repo_root, "add", "tracked.txt")
    _git(repo_root, "commit", "-q", "-m", "baseline")
    tracked.write_text("staged change\n", encoding="utf-8")
    _git(repo_root, "add", "tracked.txt")
    index_path = _index_path(repo_root)
    monkeypatch.delenv("GIT_OPTIONAL_LOCKS", raising=False)

    with tempfile.TemporaryDirectory(prefix="spice-index-seal-") as coordination:
        socket_path = Path(coordination) / "hook.sock"
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.settimeout(COORDINATION_TIMEOUT_SECONDS)
            listener.bind(str(socket_path))
            listener.listen(1)
            _install_seal_hook(repo_root, socket_path)
            commit = subprocess.Popen(
                [
                    "git",
                    "-C",
                    str(repo_root),
                    "commit",
                    "-q",
                    "-m",
                    "sealed commit",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            connection, _ = listener.accept()
            connection.settimeout(COORDINATION_TIMEOUT_SECONDS)
            with connection, connection.makefile("rwb", buffering=0) as stream:
                assert stream.readline() == b"ready\n"
                sealed = _index_generation(index_path)
                tracked_stat = tracked.stat()
                os.utime(
                    tracked,
                    ns=(
                        tracked_stat.st_atime_ns,
                        max(
                            0,
                            tracked_stat.st_mtime_ns - INDEX_MTIME_BACKDATE_NS,
                        ),
                    ),
                )
                assert all(
                    worktree_dirty(repo_root)
                    for _ in range(SUPERVISOR_PROBE_REPETITIONS)
                )
                after_polling = _index_generation(index_path)
                stream.write(b"poll-complete\n")
                hook_verdict = stream.readline()
                stream.write(b"release\n")
            stdout, stderr = commit.communicate(timeout=COMMIT_TIMEOUT_SECONDS)

    assert after_polling == sealed
    assert hook_verdict == b"stable\n"
    assert commit.returncode == 0, (stdout, stderr)
    assert _git(repo_root, "log", "-1", "--format=%s").stdout.strip() == (
        "sealed commit"
    )
