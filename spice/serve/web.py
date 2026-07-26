"""Index page and static asset delivery for the serve UI."""

from __future__ import annotations

import hashlib
import html
import json
import mimetypes
from collections.abc import Mapping
from dataclasses import dataclass
from http import HTTPStatus
from pathlib import Path
from typing import Any

from spice import defaults
from spice.config.layers import SYSTEM_SOURCE, contextualize_config_error, load_config
from spice.config.pyproject import pyproject_table, read_pyproject
from spice.errors import SpiceError
from spice.serve.payload.wire import validate_emitter_payload
from spice.version import runtime_version

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
  <script src="/static/app.lane-store.js"></script>
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
    data = read_pyproject(repo_root) if repo_root is not None else {}
    project = pyproject_table(data, "project")
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
    brand_json = _page_global_json(
        validate_emitter_payload(
            "web.branding_payload",
            {
                "name": resolved.name,
                "defaultLifetime": resolved.default_lifetime,
                "version": runtime_version(),
            },
        )
    )
    global_settings_json = _page_global_json(
        validate_emitter_payload(
            "web.initial_global_settings_payload",
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
        )
    )
    return _INDEX_HTML_TEMPLATE.format(
        brand_html=brand_html,
        brand_attr=brand_attr,
        brand_json=brand_json,
        global_settings_json=global_settings_json,
    )


def _page_global_json(payload: Any) -> str:
    """A page global as script text that cannot terminate its own element.

    The browser parses ``</`` inside a script as the start of the end tag no
    matter where it sits, so a brand carrying that pair would close the element
    early and spill the rest of the object into the document as markup.
    """
    return json.dumps(payload, ensure_ascii=False).replace("</", "<\\/")


def _string(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


# 32 hex chars == 128 bits of the sha256 digest: ample collision resistance for
# a cache validator while keeping the response header compact.
_ETAG_DIGEST_HEXCHARS = 32


def _asset_etag(body: bytes) -> str:
    """A strong validator that changes only when the asset bytes change."""
    return '"' + hashlib.sha256(body).hexdigest()[:_ETAG_DIGEST_HEXCHARS] + '"'


def _if_none_match_satisfied(if_none_match: str, etag: str) -> bool:
    """Whether a conditional request's If-None-Match covers the current ETag.

    A browser echoes the ETag it was given verbatim, and may send several as a
    comma list or the wildcard ``*``. We emit only strong tags, so exact-string
    membership is the match.
    """
    candidates = [token.strip() for token in if_none_match.split(",")]
    return "*" in candidates or etag in candidates


def send_static_asset(
    handler: Any, name: str, *, if_none_match: str | None = None
) -> None:
    static_root = STATIC_ROOT.resolve()
    candidate = (STATIC_ROOT / name).resolve()
    if not candidate.is_relative_to(static_root) or not candidate.is_file():
        handler.send_error(HTTPStatus.NOT_FOUND)
        return
    body = candidate.read_bytes()
    etag = _asset_etag(body)
    # `no-cache` keeps the current always-fresh guarantee -- the browser
    # revalidates on every load -- while the ETag lets an unchanged asset come
    # back 304 with no body instead of a full re-download. The prior `no-store`
    # forbade caching outright, so every load re-transferred the whole UI bundle.
    if if_none_match is not None and _if_none_match_satisfied(if_none_match, etag):
        handler.send_response(HTTPStatus.NOT_MODIFIED)
        handler.send_header("ETag", etag)
        handler.send_header("Cache-Control", "no-cache")
        handler.end_headers()
        return
    content_type = mimetypes.guess_type(candidate.name)[0] or "application/octet-stream"
    handler.send_response(HTTPStatus.OK)
    handler.send_header("Content-Type", content_type)
    handler.send_header("Content-Length", str(len(body)))
    handler.send_header("ETag", etag)
    handler.send_header("Cache-Control", "no-cache")
    handler.end_headers()
    handler.wfile.write(body)
