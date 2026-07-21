"""Read-only observer mode over existing Codex and Claude transcripts."""

from __future__ import annotations

import http.client
import json
import shutil
import threading
from http import HTTPStatus
from pathlib import Path
from typing import Any

from spice.cli.parser import build_parser
from spice.serve import app
from spice.serve.observer import (
    detect_observer_primary,
    discover_observer_sessions,
    observer_messages_payload,
)

FIXTURES = Path(__file__).parent / "fixtures" / "session"
CODEX_THREAD = "12345678-1234-1234-1234-123456789abc"
CLAUDE_THREAD = "87654321-4321-4321-4321-cba987654321"
CLAUDE_CONFIG_DIR_ENV = "CLAUDE_CONFIG_DIR"  # env-policy: allow
CODEX_HOME_ENV = "CODEX_HOME"  # env-policy: allow


def _copy_observer_fixtures(root: Path) -> tuple[Path, Path]:
    codex = root / f"rollout-2026-01-01T00-00-00-{CODEX_THREAD}.jsonl"
    claude = root / f"{CLAUDE_THREAD}.jsonl"
    shutil.copyfile(FIXTURES / "supervised_codex.jsonl", codex)
    shutil.copyfile(FIXTURES / "supervised_claude.jsonl", claude)
    return codex, claude


def _directory_snapshot(root: Path) -> dict[str, tuple[bool, int, bytes]]:
    return {
        str(path.relative_to(root)): (
            path.is_file(),
            path.stat().st_mtime_ns,
            path.read_bytes() if path.is_file() else b"",
        )
        for path in sorted(root.rglob("*"))
    }


def _request_json(
    server: app._ServeHttpServer,
    method: str,
    path: str,
    payload: dict[str, Any] | None = None,
) -> tuple[int, dict[str, Any]]:
    host, port = server.server_address[:2]
    body = json.dumps(payload).encode() if payload is not None else None
    headers = {"Content-Type": "application/json"} if body is not None else {}
    connection = http.client.HTTPConnection(host, port, timeout=2)
    connection.request(method, path, body=body, headers=headers)
    response = connection.getresponse()
    response_body = json.loads(response.read())
    connection.close()
    return response.status, response_body


def test_watch_parser_accepts_multiple_session_roots_and_until(tmp_path: Path) -> None:
    stop_path = tmp_path / "watch.stop"

    args = build_parser().parse_args(
        ["watch", "one", "two", "--port", "0", "--until", str(stop_path)]
    )

    assert args.command == "watch"
    assert args.session_dirs == [Path("one"), Path("two")]
    assert args.port == 0
    assert args.until == stop_path
    assert args.observer_mode is True


def test_watch_discovery_prints_paste_ready_command_and_token_url_read_only(
    tmp_path: Path,
    monkeypatch: Any,
    capsys: Any,
) -> None:
    codex_home = tmp_path / "codex-home"
    codex_sessions = codex_home / "sessions"
    claude_home = tmp_path / "claude-home"
    claude_projects = claude_home / "projects"
    codex_sessions.mkdir(parents=True)
    claude_projects.mkdir(parents=True)
    (codex_sessions / "existing.txt").write_text("codex\n", encoding="utf-8")
    (claude_projects / "existing.txt").write_text("claude\n", encoding="utf-8")
    before = _directory_snapshot(tmp_path)
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))
    monkeypatch.setenv(CLAUDE_CONFIG_DIR_ENV, str(claude_home))
    monkeypatch.setattr(
        "spice.serve.observer.shutil.which", lambda binary: f"/tools/{binary}"
    )
    args = build_parser().parse_args(
        ["watch", "--port", "9876", "--auth-token", "hello world"]
    )

    result = args.func(args)

    lines = capsys.readouterr().out.splitlines()
    assert result == 0
    assert args.session_dirs == []
    assert lines == [
        "command: spice watch "
        f"{codex_sessions} {claude_projects} --host 127.0.0.1 --port 9876 "
        "--auth-token 'hello world'",
        "url: http://127.0.0.1:9876/?token=hello+world",
        "spice watch: classification=both primary=codex basis=session-root "
        "precedence=session-root>config>cli;codex>claude "
        "signals=codex[session-root,config,cli];"
        "claude[session-root,config,cli] roots=2 read_only=true",
    ]
    assert _directory_snapshot(tmp_path) == before


def test_watch_primary_override_is_surfaced_and_orders_roots(
    tmp_path: Path,
    monkeypatch: Any,
    capsys: Any,
) -> None:
    codex_home = tmp_path / "codex-home"
    codex_sessions = codex_home / "sessions"
    claude_home = tmp_path / "claude-home"
    claude_projects = claude_home / "projects"
    codex_sessions.mkdir(parents=True)
    claude_projects.mkdir(parents=True)
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))
    monkeypatch.setenv(CLAUDE_CONFIG_DIR_ENV, str(claude_home))
    monkeypatch.setattr("spice.serve.observer.shutil.which", lambda _binary: None)
    args = build_parser().parse_args(["watch", "--primary", "claude", "--port", "9876"])

    result = args.func(args)

    lines = capsys.readouterr().out.splitlines()
    assert result == 0
    assert lines == [
        f"command: spice watch {claude_projects} {codex_sessions} "
        "--host 127.0.0.1 --port 9876",
        "url: http://127.0.0.1:9876/",
        "spice watch: classification=both primary=claude basis=override "
        "precedence=session-root>config>cli;codex>claude "
        "signals=codex[session-root,config];claude[session-root,config] "
        "roots=2 read_only=true",
    ]


