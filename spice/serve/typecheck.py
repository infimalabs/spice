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
    "spice/serve/static/app.menu.js",
    "spice/serve/static/app.shell.js",
    "spice/serve/static/app.composer.js",
    "spice/serve/static/app.controls.js",
    "spice/serve/static/app.filter-model.js",
    "spice/serve/static/app.panes.js",
    "spice/serve/static/app.groups.js",
    "spice/serve/static/app.audio.js",
    "spice/serve/static/app.js",
)

SERVE_WEB_MODULE_JS_PATHS = (
    # Module-style islands use top-level await/import/export and need a tsc
    # pass with module resolution enabled instead of the global-script lane.
    "spice/serve/static/app.metrics-lit.js",
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

TSC_MODULE_CHECKJS_ARGS = (
    *TSC_CHECKJS_ARGS,
    "--module",
    "ESNext",
    "--moduleResolution",
    "bundler",
)


def serve_web_js_targets(repo_root: Path) -> tuple[str, ...]:
    """The serve static sources present in this repo; empty for target repos."""
    return tuple(p for p in SERVE_WEB_JS_PATHS if (repo_root / p).is_file())


def serve_web_module_js_targets(repo_root: Path) -> tuple[str, ...]:
    """The serve static ES module sources present in this repo."""
    return tuple(p for p in SERVE_WEB_MODULE_JS_PATHS if (repo_root / p).is_file())


def serve_web_typecheck_targets(repo_root: Path) -> tuple[str, ...]:
    return (*serve_web_js_targets(repo_root), *serve_web_module_js_targets(repo_root))


def serve_web_typecheck_argv(
    targets: tuple[str, ...] = SERVE_WEB_JS_PATHS,
    *,
    module_mode: bool = False,
) -> tuple[str, ...]:
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
        *(TSC_MODULE_CHECKJS_ARGS if module_mode else TSC_CHECKJS_ARGS),
        *targets,
    )


def run_serve_web_typecheck(repo_root: Path) -> None:
    targets = serve_web_js_targets(repo_root)
    module_targets = serve_web_module_js_targets(repo_root)
    if not targets and not module_targets:
        # The checkJs lane gates spice's own static sources; a target repo
        # without them has nothing in this lane.
        return
    if targets:
        _run_serve_web_typecheck_argv(repo_root, serve_web_typecheck_argv(targets))
    if module_targets:
        _run_serve_web_typecheck_argv(
            repo_root,
            serve_web_typecheck_argv(module_targets, module_mode=True),
        )


def _run_serve_web_typecheck_argv(repo_root: Path, argv: tuple[str, ...]) -> None:
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
