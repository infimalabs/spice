"""Canonical layered TOML configuration and source provenance."""

from __future__ import annotations

import tomllib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

from spice import paths
from spice.errors import SpiceError

PACKAGED_SOURCE = "packaged"
PYPROJECT_SOURCE = "pyproject"
REPOSITORY_SOURCE = "repository"
WORKTREE_SOURCE = "worktree"


@dataclass(frozen=True)
class ConfigLayer:
    """One immutable parsed source in the configuration precedence chain."""

    name: str
    path: Path
    values: Mapping[str, Any]
    present: bool


@dataclass(frozen=True)
class LayeredConfig:
    """The source layers, recursively merged values, and winning sources."""

    layers: tuple[ConfigLayer, ...]
    effective: Mapping[str, Any]
    sources: Mapping[tuple[str, ...], ConfigLayer]

    def layer(self, name: str) -> ConfigLayer:
        """Return the named layer, including an explicit empty absent layer."""
        for layer in self.layers:
            if layer.name == name:
                return layer
        raise KeyError(name)

    def source_for(self, key: str | Sequence[str]) -> ConfigLayer | None:
        """Return the winning layer for a dotted or explicitly segmented key."""
        parts = tuple(key.split(".")) if isinstance(key, str) else tuple(key)
        return self.sources.get(parts)


def load_config(repo_root: Path) -> LayeredConfig:
    """Load the four Spice TOML layers in increasing precedence order."""
    packaged = load_packaged_config()
    specifications = (
        (PYPROJECT_SOURCE, repo_root / "pyproject.toml", True),
        (REPOSITORY_SOURCE, repo_root / "spice.toml", False),
        (
            WORKTREE_SOURCE,
            paths.state_dir(repo_root) / "config" / "spice.toml",
            False,
        ),
    )
    parsed: list[dict[str, Any]] = [dict(packaged.values)]
    layers: list[ConfigLayer] = [packaged]
    for name, path, pyproject in specifications:
        values, present = _read_toml(path)
        if pyproject:
            values = _pyproject_spice_table(values)
        parsed.append(values)
        layers.append(
            ConfigLayer(
                name=name,
                path=path,
                values=_freeze_mapping(values),
                present=present,
            )
        )

    effective: dict[str, Any] = {}
    sources: dict[tuple[str, ...], ConfigLayer] = {}
    for layer, values in zip(layers, parsed, strict=True):
        _merge_mapping(effective, values, layer, sources)
    return LayeredConfig(
        layers=tuple(layers),
        effective=_freeze_mapping(effective),
        sources=MappingProxyType(dict(sources)),
    )


def load_packaged_config() -> ConfigLayer:
    """Load the required installed default layer from its canonical path."""
    path = paths.runtime_spice_source() / "spice.toml"
    values, present = _read_toml(path)
    if not present:
        raise SpiceError(f"packaged configuration is missing: {path}")
    return ConfigLayer(
        name=PACKAGED_SOURCE,
        path=path,
        values=_freeze_mapping(values),
        present=True,
    )


def _read_toml(path: Path) -> tuple[dict[str, Any], bool]:
    try:
        with path.open("rb") as handle:
            loaded = tomllib.load(handle)
    except FileNotFoundError:
        return {}, False
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(f"invalid TOML in {path}: {exc}") from exc
    except OSError as exc:
        raise SpiceError(f"cannot read configuration {path}: {exc}") from exc
    return loaded, True


def _pyproject_spice_table(values: Mapping[str, Any]) -> dict[str, Any]:
    tool = values.get("tool")
    if not isinstance(tool, Mapping):
        return {}
    spice = tool.get("spice")
    return dict(spice) if isinstance(spice, Mapping) else {}


def _merge_mapping(
    destination: dict[str, Any],
    incoming: Mapping[str, Any],
    layer: ConfigLayer,
    sources: dict[tuple[str, ...], ConfigLayer],
    prefix: tuple[str, ...] = (),
) -> None:
    for key, value in incoming.items():
        path = (*prefix, key)
        previous = destination.get(key)
        if isinstance(value, Mapping) and prefix == ("wrappers",):
            _forget_sources(sources, path)
            replacement: dict[str, Any] = {}
            destination[key] = replacement
            sources[path] = layer
            _merge_mapping(replacement, value, layer, sources, path)
            continue
        if isinstance(value, Mapping):
            sources[path] = layer
            if not isinstance(previous, dict):
                _forget_sources(sources, path)
                sources[path] = layer
                previous = {}
                destination[key] = previous
            _merge_mapping(previous, value, layer, sources, path)
            continue
        _forget_sources(sources, path)
        destination[key] = value
        sources[path] = layer


def _forget_sources(
    sources: dict[tuple[str, ...], ConfigLayer], prefix: tuple[str, ...]
) -> None:
    for path in tuple(sources):
        if path[: len(prefix)] == prefix:
            del sources[path]


def _freeze_mapping(values: Mapping[str, Any]) -> Mapping[str, Any]:
    return MappingProxyType({key: _freeze(value) for key, value in values.items()})


def _freeze(value: Any) -> Any:
    if isinstance(value, Mapping):
        return _freeze_mapping(value)
    if isinstance(value, list):
        return tuple(_freeze(item) for item in value)
    return value
