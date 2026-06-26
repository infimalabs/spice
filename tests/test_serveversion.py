"""Serve UI surfaces the running spice runtime version.

The version must come from the active installed distribution (the running
tool), not a hard-coded or worktree-derived string, and reach the UI through the
served page so the global menu can render it. See
docs/studies/single-install-runtime-model.md — the installed tool is the single
coherent running code.
"""

from __future__ import annotations

import json
import re
import subprocess
from importlib import metadata
from pathlib import Path

from spice.serve.web import STATIC_ROOT, render_index_html, spice_runtime_version


def test_runtime_version_matches_installed_distribution():
    assert spice_runtime_version() == metadata.version("spice-harness")


def test_index_html_injects_runtime_version_into_branding():
    html = render_index_html()
    match = re.search(r"const spiceServeBranding = (\{.*?\});", html)
    assert match, "branding blob is injected into the served page"
    branding = json.loads(match.group(1))
    assert branding["version"] == spice_runtime_version()


def test_runtime_version_falls_back_to_empty_when_not_installed(monkeypatch):
    def raise_not_found(_name: str) -> str:
        raise metadata.PackageNotFoundError("spice-harness")

    monkeypatch.setattr(metadata, "version", raise_not_found)
    assert spice_runtime_version() == ""


def test_menu_renders_runtime_version_footer():
    fixture = Path(__file__).with_name("fixtures") / "serve_menu_version.js"
    result = subprocess.run(
        ["node", str(fixture), str(STATIC_ROOT / "app.menu.js")],
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.stdout.strip() == "ok"
