"""`spice serve` — the supervisor web UI for steering bound agents."""

from __future__ import annotations

import json
import shlex
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

from spice.config.values import configured_serve_host, configured_serve_port
from spice.errors import SpiceError
from spice.serve.app import (
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
from spice.serve.metrics import rebuild_transcript_metrics
from spice.serve.observer import (
    OBSERVER_PRIMARY_PRECEDENCE,
    detect_observer_primary,
)
from spice.serve.team.projection import (
    AGENT_ACTIVITY,
    PROJECTION_FAMILIES,
    PROJECTION_FAMILIES_BY_NAME,
)
from spice.serve.team.store import ServeTeamStore


def configure_serve_parser(subparsers: Any) -> None:
    default_host = configured_serve_host()
    default_port = configured_serve_port()
    parser = subparsers.add_parser(
        "serve",
        help="Serve a localhost web UI for steering the repository's agents.",
    )
    parser.add_argument(
        "--host",
        default=default_host,
        help=f"Bind address. Default: {default_host}.",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=default_port,
        help=f"Bind port. Default: {default_port}.",
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

    rebuild_projections = actions.add_parser(
        "rebuild-projections",
        help=(
            "Rebuild Serve projections from native facts and atomically publish "
            "the completed generation."
        ),
        recovery_examples=(
            "spice serve rebuild-projections",
            "spice serve rebuild-projections agentActivity",
        ),
    )
    rebuild_projections.add_argument(
        "families",
        nargs="*",
        metavar="FAMILY",
        help="Family names to rebuild. Default: every registered family.",
    )
    rebuild_projections.set_defaults(func=run_serve_rebuild_projections)

    browser_artifact = actions.add_parser(
        "browser-artifact-path",
        help="Print the dedicated serve browser-smoke artifact path.",
        recovery_examples=("spice serve browser-artifact-path composer-smoke.png",),
    )
    browser_artifact.add_argument("filename")
    browser_artifact.set_defaults(func=run_serve_browser_artifact_path)


def configure_watch_parser(subparsers: Any) -> None:
    default_host = configured_serve_host()
    default_port = configured_serve_port()
    parser = subparsers.add_parser(
        "watch",
        help="Observe existing Codex or Claude session directories read-only.",
        description=(
            "Observe existing Codex or Claude sessions read-only. With no "
            "SESSION_DIR, detect the local primary provider and print a "
            "paste-ready command plus browser URL."
        ),
    )
    parser.add_argument(
        "session_dirs",
        nargs="*",
        type=Path,
        metavar="SESSION_DIR",
        help="Directory or transcript file to observe; repeat for multiple roots.",
    )
    parser.add_argument(
        "--primary",
        choices=OBSERVER_PRIMARY_PRECEDENCE,
        help=(
            "Force the detected primary provider. The provider must have an "
            "existing session root."
        ),
    )
    parser.add_argument("--host", default=default_host)
    parser.add_argument("--port", type=int, default=default_port)
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
    if not args.session_dirs:
        return run_watch_detection(args)
    if args.primary is not None:
        raise SpiceError(
            "spice watch --primary is only valid with automatic detection; "
            "remove explicit SESSION_DIR arguments"
        )
    return run_serve(args)


def run_watch_detection(args: Any) -> int:
    detection = detect_observer_primary(args.primary)
    roots = detection.roots
    if int(args.port) == 0:
        raise SpiceError(
            "spice watch automatic detection requires a fixed --port so the "
            "printed URL is exact"
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
    print(
        "spice watch: "
        f"classification={detection.classification} "
        f"primary={detection.primary} "
        f"basis={detection.basis} "
        f"precedence={detection.precedence} "
        f"signals={detection.signal_summary} "
        f"roots={len(roots)} read_only=true"
    )
    return 0


def run_serve_team_diagnostics(args: Any) -> int:
    apply_serve_backends(args)
    payload = team_diagnostics_payload()
    if args.json_output:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        print(render_team_diagnostics(payload))
    return 0


def run_serve_rebuild_projections(args: Any) -> int:
    """Rebuild selected families and report the build each reader now sees.

    Population happens in an isolated projection file. Readers keep the prior
    complete generation until the replacement is ready and one transaction
    publishes it, so interruption cannot expose a partial replay.
    """
    apply_serve_backends(args)
    return _rebuild_projection_families(ServeTeamStore(), args.families)


def _rebuild_projection_families(store: ServeTeamStore, families: list[str]) -> int:
    requested = tuple(dict.fromkeys(families)) or tuple(
        family.name for family in PROJECTION_FAMILIES
    )
    unknown = sorted(set(requested) - set(PROJECTION_FAMILIES_BY_NAME))
    if unknown:
        known = ", ".join(sorted(PROJECTION_FAMILIES_BY_NAME))
        raise SpiceError(
            f"unknown Serve projection family {', '.join(unknown)}; known: {known}"
        )
    for family_name in requested:
        if family_name != AGENT_ACTIVITY.name:
            raise SpiceError(f"no projection rebuilder registered for {family_name}")
        state = rebuild_transcript_metrics(store)
        counts = " ".join(
            f"{table}={count}" for table, count in sorted(state.row_counts.items())
        )
        print(
            f"serve projections rebuilt {state.family.name} "
            f"generation={state.generation} {counts} "
            f"status={state.status} rebuild={state.family.rebuild}"
        )
    return 0


def run_serve_browser_artifact_path(args: Any) -> int:
    apply_serve_backends(args)
    try:
        path = serve_browser_artifact_path(args.filename)
    except ValueError as exc:
        raise SpiceError(str(exc)) from exc
    print(path)
    return 0
