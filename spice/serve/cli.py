"""`spice serve` — the supervisor web UI for steering bound agents."""

from __future__ import annotations

import json
import shlex
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

from spice.errors import SpiceError
from spice.serve.app import (
    DEFAULT_SERVE_HOST,
    DEFAULT_SERVE_PORT,
    apply_serve_backends,
    guard_exposed_bind,
    run_serve,
    serve_address,
    serve_auth_token,
)
from spice.serve.browser.artifacts import serve_browser_artifact_path
from spice.serve.diagnostics import (
    render_team_diagnostics,
    team_diagnostics_payload,
)
from spice.serve.observer import discover_default_observer_roots


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
        "--allow-insecure-bind",
        action="store_true",
        help=(
            "Allow the no-auth serve control surface to bind to a non-loopback "
            "address. On wildcard binds, WebSocket Origin checks degrade to "
            "Origin-equals-Host instead of the rebinding-resistant authority "
            "match."
        ),
    )
    parser.add_argument(
        "--auth-token",
        metavar="TOKEN",
        help=(
            "Require TOKEN for serve HTTP and WebSocket requests; on wildcard "
            "binds the supplied token, not the rebinding-resistant authority "
            "match, is the operative defense after Origin checks degrade to "
            "Origin-equals-Host."
        ),
    )
    parser.add_argument(
        "--until",
        type=Path,
        metavar="PATH",
        help=(
            "Watch PATH and stop the server when it appears, disappears, or "
            "its content changes. PATH is never created; only the final path "
            "component may be missing (the parent directory must exist)."
        ),
    )
    parser.add_argument(
        "--backend",
        metavar="PATH",
        help=(
            "Absolute scratch root capturing every managed-state surface for "
            "this serve process (agent registry, inboxes, session records, "
            "and the task store unless --task-backend claims it)."
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


def configure_watch_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "watch",
        help="Observe existing Codex or Claude session directories read-only.",
    )
    parser.add_argument(
        "session_dirs",
        nargs="*",
        type=Path,
        metavar="SESSION_DIR",
        help="Directory or transcript file to observe; repeat for multiple roots.",
    )
    parser.add_argument(
        "--discover",
        action="store_true",
        help=(
            "Print a paste-ready watch command and browser URL for existing "
            "Codex and Claude session roots, then exit."
        ),
    )
    parser.add_argument("--host", default=DEFAULT_SERVE_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_SERVE_PORT)
    parser.add_argument("--allow-insecure-bind", action="store_true")
    parser.add_argument("--auth-token", metavar="TOKEN")
    parser.add_argument(
        "--until",
        type=Path,
        metavar="PATH",
        help="Stop when PATH appears, disappears, or changes.",
    )
    parser.set_defaults(func=run_watch, serve_action=None, observer_mode=True)


def run_watch(args: Any) -> int:
    if bool(args.discover):
        return run_watch_discovery(args)
    if not args.session_dirs:
        raise SpiceError(
            "spice watch requires SESSION_DIR... or --discover; manual usage: "
            "spice watch <session-dir> [<session-dir> ...]"
        )
    return run_serve(args)


def run_watch_discovery(args: Any) -> int:
    if args.session_dirs:
        raise SpiceError(
            "spice watch --discover does not accept SESSION_DIR arguments; use "
            "spice watch <session-dir> [<session-dir> ...] for explicit roots"
        )
    roots = discover_default_observer_roots()
    if not roots:
        raise SpiceError(
            "spice watch --discover: none detected; manual usage: "
            "spice watch <session-dir> [<session-dir> ...]"
        )
    if int(args.port) == 0:
        raise SpiceError(
            "spice watch --discover requires a fixed --port so the printed URL is exact"
        )

    auth_token = serve_auth_token(args)
    guard_exposed_bind(
        str(args.host),
        int(args.port),
        allow_insecure=bool(args.allow_insecure_bind),
        auth_token=auth_token,
    )
    command = [
        "spice",
        "watch",
        *[str(path) for path in roots],
        "--host",
        str(args.host),
        "--port",
        str(args.port),
    ]
    if bool(args.allow_insecure_bind):
        command.append("--allow-insecure-bind")
    if auth_token is not None:
        command.extend(["--auth-token", auth_token])
    if args.until is not None:
        command.extend(["--until", str(args.until)])

    url = serve_address(str(args.host), int(args.port)) + "/"
    if auth_token is not None:
        url += "?" + urlencode({"token": auth_token})
    print(f"command: {shlex.join(command)}")
    print(f"url: {url}")
    print(f"spice watch: detected={len(roots)} read_only=true")
    return 0


def run_serve_team_diagnostics(args: Any) -> int:
    apply_serve_backends(args)
    payload = team_diagnostics_payload()
    if args.json_output:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        print(render_team_diagnostics(payload))
    return 0


def run_serve_browser_artifact_path(args: Any) -> int:
    apply_serve_backends(args)
    try:
        path = serve_browser_artifact_path(args.filename)
    except ValueError as exc:
        raise SpiceError(str(exc)) from exc
    print(path)
    return 0
