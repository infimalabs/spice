"""Installed Spice runtime version."""

from __future__ import annotations

from importlib import metadata

DISTRIBUTION_NAME = "spice-harness"
SOURCE_LOOP_VERSION = "0.0.0+source"


def runtime_version() -> str:
    """Return the installed distribution version or the source-loop fallback."""
    try:
        return metadata.version(DISTRIBUTION_NAME)
    except metadata.PackageNotFoundError:
        return SOURCE_LOOP_VERSION
