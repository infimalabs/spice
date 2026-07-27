"""Serve stops serving once the team authority store moves past its build."""

from __future__ import annotations

import http.client
import threading
from argparse import Namespace
from http import HTTPStatus
from pathlib import Path

from spice.serve import app as serve_app
from spice.serve.app import run_serve
from spice.serve.team.schema import TEAM_AUTHORITY_SCHEMA_VERSION
from spice.serve.team.store import team_database_path
from spice.sqliteconnection import sqlite_connection
from spice.tasks import config as task_config

TEAM_SNAPSHOT_ROUTE = "/api/teams"
# Generous because these bound a pass, not a measurement: a serve loop that is
# going to stop does it within one poll of the request that found out, so
# anything reaching these is the loop still running rather than a slow machine.
SERVER_BIND_TIMEOUT_SECONDS = 10.0
SERVE_EXIT_TIMEOUT_SECONDS = 10.0
REQUEST_TIMEOUT_SECONDS = 5.0


def _serve_args(task_backend: Path) -> Namespace:
    return Namespace(
        host="127.0.0.1",
        port=0,
        until=None,
        task_backend=str(task_backend),
        allow_insecure_bind=False,
        auth_token=None,
    )


def _team_snapshot_status(host: str, port: int) -> int:
    """Ask the running server for team state and report how it answered."""
    connection = http.client.HTTPConnection(host, port, timeout=REQUEST_TIMEOUT_SECONDS)
    try:
        connection.request("GET", TEAM_SNAPSHOT_ROUTE)
        response = connection.getresponse()
        response.read()
        return int(response.status)
    finally:
        connection.close()


def _ask_for_team_state(host: str, port: int) -> None:
    """Make the request that meets the moved store, however it comes back.

    What the request returns is the caller's problem and not this one's: the
    store is refusing, so an error response, a dropped connection, and a
    reset are all faithful. The claim under test is what the server does after
    answering, which is stop.
    """
    try:
        _team_snapshot_status(host, port)
    except (OSError, http.client.HTTPException):
        return


def test_serve_stops_serving_once_the_store_moves_past_this_build(
    monkeypatch, tmp_path, capfd
):
    """The upgrade path: the store moved, so this process gets out of the way.

    Driven through `run_serve` against a real bound server because the wiring
    is the claim. The store learns to shut this process down only if serving
    handed it something to stop with, and the refusal only reaches that wiring
    if it survives the request handler that catches it first -- neither of
    which a store-level test can see.

    The first request establishes that this server was answering, so the exit
    is attributable to the stamp moving rather than to a server that never
    served.
    """
    monkeypatch.setattr(serve_app, "start_available_work_watch", lambda _state: None)
    monkeypatch.setattr(serve_app, "start_lifecycle_reconciler", lambda _state: None)
    real_server_class = serve_app._ServeHttpServer
    bound = threading.Event()
    servers: list[object] = []

    def record_server(*args: object) -> object:
        server = real_server_class(*args)
        servers.append(server)
        bound.set()
        return server

    monkeypatch.setattr(serve_app, "_ServeHttpServer", record_server)
    exit_codes: list[int] = []
    serve_thread = threading.Thread(
        target=lambda: exit_codes.append(run_serve(_serve_args(tmp_path / "backend"))),
        daemon=True,
    )

    try:
        serve_thread.start()
        assert bound.wait(SERVER_BIND_TIMEOUT_SECONDS)
        host, port = servers[0].server_address[:2]  # type: ignore[attr-defined]

        assert _team_snapshot_status(str(host), int(port)) == HTTPStatus.OK

        # Another process, built for a schema this one does not have, migrates
        # the store the two of them share.
        with sqlite_connection(team_database_path()) as connection:
            connection.execute(
                f"PRAGMA user_version = {TEAM_AUTHORITY_SCHEMA_VERSION + 1}"
            )
        _ask_for_team_state(str(host), int(port))

        serve_thread.join(SERVE_EXIT_TIMEOUT_SECONDS)
        assert exit_codes == [0]
    finally:
        for server in servers:
            server.shutdown()  # type: ignore[attr-defined]
        serve_thread.join(SERVE_EXIT_TIMEOUT_SECONDS)
        task_config.set_backend(None)

    output = capfd.readouterr().out
    assert "spice serve: team authority database changed to newer schema version" in (
        output
    )
    assert f"version {TEAM_AUTHORITY_SCHEMA_VERSION + 1}" in output
    assert f"requires {TEAM_AUTHORITY_SCHEMA_VERSION}" in output
