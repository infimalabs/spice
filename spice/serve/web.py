"""Index page and static asset delivery for the serve UI."""

from __future__ import annotations

import html
import json
import mimetypes
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass
from http import HTTPStatus
from importlib import metadata
from pathlib import Path
from typing import Any

from spice import defaults
from spice.configlayer import (
    SYSTEM_SOURCE,
    contextualize_config_error,
    load_config,
)
from spice.errors import SpiceError

STATIC_ROOT = Path(__file__).resolve().parent / "static"
DEFAULT_BRAND = defaults.string("serve", "brand")
DEFAULT_LIFETIME = defaults.string("serve", "default_lifetime")
VALID_LIFETIMES = defaults.strings("serve", "valid_lifetimes")


@dataclass(frozen=True)
class ServeBranding:
    name: str
    default_lifetime: str = DEFAULT_LIFETIME


_INDEX_HTML_TEMPLATE = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{brand_html}</title>
  <link rel="apple-touch-icon" sizes="180x180" href="/static/apple-touch-icon.png">
  <link rel="icon" type="image/png" sizes="32x32" href="/static/favicon-32x32.png">
  <link rel="icon" type="image/png" sizes="16x16" href="/static/favicon-16x16.png">
  <link rel="icon" href="/static/favicon.ico" sizes="any">
  <link rel="manifest" href="/static/site.webmanifest">
  <link rel="stylesheet" href="/static/index.css">
  <link rel="stylesheet" href="/static/composer.css">
  <link rel="stylesheet" href="/static/messages.css">
  <link rel="stylesheet" href="/static/status-colors.css">
</head>
<body>
  <header class="app-header">
    <div id="filter-strip" class="filter-strip" aria-hidden="true"></div>
    <button id="open-lane" class="spice-menu-button" type="button"
            title="Open {brand_attr} menu" aria-label="Open {brand_attr} menu"
            aria-haspopup="menu" aria-expanded="false">
      <span class="spice-menu-icon" aria-hidden="true">
        <span class="spice-menu-pepper">🌶️</span>
      </span>
      <span class="spice-menu-label">{brand_html}</span>
    </button>
  </header>
  <main id="swimlanes" class="swimlanes" aria-label="Open teams"></main>
  <script>const spiceServeBranding = {brand_json};</script>
  <script>const spiceServeInitialGlobalSettings = {global_settings_json};</script>
  <script src="/static/app.render.js"></script>
  <script src="/static/app.live-bus.js"></script>
  <script src="/static/app.mosaic-geometry.js"></script>
  <script src="/static/app.mosaic-engine.js"></script>
  <script src="/static/app.mosaic-wet-frozen.js"></script>
  <script src="/static/app.mosaic-full-replay.js"></script>
  <script src="/static/app.mosaic-event-log.js"></script>
  <script src="/static/app.mosaic-sizing.js"></script>
  <script src="/static/app.mosaic-reservations.js"></script>
  <script src="/static/app.mosaic-span.js"></script>
  <script src="/static/app.mosaic-render.js"></script>
  <script src="/static/app.mosaic-scroll.js"></script>
  <script src="/static/app.mosaic-stream.js"></script>
  <script src="/static/app.stream.js"></script>
  <script src="/static/app.submissions.js"></script>
  <script src="/static/app.lanes.js"></script>
  <script src="/static/app.menu.js"></script>
  <script src="/static/app.shell.js"></script>
  <script src="/static/app.composer.js"></script>
  <script src="/static/app.controls.js"></script>
  <script src="/static/app.filter-model.js"></script>
  <script src="/static/app.panes.js"></script>
  <script src="/static/app.groups.js"></script>
  <script src="/static/app.audio.js"></script>
  <script src="/static/app.js"></script>
