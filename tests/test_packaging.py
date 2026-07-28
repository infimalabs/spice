"""Packaging data-file completeness contracts.

The serve UI loads static assets by URL (`/static/...`). Those files only reach
an installed wheel if a `[tool.setuptools.package-data]` glob for
`spice.serve.static` matches them, so a referenced asset with no matching glob
ships broken. This guards that contract without building a wheel.
"""

from __future__ import annotations

import fnmatch
import re
import subprocess
import tomllib
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SERVE_STATIC_DIR = PROJECT_ROOT / "spice" / "serve" / "static"
STATIC_REF_RE = re.compile(r"/static/([A-Za-z0-9_./-]+)")
PRIMARY_RUNTIME_DOCS = ("README.md", "DESIGN.md", "CONFIG.md")
BROWSER_VALIDATION_FILES = (
    "package.json",
    "package-lock.json",
    "tests/browser/release_smoke_manifest.js",
    "tests/browser/run_release_smokes.js",
    "tests/browser/serve_composer_reorder_smoke.js",
    "tests/browser/serve_identity_smoke.js",
    "tests/browser/serve_lifetime_team_smoke.js",
    "tests/browser/serve_menu_smoke.js",
    "tests/browser/serve_pending_badge_smoke.js",
    "tests/browser/serve_playwright_harness.js",
    "tests/browser/serve_structural_status_smoke.js",
    "tests/browser/serve_submission_lifecycle_smoke.js",
    "tests/browser/serve_submit_latency_smoke.js",
    "tests/browser/serve_task_card_live_smoke.js",
    "tests/browser/serve_team_metrics_smoke.js",
)
CURRENT_DOC_SPELLING_GATE = PROJECT_ROOT / "scripts/check-current-doc-spellings"


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


def test_project_metadata_declares_taskwarrior_three_for_task_plane():
    description = _pyproject_data()["project"]["description"]

    assert "task plane requires Taskwarrior 3" in description


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
    assert "editable main tree is the server deployment" in install_text
    assert "worktrees remain operated trees" in install_text
    assert "Taskwarrior 3 or newer is required for the task plane" in install_text


def test_entry_ladder_places_taskwarrior_requirement_at_fleet():
    overview = (PROJECT_ROOT / "docs" / "overview.md").read_text(encoding="utf-8")
    ladder = overview.split("## Entry Ladder", maxsplit=1)[1].split(
        "## Honest Feedback", maxsplit=1
    )[0]
    ladder_text = _collapsed(ladder)

    assert "**Fleet** | A successful steered lane, Taskwarrior 3 or newer" in ladder
    assert "Watch, Retrospect, and Gates do not require Taskwarrior" in ladder_text
    assert "records worktree-local approval" in ladder_text
    assert "another clone or worktree records its own approval" in ladder_text


def test_current_documentation_spelling_gate_is_configured_and_clean():
    with (PROJECT_ROOT / "spice.toml").open("rb") as handle:
        repository_config = tomllib.load(handle)
    steps = repository_config["policy"]["pre_commit"]

    assert steps == [
        {
            "label": "current documentation spellings",
            "run": ["scripts/check-current-doc-spellings"],
            "scopes": {
                "paths": [
                    "README.md",
                    "DESIGN.md",
                    "CONFIG.md",
                    "STABILITY.md",
                    "docs/cli/wrapper-commands.md",
                    "docs/config/reference.md",
                    "docs/overview.md",
                    "docs/release.md",
                    "scripts/check-current-doc-spellings",
                    "spice.toml",
                ]
            },
        }
    ]
    completed = subprocess.run(
        [str(CURRENT_DOC_SPELLING_GATE)],
        cwd=PROJECT_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode == 0, completed.stderr
    assert completed.stderr == ""


def test_current_documentation_spelling_gate_fails_on_every_withdrawn_surface(
    tmp_path,
):
    subprocess.run(["git", "init", "--quiet", str(tmp_path)], check=True)
    readme = tmp_path / "README.md"
    readme.write_text("Run `spice init` to preview.\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=tmp_path, check=True)

    clean = subprocess.run(
        [str(CURRENT_DOC_SPELLING_GATE), "README.md"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )
    assert clean.returncode == 0

    for withdrawn in (
        "spice init --dry-run",
        "spice deinit",
        ".spice/init-receipt.json",
        "[tool.spice.agent]",
    ):
        readme.write_text(f"{withdrawn}\n", encoding="utf-8")
        subprocess.run(["git", "add", "README.md"], cwd=tmp_path, check=True)
        refused = subprocess.run(
            [str(CURRENT_DOC_SPELLING_GATE), "README.md"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
        )

        assert refused.returncode == 1
        assert "current documentation names a withdrawn spelling" in refused.stderr
        assert "README.md:1" in refused.stderr
        assert withdrawn in refused.stderr


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


def test_config_documents_runtime_model_as_non_configurable():
    config = (PROJECT_ROOT / "CONFIG.md").read_text(encoding="utf-8")
    runtime_section = config.split("## Runtime Model", maxsplit=1)[1].split(
        "## `[agent]`", maxsplit=1
    )[0]
    runtime_text = _collapsed(runtime_section)

    assert "Runtime is not a per-repo config surface" in runtime_section
    assert "uv tool" in runtime_text
    assert "`uv tool install -e /path/to/spice-main`" in runtime_section
    assert "editable main tree the server deployment" in runtime_text
    assert "Worker worktrees are operated trees" in runtime_text
    assert "Taskwarrior 3 or newer is a separate system requirement" in runtime_text
    assert "not a Python package or per-repo setting" in runtime_text


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


def test_accepted_runtime_model_requires_installed_tree_evidence():
    runtime = (
        PROJECT_ROOT / "docs/design/accepted/single-install-runtime-model.md"
    ).read_text(encoding="utf-8")
    boundary = runtime.split("## Deployment Evidence Boundary", maxsplit=1)[1].split(
        "## Per-Tree-Runtime Magic Removed", maxsplit=1
    )[0]
    text = _collapsed(boundary)

    assert "no fleet effect" in text
    assert "team-authority schema version" in text
    assert "_refresh_generated_skill_after_advance" in boundary
    assert "/path/to/installed/python -P -c" in boundary
    assert "b.__file__" in boundary
    assert "interpreter inside the candidate worktree" in text
    assert "Release and task evidence must not claim" in text
    assert "read_bytes() == Path(...).read_bytes()" in boundary
    assert "human-oriented diff" in text
