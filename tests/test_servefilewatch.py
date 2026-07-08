from __future__ import annotations

from argparse import Namespace
import os
from pathlib import Path
import threading

import pytest

from spice.errors import SpiceError
from spice.serve import app as serve_app
from spice.serve import filewatch as serve_filewatch
from spice.serve.app import run_serve
from spice.serve.filewatch import start_exit_file_watch


class FakeServer:
    server_address = ("127.0.0.1", 9999)

    def __init__(self, *_args: object) -> None:
        self.shutdown_count = 0
        self.closed = False
        self.shutdown_event = threading.Event()

    def serve_forever(self) -> None:
        self.shutdown_event.wait(timeout=5.0)

    def shutdown(self) -> None:
        self.shutdown_count += 1
        self.shutdown_event.set()

    def server_close(self) -> None:
        self.closed = True


def test_start_exit_file_watch_exits_on_content_change(
    monkeypatch, tmp_path: Path
) -> None:
    watched_path = tmp_path / "serve.stop"
    watched_path.write_text("initial\n", encoding="utf-8")
    fake_server = FakeServer()
    stop_event = threading.Event()
    watch_roots: list[Path] = []
    recursive_values: list[bool] = []
    filter_results: list[bool] = []

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del force_polling, debounce, stop_event
        watch_roots.append(root)
        recursive_values.append(recursive)
        filter_results.append(watch_filter(object(), str(watched_path)))
        filter_results.append(watch_filter(object(), str(tmp_path / "other.stop")))
        watched_path.write_text("changed\n", encoding="utf-8")
        yield {
            (object(), str(tmp_path / "other.stop")),
            (object(), str(watched_path)),
        }

    monkeypatch.setattr(serve_filewatch, "_import_watch", lambda: fake_watch)

    thread = start_exit_file_watch(
        fake_server,
        Namespace(until=watched_path),
        stop_event=stop_event,
    )
    assert isinstance(thread, threading.Thread)
    thread.join(timeout=1.0)

    assert watch_roots == [watched_path.resolve().parent]
    assert recursive_values == [False]
    assert filter_results == [True, False]
    assert fake_server.shutdown_count == 1


def test_start_exit_file_watch_ignores_events_without_content_change(
    monkeypatch, tmp_path: Path
) -> None:
    # macOS FSEvents replays pre-watch writes and fires on metadata churn;
    # events whose bytes match the at-watch-start baseline must not exit.
    watched_path = tmp_path / "serve.stop"
    watched_path.write_text("initial\n", encoding="utf-8")
    fake_server = FakeServer()
    stop_event = threading.Event()

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del root, watch_filter, force_polling, debounce, stop_event, recursive
        yield {(object(), str(watched_path))}
        yield {(object(), str(watched_path))}

    monkeypatch.setattr(serve_filewatch, "_import_watch", lambda: fake_watch)

    thread = start_exit_file_watch(
        fake_server,
        Namespace(until=watched_path),
        stop_event=stop_event,
    )
    assert isinstance(thread, threading.Thread)
    thread.join(timeout=1.0)

    assert fake_server.shutdown_count == 0


def test_start_exit_file_watch_exits_when_file_removed(
    monkeypatch, tmp_path: Path
) -> None:
    watched_path = tmp_path / "serve.stop"
    watched_path.write_text("initial\n", encoding="utf-8")
    fake_server = FakeServer()
    stop_event = threading.Event()

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del root, watch_filter, force_polling, debounce, stop_event, recursive
        watched_path.unlink()
        yield {(object(), str(watched_path))}

    monkeypatch.setattr(serve_filewatch, "_import_watch", lambda: fake_watch)

    thread = start_exit_file_watch(
        fake_server,
        Namespace(until=watched_path),
        stop_event=stop_event,
    )
    assert isinstance(thread, threading.Thread)
    thread.join(timeout=1.0)

    assert fake_server.shutdown_count == 1


def test_start_exit_file_watch_watches_parent_until_file_appears(
    monkeypatch, tmp_path: Path
) -> None:
    watched_path = tmp_path / "serve.stop"
    fake_server = FakeServer()
    stop_event = threading.Event()
    watch_roots: list[Path] = []
    exists_at_watch: list[bool] = []

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del watch_filter, force_polling, debounce, stop_event, recursive
        watch_roots.append(root)
        exists_at_watch.append(watched_path.exists())
        watched_path.write_text("created\n", encoding="utf-8")
        yield {(object(), str(watched_path))}

    monkeypatch.setattr(serve_filewatch, "_import_watch", lambda: fake_watch)

    thread = start_exit_file_watch(
        fake_server,
        Namespace(until=watched_path),
        stop_event=stop_event,
    )
    assert isinstance(thread, threading.Thread)
    thread.join(timeout=1.0)

    assert exists_at_watch == [False]
    assert watch_roots == [watched_path.parent.resolve()]
    assert fake_server.shutdown_count == 1


def test_start_exit_file_watch_rejects_missing_parent_directory(
    tmp_path: Path,
) -> None:
    watched_path = tmp_path / "missing" / "serve.stop"
    fake_server = FakeServer()

    with pytest.raises(SpiceError) as exc_info:
        start_exit_file_watch(
            fake_server,
            Namespace(until=watched_path),
            stop_event=threading.Event(),
        )

    assert "parent directory is missing" in str(exc_info.value)
    assert not watched_path.parent.exists()
    assert fake_server.shutdown_count == 0