</body>
</html>
"""


def spice_runtime_version() -> str:
    """Version of the installed spice runtime serving this UI.

    Reads the active package metadata so the UI reports the running tool, not a
    hard-coded or worktree-derived string. Empty when spice is run from a source
    tree with no installed distribution.
    """
    try:
        return metadata.version("spice-harness")
    except metadata.PackageNotFoundError:
        return ""


def serve_branding(repo_root: Path | None = None) -> ServeBranding:
    loaded = load_config(repo_root) if repo_root is not None else None
    raw_serve = (
        loaded.effective.get("serve", {})
        if loaded is not None
        else defaults.table("serve")
    )
    if not isinstance(raw_serve, Mapping):
        error = SpiceError("[tool.spice.serve] must be a table")
        if repo_root is None:
            raise error
        raise contextualize_config_error(repo_root, error, "serve") from error
    serve = dict(raw_serve)
    data = _read_pyproject(repo_root)
    project = _table(data, "project")
    brand_source = loaded.source_for("serve.brand") if loaded is not None else None
    configured_brand = _string(serve.get("brand"))
    if "brand" in serve and not configured_brand:
        error = SpiceError("[tool.spice.serve] brand must be a non-empty string")
        if repo_root is None:
            raise error
        raise contextualize_config_error(repo_root, error, "serve", "brand") from error
    project_brand = _string(project.get("name"))
    name = (
        project_brand
        if brand_source is not None
        and brand_source.name == SYSTEM_SOURCE
        and project_brand
        else configured_brand or DEFAULT_BRAND
    )
    raw_lifetime = _string(serve.get("default_lifetime"))
    if raw_lifetime not in VALID_LIFETIMES:
        error = SpiceError(
            "[tool.spice.serve] default_lifetime must name a valid lifetime"
        )
        if repo_root is None:
            raise error
        raise contextualize_config_error(
            repo_root, error, "serve", "default_lifetime"
        ) from error
    default_lifetime = raw_lifetime
    return ServeBranding(name=name, default_lifetime=default_lifetime)


def render_index_html(
    repo_root: Path | None = None,
    *,
    branding: ServeBranding | None = None,
    initial_global_settings: dict[str, Any] | None = None,
) -> str:
    resolved = branding or serve_branding(repo_root)
    brand_html = html.escape(resolved.name)
    brand_attr = html.escape(resolved.name, quote=True)
    brand_json = json.dumps(
        {
            "name": resolved.name,
            "defaultLifetime": resolved.default_lifetime,
            "version": spice_runtime_version(),
        },
        ensure_ascii=False,
    ).replace("</", "<\\/")
    global_settings_json = json.dumps(
        {
            "fastMode": (
                initial_global_settings.get("fastMode") is True
                if initial_global_settings
                else False
            ),
            "observerMode": (
                initial_global_settings.get("observerMode") is True
                if initial_global_settings
                else False
            ),
        },
        ensure_ascii=False,
    ).replace("</", "<\\/")
    return _INDEX_HTML_TEMPLATE.format(
        brand_html=brand_html,
        brand_attr=brand_attr,
        brand_json=brand_json,
        global_settings_json=global_settings_json,
    )


def _read_pyproject(repo_root: Path | None) -> dict[str, Any]:
    if repo_root is None:
        return {}
    try:
        with (repo_root / "pyproject.toml").open("rb") as handle:
            loaded = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError):
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _table(source: dict[str, Any], *path: str) -> dict[str, Any]:
    current: Any = source
    for key in path:
        if not isinstance(current, dict):
            return {}
        current = current.get(key)
    return current if isinstance(current, dict) else {}


def _string(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


def send_static_asset(handler: Any, name: str) -> None:
    static_root = STATIC_ROOT.resolve()
    candidate = (STATIC_ROOT / name).resolve()
    if not candidate.is_relative_to(static_root) or not candidate.is_file():
        handler.send_error(HTTPStatus.NOT_FOUND)
        return
    body = candidate.read_bytes()
    content_type = mimetypes.guess_type(candidate.name)[0] or "application/octet-stream"
    handler.send_response(HTTPStatus.OK)
    handler.send_header("Content-Type", content_type)
    handler.send_header("Content-Length", str(len(body)))
    handler.send_header("Cache-Control", "no-store")
    handler.end_headers()
    handler.wfile.write(body)
