"""Typed access to Spice's packaged static defaults."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from functools import cache
from typing import Any, cast

from spice import defaultinventory
from spice.errors import SpiceError


@cache
def packaged_values() -> Mapping[str, Any]:
    """The immutable canonical values installed in ``spice/spice.toml``."""
    # Function-level import: spice.config.values evaluates packaged defaults
    # while the spice.config package is still initializing, so a module-scope
    # binding here would be circular.
    from spice.config.layers import load_packaged_config

    return load_packaged_config().values


def export_classifications() -> dict[str, str]:
    """The diagnostic classification of every inventoried exported default."""
    return dict(defaultinventory.EXPORTED_DEFAULT_CLASSIFICATION)


def value(*path: str) -> Any:
    current: Any = packaged_values()
    for part in path:
        if not isinstance(current, Mapping) or part not in current:
            raise SpiceError(f"packaged configuration is missing {'.'.join(path)}")
        current = current[part]
    return current


def table(*path: str) -> Mapping[str, Any]:
    raw = value(*path)
    if not isinstance(raw, Mapping):
        raise SpiceError(f"packaged configuration {'.'.join(path)} must be a table")
    return cast(Mapping[str, Any], raw)


def string(*path: str) -> str:
    raw = value(*path)
    if not isinstance(raw, str):
        raise SpiceError(f"packaged configuration {'.'.join(path)} must be a string")
    return raw


def integer(*path: str) -> int:
    raw = value(*path)
    if not isinstance(raw, int) or isinstance(raw, bool):
        raise SpiceError(f"packaged configuration {'.'.join(path)} must be an integer")
    return raw


def number(*path: str) -> float:
    raw = value(*path)
    if not isinstance(raw, (int, float)) or isinstance(raw, bool):
        raise SpiceError(f"packaged configuration {'.'.join(path)} must be numeric")
    return float(raw)


def strings(*path: str) -> tuple[str, ...]:
    raw = value(*path)
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        raise SpiceError(f"packaged configuration {'.'.join(path)} must be a list")
    if not all(isinstance(item, str) for item in raw):
        raise SpiceError(
            f"packaged configuration {'.'.join(path)} must contain only strings"
        )
    return tuple(raw)
