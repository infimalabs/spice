"""Canonical layered TOML configuration and source provenance."""

from __future__ import annotations

import re
import tomllib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any

from spice import paths
from spice.config.schema import validate_config_keys
from spice.errors import SpiceError
from spice.scopes import SCOPES_KEY

SYSTEM_SOURCE = "system"
REPOSITORY_SOURCE = "repository"
WORKTREE_SOURCE = "worktree"
_CONFIG_ERROR_TABLE_RE = re.compile(r"^\[(?:tool\.spice\.)?([^\]]+)\]")
_CONFIG_ERROR_CANDIDATE_RE = re.compile(r"^[ .]([A-Za-z0-9_-]+)")
_RETIRED_PYPROJECT_TABLE_RE = re.compile(
    r"^\s*\[\[?\s*tool\.spice(?:[.\]\s])",
    re.MULTILINE,
)
_RETIRED_PYPROJECT_RELEASE = "v0.30"
CONFIG_SCOPE_NAMES = (
    SYSTEM_SOURCE,
    REPOSITORY_SOURCE,
    WORKTREE_SOURCE,
)
# Named entries in these tables inherit across configuration layers and use the
# literal boolean ``false`` as their one removal spelling. Wrapper entries are
# the nested named registry inside each wrapper group; the wildcard is a schema
# path, not a configuration lookup path.
FALSE_DISABLE_REGISTRY_PATHS = (
    ("commands",),
    ("maxims",),
    ("policy", "pre_commit_builtins"),
    ("policy", "taste", "words"),
    ("tasks", "reports"),
    ("wrappers",),
    ("wrappers", "*"),
)


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


_ConfigSource = tuple[str, Path]
_ConfigRevision = tuple[int, int, int, int, int, int] | None
# Cached values are immutable. Concurrent cold reads may redundantly build the
# same revision, but either complete value is safe to publish and config reads
# never acquire a process-wide mutex.
_CONFIG_CACHE: dict[
    tuple[Path, ...], tuple[tuple[_ConfigRevision, ...], LayeredConfig]
] = {}
_RETIRED_PYPROJECT_REVISIONS: dict[Path, _ConfigRevision] = {}


def load_config(repo_root: Path) -> LayeredConfig:
    """Load three immutable TOML layers, reusing them while sources are unchanged."""
    _reject_retired_pyproject_config(repo_root)
    specifications: tuple[_ConfigSource, ...] = (
        (
            SYSTEM_SOURCE,
            paths.runtime_spice_source() / "spice.toml",
        ),
        (REPOSITORY_SOURCE, repo_root / "spice.toml"),
        (
            WORKTREE_SOURCE,
            paths.state_dir(repo_root) / "config" / "spice.toml",
        ),
    )
    revisions = tuple(_config_revision(name, path) for name, path in specifications)
    cache_key = tuple(path for _name, path in specifications)
    cached = _CONFIG_CACHE.get(cache_key)
    if cached is not None and cached[0] == revisions:
        return cached[1]
    loaded = _load_config_sources(specifications)
    _CONFIG_CACHE[cache_key] = (revisions, loaded)
    return loaded


def _config_revision(source_name: str, path: Path) -> _ConfigRevision:
    try:
        stat = path.stat()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise SpiceError(
            f"cannot read configuration source={source_name} path={path}: {exc}"
        ) from exc
    return (
        stat.st_dev,
        stat.st_ino,
        stat.st_mode,
        stat.st_size,
        stat.st_mtime_ns,
        stat.st_ctime_ns,
    )