def test_start_exit_file_watch_rejects_directory_path(tmp_path: Path) -> None:
    with pytest.raises(SpiceError) as exc_info:
        start_exit_file_watch(
            FakeServer(),
            Namespace(until=tmp_path),
            stop_event=threading.Event(),
        )

    assert "is a directory" in str(exc_info.value)


def test_serve_leaves_missing_until_path_uncreated(monkeypatch, tmp_path: Path):
    watched_path = tmp_path / "serve.stop"
    fake_server = FakeServer()
    watch_roots: list[Path] = []
    exists_at_watch: list[bool] = []
    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del watch_filter, force_polling, debounce, stop_event, recursive
        watch_roots.append(root)
        exists_at_watch.append(watched_path.exists())
        watched_path.write_text("created\n", encoding="utf-8")
        yield {(object(), str(watched_path))}

    monkeypatch.setattr(serve_filewatch, "_import_watch", lambda: fake_watch)

    result = run_serve(
        Namespace(
            host="127.0.0.1",
            port=0,
            until=watched_path,
            task_backend=None,
        )
    )

    assert result == 0
    assert watch_roots == [watched_path.parent.resolve()]
    assert exists_at_watch == [False]
    assert fake_server.shutdown_count == 1
    assert fake_server.closed is True


def test_serve_refuses_non_loopback_bind_without_opt_in(monkeypatch) -> None:
    def fail_server(*_args: object) -> FakeServer:
        raise AssertionError("server should not bind before opt-in guard passes")

    monkeypatch.setattr(serve_app, "_ServeHttpServer", fail_server)

    with pytest.raises(SpiceError) as exc_info:
        run_serve(
            Namespace(
                host="0.0.0.0",
                port=8765,
                until=None,
                task_backend=None,
                allow_insecure_bind=False,
                auth_token=None,
            )
        )

    message = str(exc_info.value)
    assert "refuses to bind" in message
    assert "http://0.0.0.0:8765" in message
    assert "--allow-insecure-bind" in message
    assert "--auth-token TOKEN" in message


def test_serve_warns_when_insecure_non_loopback_bind_allowed(
    monkeypatch, capsys
) -> None:
    fake_server = FakeServer()
    fake_server.server_address = ("0.0.0.0", 9999)
    fake_server.shutdown_event.set()
    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)
    monkeypatch.setattr(
        serve_app, "start_exit_file_watch", lambda *_args, **_kwargs: None
    )

    result = run_serve(
        Namespace(
            host="0.0.0.0",
            port=9999,
            until=None,
            task_backend=None,
            allow_insecure_bind=True,
            auth_token=None,
        )
    )

    assert result == 0
    output = capsys.readouterr().out
    assert "WARNING:" in output
    assert "http://0.0.0.0:9999" in output
    assert "--allow-insecure-bind" in output


def test_serve_warns_when_token_protected_non_loopback_bind_allowed(
    monkeypatch, capsys
) -> None:
    fake_server = FakeServer()
    fake_server.server_address = ("0.0.0.0", 9999)
    fake_server.shutdown_event.set()
    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)
    monkeypatch.setattr(
        serve_app, "start_exit_file_watch", lambda *_args, **_kwargs: None
    )

    result = run_serve(
        Namespace(
            host="0.0.0.0",
            port=9999,
            until=None,
            task_backend=None,
            allow_insecure_bind=False,
            auth_token="secret",
        )
    )

    assert result == 0
    output = capsys.readouterr().out
    assert "WARNING:" in output
    assert "http://0.0.0.0:9999" in output
    assert "token auth enabled" in output


def test_serve_exits_after_watched_file_changes(monkeypatch, tmp_path: Path) -> None:
    watched_path = tmp_path / "serve.stop"
    watched_path.write_text("initial\n", encoding="utf-8")
    fake_server = FakeServer()
    watch_roots: list[Path] = []

    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del watch_filter, force_polling, debounce, stop_event, recursive
        watch_roots.append(root)
        watched_path.write_text("changed\n", encoding="utf-8")
        yield {(object(), str(watched_path))}

    monkeypatch.setattr(serve_filewatch, "_import_watch", lambda: fake_watch)

    result = run_serve(
        Namespace(
            host="127.0.0.1",
            port=0,
            until=watched_path,
            task_backend=None,
        )
    )

    assert result == 0
    assert watch_roots == [watched_path.resolve().parent]
    assert fake_server.shutdown_count == 1
    assert fake_server.closed is True


def test_serve_scrubs_agent_driver_environment(monkeypatch, tmp_path: Path) -> None:
    fake_server = FakeServer()
    fake_server.shutdown_event.set()
    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)
    monkeypatch.setattr(
        serve_app, "start_exit_file_watch", lambda *_args, **_kwargs: None
    )
    for driver in serve_app.all_drivers():
        monkeypatch.setenv(driver.thread_id_env, "ambient-thread")
    monkeypatch.setenv(serve_app.SPICE_AGENT_DRIVER_ENV, "codex")

    result = run_serve(
        Namespace(
            host="127.0.0.1",
            port=0,
            until=None,
            task_backend=None,
        )
    )

    assert result == 0
    assert serve_app.SPICE_AGENT_DRIVER_ENV not in os.environ  # env-policy: allow
    for driver in serve_app.all_drivers():
        assert driver.thread_id_env not in os.environ  # env-policy: allow
