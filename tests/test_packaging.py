"""Packaging data-file completeness contracts.

The serve UI loads static assets by URL (`/static/...`). Those files only reach
an installed wheel if a `[tool.setuptools.package-data]` glob for
`spice.serve.static` matches them, so a referenced asset with no matching glob
ships broken. This guards that contract without building a wheel.
"""

from __future__ import annotations

import fnmatch
import re
import tomllib
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SERVE_STATIC_DIR = PROJECT_ROOT / "spice" / "serve" / "static"
STATIC_REF_RE = re.compile(r"/static/([A-Za-z0-9_./-]+)")
PRIMARY_RUNTIME_DOCS = ("README.md", "DESIGN.md", "CONFIG.md")
BROWSER_VALIDATION_FILES = (
    "package.json",
    "package-lock.json",
    "tests/browser/serve_composer_reorder_smoke.js",
    "tests/browser/serve_identity_smoke.js",
    "tests/browser/serve_lifetime_team_smoke.js",
    "tests/browser/serve_menu_smoke.js",
    "tests/browser/serve_pending_badge_smoke.js",
    "tests/browser/serve_playwright_harness.js",
    "tests/browser/serve_task_card_live_smoke.js",
    "tests/browser/serve_team_metrics_smoke.js",
)


def _pyproject_data():
    return tomllib.loads((PROJECT_ROOT / "pyproject.toml").read_text(encoding="utf-8"))


def _serve_static_globs() -> list[str]:
    data = _pyproject_data()
    return data["tool"]["setuptools"]["package-data"]["spice.serve.static"]


def _referenced_static_assets() -> set[str]:
    sources = [PROJECT_ROOT / "spice" / "serve" / "web.py"]
    sources += sorted(SERVE_STATIC_DIR.glob("*.js"))
    sources += sorted(SERVE_STATIC_DIR.glob("*.webmanifest"))
    refs: set[str] = set()
    for source in sources:
        for match in STATIC_REF_RE.finditer(source.read_text(encoding="utf-8")):
            refs.add(match.group(1))
    return refs


def _package_data_matches(glob: str, asset: str) -> bool:
    # setuptools package-data globs are per path segment: `*` does not cross a
    # directory separator, so `*.svg` ships `claude.svg` but not `icons/x.svg`.
    glob_parts = glob.split("/")
    asset_parts = asset.split("/")
    if len(glob_parts) != len(asset_parts):
        return False
    return all(
        fnmatch.fnmatch(asset_part, glob_part)
        for glob_part, asset_part in zip(glob_parts, asset_parts)
    )


def _collapsed(text: str) -> str:
    return " ".join(text.split())


def test_referenced_static_assets_are_declared_in_package_data():
    globs = _serve_static_globs()
    referenced = _referenced_static_assets()
    assert referenced, "expected to find /static/ asset references in serve source"
    for asset in sorted(referenced):
        assert (SERVE_STATIC_DIR / asset).is_file(), (
            f"/static/{asset} is referenced but missing from the static dir"
        )
        assert any(_package_data_matches(glob, asset) for glob in globs), (
            f"/static/{asset} is referenced but no spice.serve.static "
            f"package-data glob ships it: {globs}"
        )


def test_uv_tool_install_contract_declares_spice_console_script():
    data = _pyproject_data()

    assert data["project"]["name"] == "spice-harness"
    assert data["project"]["scripts"]["spice"] == "spice.cli.entry:main"


def test_sdist_includes_browser_validation_inputs():
    manifest = (PROJECT_ROOT / "MANIFEST.in").read_text(encoding="utf-8")

    assert "include package.json" in manifest
    assert "include package-lock.json" in manifest
    assert "recursive-include tests/browser *.js" in manifest
    for relative in BROWSER_VALIDATION_FILES:
        assert (PROJECT_ROOT / relative).is_file(), (
            f"{relative} must ship in the sdist so extracted test runs keep "
            "browser validation context"
        )


def test_readme_documents_single_install_runtime_model():
    readme = (PROJECT_ROOT / "README.md").read_text(encoding="utf-8")
    install_section = readme.split("## Install", maxsplit=1)[1].split(
        "### Graceful degradation", maxsplit=1
    )[0]
    install_text = _collapsed(install_section)

    assert "uv tool install -e /path/to/spice-main" in install_section
    assert "uv tool install spice-harness" in install_section
    assert "pip install spice-harness" not in install_section
    assert "UV_TOOL_DIR" in install_section
    assert "UV_TOOL_BIN_DIR" in install_section
    assert "editable main tree is the server deployment" in install_text
    assert "worktrees remain operated trees" in install_text
    assert "common-dir layout is opt-in" in install_text


def test_design_documents_single_install_runtime_model():
    design = (PROJECT_ROOT / "DESIGN.md").read_text(encoding="utf-8")
    principle = design.split("0. **Standalone product", maxsplit=1)[1].split(
        "\n1. **The driver seam.**", maxsplit=1
    )[0]
    principle_text = _collapsed(principle)

    assert "`uv tool install spice-harness`" in principle
    assert "`uv tool install -e /path/to/spice-main`" in principle
    assert "editable main tree is the server deployment" in principle_text
    assert "Worker worktrees are operated trees" in principle_text
    assert "common-dir layout remains an opt-in install shape" in principle_text


def test_config_documents_runtime_model_as_non_configurable():
    config = (PROJECT_ROOT / "CONFIG.md").read_text(encoding="utf-8")
    runtime_section = config.split("## Runtime Model", maxsplit=1)[1].split(
        "## `[tool.spice.agent]`", maxsplit=1
    )[0]
    runtime_text = _collapsed(runtime_section)

    assert "Runtime is not a per-repo config surface" in runtime_section
    assert "uv tool" in runtime_text
    assert "`uv tool install -e /path/to/spice-main`" in runtime_section
    assert "editable main tree the server deployment" in runtime_text
    assert "Worker worktrees are operated trees" in runtime_text
    assert "common-dir layout is opt-in" in runtime_text


def test_primary_runtime_docs_do_not_describe_per_tree_runtime_magic():
    forbidden = (
        "worktree-source-checkout-precedence",
        "worktree-true",
        "source checkout or target virtualenv",
        "target virtualenv",
        "PYTHONPATH",
    )

    offenders: list[str] = []
    for relative in PRIMARY_RUNTIME_DOCS:
        text = (PROJECT_ROOT / relative).read_text(encoding="utf-8")
        for token in forbidden:
            if token in text:
                offenders.append(f"{relative}: {token}")

    assert offenders == []