def test_watch_detection_prefers_session_root_over_config_and_cli(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    codex_home = tmp_path / "codex-home"
    codex_home.mkdir()
    claude_home = tmp_path / "claude-home"
    claude_projects = claude_home / "projects"
    claude_projects.mkdir(parents=True)
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))
    monkeypatch.setenv(CLAUDE_CONFIG_DIR_ENV, str(claude_home))
    monkeypatch.setattr(
        "spice.serve.observer.shutil.which",
        lambda binary: "/tools/codex" if binary == "codex" else None,
    )

    detection = detect_observer_primary()

    assert detection.classification == "both"
    assert detection.primary == "claude"
    assert detection.basis == "session-root"
    assert detection.roots == (claude_projects,)


def test_watch_detection_classifies_a_single_claude_provider(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    codex_home = tmp_path / "absent-codex-home"
    claude_home = tmp_path / "claude-home"
    claude_projects = claude_home / "projects"
    claude_projects.mkdir(parents=True)
    monkeypatch.setenv(CODEX_HOME_ENV, str(codex_home))
    monkeypatch.setenv(CLAUDE_CONFIG_DIR_ENV, str(claude_home))
    monkeypatch.setattr(
        "spice.serve.observer.shutil.which",
        lambda binary: "/tools/claude" if binary == "claude" else None,
    )

    detection = detect_observer_primary()

    assert detection.classification == "Claude-primary"
    assert detection.primary == "claude"
    assert detection.signal_summary == "codex[none];claude[session-root,config,cli]"


def test_observer_discovers_both_drivers_and_preserves_timeline(tmp_path: Path) -> None:
    _copy_observer_fixtures(tmp_path)
    registry = discover_observer_sessions([tmp_path])
    state = app.ServeState(anchor_root=tmp_path, observer=registry)

    assert len(registry.sessions) == 2
    assert sorted(
        session.transcript.owner_driver.name for session in registry.sessions
    ) == ["claude", "codex"]
    assert len({session.target.id for session in registry.sessions}) == 2

    payloads = {
        session.transcript.owner_driver.name: observer_messages_payload(
            state, session.target, limit=50
        )
        for session in registry.sessions
    }
    codex_messages = payloads["codex"]["messages"]
    claude_messages = payloads["claude"]["messages"]

    assert "slices rendered" in [message["text"] for message in codex_messages]
    assert "claude slices rendered" in [message["text"] for message in claude_messages]
    assert "Bash: spice task status" in [
        message["preview"] for message in claude_messages
    ]
    assert [message["index"] for message in codex_messages] == sorted(
        (message["index"] for message in codex_messages), reverse=True
    )
    assert [message["index"] for message in claude_messages] == sorted(
        (message["index"] for message in claude_messages), reverse=True
    )


def test_observer_http_surface_is_read_only_and_directory_stable(
    tmp_path: Path,
    monkeypatch: Any,
) -> None:
    _copy_observer_fixtures(tmp_path)
    before = _directory_snapshot(tmp_path)
    registry = discover_observer_sessions([tmp_path])

    def reject_team_store() -> None:
        raise AssertionError("observer mode attempted to construct a team store")

    monkeypatch.setattr(app, "ServeTeamStore", reject_team_store)
    state = app.ServeState(anchor_root=tmp_path, observer=registry)
    server = app._ServeHttpServer(("127.0.0.1", 0), app._ServeHandler, state)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    target = registry.sessions[0].target

    try:
        status, targets = _request_json(server, "GET", "/api/work/trees")
        assert status == HTTPStatus.OK
        assert len(targets["workTrees"]) == 2
        assert targets["defaultTargetId"] in {
            session.target.id for session in registry.sessions
        }

        status, agent = _request_json(
            server, "GET", f"/api/work/trees/{target.id}/agent/status"
        )
        assert status == HTTPStatus.OK
        assert agent["status"] == "observer"
        assert agent["launchable"] is False

        status, response = _request_json(
            server,
            "POST",
            f"/api/work/trees/{target.id}/send",
            {"text": "attempted mutation"},
        )
        assert status == HTTPStatus.METHOD_NOT_ALLOWED
        assert response == {"ok": False, "error": "spice watch is read-only"}
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

    assert _directory_snapshot(tmp_path) == before


def test_observer_reports_empty_and_unreadable_sources(tmp_path: Path) -> None:
    empty = discover_observer_sessions([tmp_path])
    assert empty.sessions == ()
    assert empty.errors == ("no recognizable Codex or Claude transcripts found",)

    transcript = tmp_path / f"{CLAUDE_THREAD}.jsonl"
    transcript.write_text("{}\n", encoding="utf-8")
    transcript.chmod(0)
    try:
        unreadable = discover_observer_sessions([tmp_path])
        assert unreadable.sessions == ()
        assert len(unreadable.errors) == 1
        assert unreadable.errors[0].startswith(f"could not read {transcript}:")
    finally:
        transcript.chmod(0o600)
