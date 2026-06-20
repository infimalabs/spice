"""Index page and static asset delivery for the serve UI."""

from __future__ import annotations

import html
import json
import mimetypes
import tomllib
from dataclasses import dataclass
from http import HTTPStatus
from pathlib import Path
from typing import Any

STATIC_ROOT = Path(__file__).resolve().parent / "static"
DEFAULT_BRAND = "spice"


@dataclass(frozen=True)
class ServeBranding:
    name: str


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
    <div class="meta" id="global-status"></div>
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
  <script src="/static/app.render.js"></script>
  <script src="/static/app.stream.js"></script>
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


def serve_branding(repo_root: Path | None = None) -> ServeBranding:
    data = _read_pyproject(repo_root) if repo_root else {}
    tool_spice = _table(data, "tool", "spice")
    serve = _table(tool_spice, "serve")
    project = _table(data, "project")
    name = _string(serve.get("brand")) or _string(project.get("name")) or DEFAULT_BRAND
    return ServeBranding(name=name)


def render_index_html(
    repo_root: Path | None = None, *, branding: ServeBranding | None = None
) -> str:
    resolved = branding or serve_branding(repo_root)
    brand_html = html.escape(resolved.name)
    brand_attr = html.escape(resolved.name, quote=True)
    brand_json = json.dumps({"name": resolved.name}, ensure_ascii=False).replace(
        "</", "<\\/"
    )
    return _INDEX_HTML_TEMPLATE.format(
        brand_html=brand_html,
        brand_attr=brand_attr,
        brand_json=brand_json,
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
    candidate = (STATIC_ROOT / name).resolve()
    if not str(candidate).startswith(str(STATIC_ROOT)) or not candidate.is_file():
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
