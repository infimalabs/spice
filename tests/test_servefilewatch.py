from __future__ import annotations

from argparse import Namespace
import os
from pathlib import Path
import threading

from spice.serve import app as serve_app
from spice.serve import filewatch as serve_filewatch
from spice.serve.app import run_serve
from spice.serve.filewatch import (
    file_watch_path_changed,
    snapshot_file_watch_path,
    start_exit_file_watch,
)


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


def test_file_watch_snapshot_detects_modified_file(tmp_path: Path) -> None:
    watched_path = tmp_path / "serve.stop"
    watched_path.write_text("initial\n", encoding="utf-8")
    baseline = snapshot_file_watch_path(watched_path)

    watched_path.write_text("initial\nchanged\n", encoding="utf-8")

    assert file_watch_path_changed(watched_path, baseline) is True


def test_start_exit_file_watch_uses_watched_parent_events(
    monkeypatch, tmp_path: Path
) -> None:
    watched_path = tmp_path / "serve.stop"
    fake_server = FakeServer()
    stop_event = threading.Event()
    watch_roots: list[Path] = []
    filter_results: list[bool] = []

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del force_polling, debounce, stop_event, recursive
        watch_roots.append(root)
        filter_results.append(watch_filter(object(), str(watched_path)))
        yield {
            (object(), str(tmp_path / "other.stop")),
            (object(), str(watched_path)),
        }

    monkeypatch.setattr(serve_filewatch, "watch", fake_watch)

    thread = start_exit_file_watch(
        fake_server,
        Namespace(until=watched_path),
        stop_event=stop_event,
    )
    assert isinstance(thread, threading.Thread)
    thread.join(timeout=1.0)

    assert watch_roots == [tmp_path]
    assert filter_results == [True]
    assert fake_server.shutdown_count == 1


def test_serve_exits_after_watched_file_changes(monkeypatch, tmp_path: Path) -> None:
    watched_path = tmp_path / "serve.stop"
    fake_server = FakeServer()
    watch_roots: list[Path] = []

    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)

    def fake_watch(
        root, *, watch_filter, force_polling, debounce, stop_event, recursive
    ):
        del watch_filter, force_polling, debounce, stop_event, recursive
        watch_roots.append(root)
        yield {(object(), str(watched_path))}

    monkeypatch.setattr(serve_filewatch, "watch", fake_watch)

    result = run_serve(
        Namespace(
            host="127.0.0.1",
            port=0,
            until=watched_path,
            task_backend=None,
        )
    )

    assert result == 0
    assert watch_roots == [tmp_path]
    assert fake_server.shutdown_count == 1
    assert fake_server.closed is True


def test_serve_scrubs_agent_driver_environment(monkeypatch, tmp_path: Path) -> None:
    fake_server = FakeServer()
    fake_server.shutdown_event.set()
    monkeypatch.setattr(serve_app, "_ServeHttpServer", lambda *_args: fake_server)
    monkeypatch.setattr(
        serve_app, "start_exit_file_watch", lambda *_args, **_kwargs: None
    )
    for driver in serve_app.ALL_DRIVERS:
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
    assert serve_app.SPICE_AGENT_DRIVER_ENV not in os.environ
    for driver in serve_app.ALL_DRIVERS:
        assert driver.thread_id_env not in os.environ
