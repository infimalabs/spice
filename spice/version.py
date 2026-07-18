"""Installed Spice runtime version."""

from __future__ import annotations

from importlib import metadata

DISTRIBUTION_NAME = "spice-harness"


def runtime_version() -> str:
    """Return the version of the active installed Spice distribution."""
    return metadata.version(DISTRIBUTION_NAME)
