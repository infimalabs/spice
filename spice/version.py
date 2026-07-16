"""Installed Spice runtime version."""

from __future__ import annotations

from importlib import metadata


def runtime_version() -> str:
    """Return the version of the active installed Spice distribution."""
    return metadata.version("spice-harness")