def _load_config_sources(
    specifications: tuple[_ConfigSource, ...],
) -> LayeredConfig:
    parsed: list[dict[str, Any]] = []
    layers: list[ConfigLayer] = []
    for name, path in specifications:
        values, present = _read_toml(path, name)
        if name == SYSTEM_SOURCE and not present:
            raise SpiceError(f"packaged configuration is missing: {path}")
        parsed.append(values)
        layers.append(
            ConfigLayer(
                name=name,
                path=path,
                values=_freeze_mapping(values),
                present=present,
            )
        )

    for layer, values in reversed(tuple(zip(layers, parsed, strict=True))):
        validate_config_keys(
            values,
            source_name=layer.name,
            source_path=layer.path,
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
    values, present = _read_toml(path, SYSTEM_SOURCE)
    if not present:
        raise SpiceError(f"packaged configuration is missing: {path}")
    validate_config_keys(values, source_name=SYSTEM_SOURCE, source_path=path)
    return ConfigLayer(
        name=SYSTEM_SOURCE,
        path=path,
        values=_freeze_mapping(values),
        present=True,
    )


def effective_mapping(repo_root: Path | None) -> dict[str, Any]:
    """Return a mutable consumer view of the canonical effective mapping."""
    values = (
        load_config(repo_root).effective
        if repo_root is not None
        else load_packaged_config().values
    )
    return _thaw_mapping(values)


def effective_table(repo_root: Path | None, *path: str) -> dict[str, Any]:
    """Return one mutable effective table, or an empty table for a non-table."""
    value: Any = effective_mapping(repo_root)
    for part in path:
        if not isinstance(value, Mapping):
            return {}
        value = value.get(part)
    return value if isinstance(value, dict) else {}


def enabled_registry_entries(
    entries: Mapping[str, Any], *registry_path: str
) -> dict[str, Any]:
    """Return enabled named entries from one declared configuration registry."""
    path = tuple(registry_path)
    if path not in FALSE_DISABLE_REGISTRY_PATHS:
        raise SpiceError(
            "configuration registry "
            f"{'.'.join(path)!r} does not declare false-disable semantics"
        )
    return {str(name): value for name, value in entries.items() if value is not False}


def effective_registry(repo_root: Path | None, *path: str) -> dict[str, Any]:
    """Return one effective named-entry table with false entries removed."""
    return enabled_registry_entries(effective_table(repo_root, *path), *path)


def layer_table(repo_root: Path, layer_name: str, *path: str) -> dict[str, Any]:
    """Return one mutable table from a specific named configuration layer."""
    value: Any = load_config(repo_root).layer(layer_name).values
    for part in path:
        if not isinstance(value, Mapping):
            return {}
        value = value.get(part)
    return _thaw_mapping(value) if isinstance(value, Mapping) else {}


def effective_commands(repo_root: Path | None) -> dict[str, Any]:
    """Return effective mounted commands flattened to dotted command names."""
    flattened: dict[str, Any] = {}
    _flatten_mapping(effective_table(repo_root, "commands"), flattened)
    return enabled_registry_entries(flattened, "commands")


def config_string_list(raw: Any) -> list[str]:
    """Normalize one effective TOML list to unique non-empty strings."""
    if not isinstance(raw, Sequence) or isinstance(raw, (str, bytes)):
        return []
    values: list[str] = []
    for item in raw:
        value = str(item or "").strip()
        if value and value not in values:
            values.append(value)
    return values


def effective_context(repo_root: Path, *path: str) -> str:
    """Identify an effective dotted key and its winning source for diagnostics."""
    dotted = ".".join(path)
    source = load_config(repo_root).source_for(path)
    if source is None:
        return dotted
    return f"{dotted} (source={source.name} path={source.path})"


def contextualize_config_error(
    repo_root: Path, exc: SpiceError, *fallback_path: str
) -> SpiceError:
    """Attach the effective key and winning layer to a consumer validation error."""
    message = str(exc)
    if "source=" in message and " path=" in message:
        return SpiceError(message)
    path = fallback_path
    detail = message
    table_match = _CONFIG_ERROR_TABLE_RE.match(message)
    if table_match is not None:
        table = table_match.group(1)
        parsed = tuple(part for part in (table or "").split(".") if part)
        remainder = message[table_match.end() :]
        candidate_match = _CONFIG_ERROR_CANDIDATE_RE.match(remainder)
        candidate = candidate_match.group(1) if candidate_match is not None else ""
        candidate_path = (*parsed, candidate) if candidate else ()
        loaded = load_config(repo_root)
        if (
            candidate_match is not None
            and loaded.source_for(candidate_path) is not None
        ):
            path = candidate_path
            detail = remainder[candidate_match.end() :].lstrip(" .:") or message
        else:
            path = parsed or fallback_path
            detail = remainder.lstrip(" .:") or message
    return SpiceError(f"{effective_context(repo_root, *path)}: {detail}")


def _read_toml(path: Path, source_name: str) -> tuple[dict[str, Any], bool]:
    try:
        with path.open("rb") as handle:
            loaded = tomllib.load(handle)
    except FileNotFoundError:
        return {}, False
    except tomllib.TOMLDecodeError as exc:
        raise SpiceError(
            f"invalid TOML for configuration source={source_name} path={path}: {exc}"
        ) from exc
    except OSError as exc:
        raise SpiceError(
            f"cannot read configuration source={source_name} path={path}: {exc}"
        ) from exc
    return loaded, True


def _reject_retired_pyproject_config(repo_root: Path) -> None:
    """Refuse the one retired repository shape without loading it as a layer."""
    path = repo_root / "pyproject.toml"
    try:
        revision = _config_revision("retired-pyproject", path)
    except SpiceError:
        return
    if revision is None or _RETIRED_PYPROJECT_REVISIONS.get(path) == revision:
        return
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return
    if _RETIRED_PYPROJECT_TABLE_RE.search(text):
        replacement = repo_root / "spice.toml"
        raise SpiceError(
            f"{_RETIRED_PYPROJECT_RELEASE} dropped [tool.spice] configuration "
            f"from {path}; move that table's contents to {replacement} and remove "
            "the tool.spice prefix"
        )
    _RETIRED_PYPROJECT_REVISIONS[path] = revision


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
        if isinstance(value, Mapping) and (
            key == SCOPES_KEY or prefix == ("wrappers",)
        ):
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


def _thaw_mapping(raw: Mapping[str, Any]) -> dict[str, Any]:
    return {str(key): _thaw(value) for key, value in raw.items()}


def _thaw(raw: Any) -> Any:
    if isinstance(raw, Mapping):
        return _thaw_mapping(raw)
    if isinstance(raw, Sequence) and not isinstance(raw, (str, bytes)):
        return [_thaw(item) for item in raw]
    return raw


def _flatten_mapping(
    source: Mapping[str, Any], destination: dict[str, Any], *, prefix: str = ""
) -> None:
    for key, value in source.items():
        name = f"{prefix}.{key}" if prefix else str(key)
        if isinstance(value, Mapping):
            _flatten_mapping(value, destination, prefix=name)
        else:
            destination[name] = value
