"""`spice serve` — the supervisor web UI for steering bound agents."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from spice.errors import SpiceError
from spice.serve.app import DEFAULT_SERVE_HOST, DEFAULT_SERVE_PORT, run_serve
from spice.serve.browser.artifacts import serve_browser_artifact_path
from spice.serve.diagnostics import (
    render_team_diagnostics,
    team_diagnostics_payload,
)
from spice.tasks import config as task_config


def configure_serve_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "serve",
        help="Serve a localhost web UI for steering the repository's agents.",
    )
    parser.add_argument(
        "--host",
        default=DEFAULT_SERVE_HOST,
        help=f"Bind address. Default: {DEFAULT_SERVE_HOST}.",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=DEFAULT_SERVE_PORT,
        help=f"Bind port. Default: {DEFAULT_SERVE_PORT}.",
    )
    parser.add_argument(
        "--until",
        type=Path,
        metavar="PATH",
        help=(
            "Watch PATH and stop the server after it is created, deleted, "
            "touched, or changed."
        ),
    )
    parser.add_argument(
        "--task-backend",
        metavar="PATH",
        help=(
            "Absolute scratch task backend for this serve process; use it for "
            "live browser smoke runs."
        ),
    )
    parser.set_defaults(func=run_serve, serve_action=None)
    actions = parser.add_subparsers(dest="serve_action")

    teams = actions.add_parser(
        "teams",
        help="Print serve team-store, routing, and task-drain diagnostics.",
        recovery_examples=(
            "spice serve --task-backend /tmp/spice-smoke teams",
            "spice serve teams --json",
        ),
    )
    teams.add_argument(
        "--json",
        dest="json_output",
        action="store_true",
        help="Emit machine-readable diagnostics JSON.",
    )
    teams.set_defaults(func=run_serve_team_diagnostics)

    browser_artifact = actions.add_parser(
        "browser-artifact-path",
        help="Print the dedicated serve browser-smoke artifact path.",
        recovery_examples=("spice serve browser-artifact-path composer-smoke.png",),
    )
    browser_artifact.add_argument("filename")
    browser_artifact.set_defaults(func=run_serve_browser_artifact_path)


def run_serve_team_diagnostics(args: Any) -> int:
    _apply_task_backend(args)
    payload = team_diagnostics_payload()
    if args.json_output:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        print(render_team_diagnostics(payload))
    return 0


def run_serve_browser_artifact_path(args: Any) -> int:
    try:
        path = serve_browser_artifact_path(args.filename)
    except ValueError as exc:
        raise SpiceError(str(exc)) from exc
    print(path)
    return 0


def _apply_task_backend(args: Any) -> None:
    raw_backend = getattr(args, "task_backend", None)
    if not raw_backend:
        return
    backend = Path(raw_backend).expanduser()
    if not backend.is_absolute():
        raise SpiceError("spice serve --task-backend requires an absolute scratch path")
    task_config.set_backend(str(backend))
