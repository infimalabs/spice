"""TypeScript checkJs lane for the serve static browser scripts."""

from __future__ import annotations

import shlex
import subprocess
from pathlib import Path

from spice.errors import SpiceError
from spice.paths import find_tool

SERVE_WEB_JS_PATHS = (
    "spice/serve/static/app.types.js",
    "spice/serve/static/app.render.js",
    "spice/serve/static/app.stream.js",
    "spice/serve/static/app.lanes.js",
    "spice/serve/static/app.shell.js",
    "spice/serve/static/app.controls.js",
    "spice/serve/static/app.panes.js",
    "spice/serve/static/app.groups.js",
    "spice/serve/static/app.audio.js",
    "spice/serve/static/app.js",
)

TSC_CHECKJS_ARGS = (
    "--allowJs",
    "--checkJs",
    "--noEmit",
    "--target",
    "ES2023",
    "--lib",
    "DOM,ES2023",
    "--noImplicitAny",
    "false",
    "--strictNullChecks",
    "false",
)


def serve_web_typecheck_argv() -> tuple[str, ...]:
    npm = find_tool("npm")
    if not npm:
        raise SpiceError(
            "npm is required for serve web typechecking; install Node/npm or "
            "run `spice dev doctor` for environment details"
        )
    return (
        npm,
        "exec",
        "--yes",
        "--package",
        "typescript",
        "tsc",
        "--",
        *TSC_CHECKJS_ARGS,
        *SERVE_WEB_JS_PATHS,
    )


def run_serve_web_typecheck(repo_root: Path) -> None:
    argv = serve_web_typecheck_argv()
    result = subprocess.run(
        list(argv),
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    if result.returncode == 0:
        return
    output = "\n".join(
        part for part in (result.stdout.strip(), result.stderr.strip()) if part
    )
    message = f"{shlex.join(argv)} exited {result.returncode}"
    if output:
        message += ":\n" + output
    raise SpiceError(message)
