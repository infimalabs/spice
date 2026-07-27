"""Process proofs for post-integration task phase continuation."""

from __future__ import annotations

import importlib
import io
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.tasks import config, phasecontinuation


def _run(repo: Path, *args: str) -> str:
    result = subprocess.run(
        list(args),
        cwd=repo,
        capture_output=True,
        check=True,
        text=True,
    )
    return result.stdout.strip()


def _commit(repo: Path, message: str) -> str:
    _run(repo, "git", "add", ".")
    _run(repo, "git", "commit", "-m", message)
    return _run(repo, "git", "rev-parse", "HEAD")


def test_unchanged_checkout_uses_the_same_continuation_without_a_child(
    tmp_path, monkeypatch
):
    requests: list[object] = []
    monkeypatch.setattr(
        phasecontinuation,
        "_dispatch",
        lambda request: requests.append(request) or "continued in process",
    )
    monkeypatch.setattr(
        phasecontinuation,
        "_run_fresh_checkout",
        lambda *_args, **_kwargs: pytest.fail("unchanged HEAD must not spawn"),
    )

    output = phasecontinuation.continue_after_integration(
        "done",
        {"handle": "TASK-1kStable"},
        repo_root=tmp_path,
        before_head="same",
        after_head="same",
        environment={config.TASK_BACKEND_ENV: "/board"},
    )

    assert output == "continued in process"
    assert requests == [
        {
            "protocol": phasecontinuation.PHASE_CONTINUATION_PROTOCOL,
            "module": "spice.tasks.ops",
            "function": "_continue_phase",
            "payload": {
                "operation": "done",
                "input": {"handle": "TASK-1kStable"},
            },
            "environment": {config.TASK_BACKEND_ENV: "/board"},
        }
    ]


def test_fresh_process_crosses_an_incompatible_checkout_and_schema_cutover(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(phasecontinuation.PHASE_CONTINUATION_ENV, raising=False)
    repo = tmp_path / "checkout"
    repo.mkdir()
    _run(repo, "git", "init", "-b", "main")
    _run(repo, "git", "config", "user.email", "test@example.com")
    _run(repo, "git", "config", "user.name", "Test")
    probe = repo / "seamprobe.py"
    probe.write_text(
        "import sqlite3\n"
        "def continue_phase(payload):\n"
        "    connection = sqlite3.connect(payload['database'])\n"
        "    try:\n"
        "        return connection.execute("
        '"SELECT value FROM directives").fetchone()[0]\n'
        "    finally:\n"
        "        connection.close()\n",
        encoding="utf-8",
    )
    old_head = _commit(repo, "old continuation")
    probe.write_text(
        "import sqlite3\n"
        "def continue_phase(payload):\n"
        "    connection = sqlite3.connect(payload['database'])\n"
        "    try:\n"
        "        value = connection.execute("
        '"SELECT value FROM events").fetchone()[0]\n'
        "        return 'fresh:' + value\n"
        "    finally:\n"
        "        connection.close()\n",
        encoding="utf-8",
    )
    new_head = _commit(repo, "new continuation")
    _run(repo, "git", "checkout", "--detach", old_head)

    sys.path.insert(0, str(repo))
    try:
        stale = importlib.import_module("seamprobe")
    finally:
        sys.path.remove(str(repo))

    database = repo / "control.sqlite3"
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE directives (value TEXT NOT NULL)")
        connection.execute("INSERT INTO directives VALUES ('old')")
    _run(repo, "git", "checkout", "--detach", new_head)
    with sqlite3.connect(database) as connection:
        connection.execute("DROP TABLE directives")
        connection.execute("CREATE TABLE events (value TEXT NOT NULL)")
        connection.execute("INSERT INTO events VALUES ('new')")

    with pytest.raises(sqlite3.OperationalError, match="no such table: directives"):
        stale.continue_phase({"database": str(database)})

    output = phasecontinuation._run_fresh_checkout(
        {
            "protocol": phasecontinuation.PHASE_CONTINUATION_PROTOCOL,
            "module": "seamprobe",
            "function": "continue_phase",
            "payload": {"database": str(database)},
            "environment": {},
        },
        repo_root=repo,
        landing_head=new_head,
        operation="done",
    )

    assert output == "fresh:new"
    assert _run(repo, "git", "rev-parse", "HEAD") == new_head


def test_failed_fresh_continuation_names_authoritative_landing_and_exact_resume(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(phasecontinuation.PHASE_CONTINUATION_ENV, raising=False)
    monkeypatch.setattr(
        phasecontinuation,
        "run_parent_lifetime_command",
        lambda *_args, **_kwargs: subprocess.CompletedProcess(
            args=["python"], returncode=7, stdout="", stderr="new schema refused"
        ),
    )

    with pytest.raises(SpiceError) as raised:
        phasecontinuation._run_fresh_checkout(
            {
                "protocol": phasecontinuation.PHASE_CONTINUATION_PROTOCOL,
                "module": "spice.tasks.ops",
                "function": "_continue_review",
                "payload": {"handle": "TASK-1kLanded"},
                "environment": {},
            },
            repo_root=tmp_path,
            landing_head="abc123",
            operation="review",
        )

    message = str(raised.value)
    assert "integration landed at abc123" in message
    assert "landing is authoritative and will not be rolled back or re-published" in (
        message
    )
    assert "Run `spice task status`" in message
    assert "-m spice.tasks.phasecontinuation --payload" in message
    assert "new schema refused" in message


def test_nested_continuation_is_refused_without_spawning(tmp_path, monkeypatch):
    monkeypatch.setenv(phasecontinuation.PHASE_CONTINUATION_ENV, "landed")
    monkeypatch.setattr(
        phasecontinuation,
        "run_parent_lifetime_command",
        lambda *_args, **_kwargs: pytest.fail("nested continuation must not spawn"),
    )

    with pytest.raises(SpiceError, match="refusing nested.*authoritative at abc123"):
        phasecontinuation._run_fresh_checkout(
            {
                "protocol": phasecontinuation.PHASE_CONTINUATION_PROTOCOL,
                "module": "spice.tasks.ops",
                "function": "_continue_done",
                "payload": {},
                "environment": {},
            },
            repo_root=tmp_path,
            landing_head="abc123",
            operation="done",
        )


def test_continuation_stdin_refuses_payload_over_protocol_limit(monkeypatch):
    monkeypatch.setattr(
        phasecontinuation.sys,
        "stdin",
        io.StringIO("x" * (phasecontinuation.PHASE_CONTINUATION_MAX_BYTES + 1)),
    )

    with pytest.raises(SpiceError, match="exceeds the 1 MiB protocol limit"):
        phasecontinuation._request_from_process([])
