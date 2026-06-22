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


def _serve_static_globs() -> list[str]:
    data = tomllib.loads((PROJECT_ROOT / "pyproject.toml").read_text(encoding="utf-8"))
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
