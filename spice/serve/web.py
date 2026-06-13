"""Index page and static asset delivery for the serve UI."""

from __future__ import annotations

import mimetypes
from http import HTTPStatus
from pathlib import Path
from typing import Any

STATIC_ROOT = Path(__file__).resolve().parent / "static"

_INDEX_HTML = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>spice</title>
  <link rel="apple-touch-icon" sizes="180x180" href="/static/apple-touch-icon.png">
  <link rel="icon" type="image/png" sizes="32x32" href="/static/favicon-32x32.png">
  <link rel="icon" type="image/png" sizes="16x16" href="/static/favicon-16x16.png">
  <link rel="icon" href="/static/favicon.ico" sizes="any">
  <link rel="manifest" href="/static/site.webmanifest">
  <link rel="stylesheet" href="/static/index.css">
  <link rel="stylesheet" href="/static/status-colors.css">
</head>
<body>
  <header class="app-header">
    <div id="filter-strip" class="filter-strip" aria-hidden="true"></div>
    <div class="meta" id="global-status"></div>
    <button id="open-lane" class="spice-menu-button" type="button"
            title="Open spice menu" aria-label="Open spice menu"
            aria-haspopup="menu" aria-expanded="false">
      <span class="spice-menu-icon" aria-hidden="true">🌶️</span>
      <span class="spice-menu-label">spice</span>
    </button>
  </header>
  <main id="swimlanes" class="swimlanes" aria-label="Open trees"></main>
  <script src="/static/app.render.js"></script>
  <script src="/static/app.stream.js"></script>
  <script src="/static/app.lanes.js"></script>
  <script src="/static/app.shell.js"></script>
  <script src="/static/app.controls.js"></script>
  <script src="/static/app.panes.js"></script>
  <script src="/static/app.groups.js"></script>
  <script src="/static/app.audio.js"></script>
  <script src="/static/app.js"></script>
</body>
</html>
"""


def render_index_html() -> str:
    return _INDEX_HTML


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
