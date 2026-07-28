"""Serve UI surfaces the running spice runtime version.

The version must come from the active installed distribution (the running
tool), not a hard-coded or worktree-derived string, and reach the UI through the
served page so the global menu can render it. See
docs/design/accepted/single-install-runtime-model.md — the installed tool is the single
coherent running code.
"""

from __future__ import annotations

import http.client
import json
import re
import subprocess
import threading
from http import HTTPStatus
from importlib import metadata
from pathlib import Path

import pytest

from spice.serve import app
from spice.serve.web import STATIC_ROOT, render_index_html
from spice.version import SOURCE_LOOP_VERSION, runtime_version


def test_index_html_injects_runtime_version_into_branding():
    html = render_index_html()
    match = re.search(r"const spiceServeBranding = (\{.*?\});", html)
    assert match, "branding blob is injected into the served page"
    branding = json.loads(match.group(1))
    assert branding["version"] == runtime_version()


def test_server_serves_index_when_distribution_metadata_is_missing(
    tmp_path, monkeypatch
):
    def missing_distribution(_distribution_name: str) -> str:
        raise metadata.PackageNotFoundError("spice-harness")

    monkeypatch.setattr("spice.version.metadata.version", missing_distribution)
    state = app.ServeState(anchor_root=tmp_path)
    server = app._ServeHttpServer(("127.0.0.1", 0), app._ServeHandler, state)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    host, port = server.server_address[:2]

    try:
        connection = http.client.HTTPConnection(host, port, timeout=2)
        connection.request("GET", "/")
        response = connection.getresponse()
        html = response.read().decode("utf-8")
        connection.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

    match = re.search(r"const spiceServeBranding = (\{.*?\});", html)
    assert response.status == HTTPStatus.OK
    assert match, "branding blob is injected into the served page"
    assert json.loads(match.group(1))["version"] == SOURCE_LOOP_VERSION


def test_index_html_does_not_mask_unrelated_metadata_failures(monkeypatch):
    def fail_version_lookup(_distribution_name: str) -> str:
        raise RuntimeError("metadata backend failed")

    monkeypatch.setattr("spice.version.metadata.version", fail_version_lookup)

    with pytest.raises(RuntimeError, match="metadata backend failed"):
        render_index_html()


def test_menu_renders_runtime_version_footer():
    fixture = Path(__file__).with_name("fixtures") / "serve_menu_version.js"
    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.menu.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.stdout.strip() == "ok"
