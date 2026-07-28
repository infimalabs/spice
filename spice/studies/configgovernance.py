"""Class-level gates for layered configuration safety invariants."""

from __future__ import annotations

import ast
from functools import cache
from pathlib import Path

from spice import paths
from spice.config import layers, schema, trust
from spice.errors import SpiceError

_FALSE_DISABLE_RESOLVERS = frozenset({"enabled_registry_entries", "effective_registry"})
_APPROVAL_GUARD = "require_repository_config_approval"
_COMMAND_STEP_COLLECTION = "_configured_command_steps"
_BUILTIN_STEP_CONFIGURATION = "_configured_builtin_step"


def run_config_key_validity_gate(repo_root: Path) -> None:
    """Validate every active layer and the required packaged source."""
    packaged_path = repo_root / "spice" / "spice.toml"
    if packaged_path.is_file():
        layers.load_packaged_config(packaged_path)
    else:
        layers.load_packaged_config()
    layers.load_config(repo_root)


def run_false_disable_gate(_repo_root: Path) -> None:
    """Prove every declared false-disable registry has a live shared consumer."""
    declared = set(layers.FALSE_DISABLE_REGISTRY_PATHS)
    for registry_path in sorted(declared):
        node = schema.config_schema_at(registry_path)
        if not isinstance(node, schema.TableSchema):
            raise SpiceError(
                "false-disable governance: declared registry "
                f"{'.'.join(registry_path)} is not a configuration table"
            )
        probe = layers.enabled_registry_entries(
            {"config-governance-probe": False}, *registry_path
        )
        if probe:
            raise SpiceError(
                "false-disable governance: shared resolver retained false for "
                f"{'.'.join(registry_path)}"
            )

    observed = _false_disable_consumer_paths(_package_source_root(_repo_root))
    missing = sorted(declared - observed)
    unexpected = sorted(observed - declared)
    if missing or unexpected:
        raise SpiceError(
            "false-disable governance: shared consumer inventory drift; "
            f"missing={_render_paths(missing)}; "
            f"undeclared={_render_paths(unexpected)}"
        )


def run_tracked_file_trust_gate(_repo_root: Path) -> None:
    """Prove every declared executable root is schema-real and guarded."""
    declared = set(trust.EXECUTABLE_REPOSITORY_CONFIG_PATHS)
    for config_path in sorted(declared):
        schema.config_schema_at(config_path)

    observed = _approval_guard_paths(_package_source_root(_repo_root), declared)
    missing = sorted(declared - observed)
    unexpected = sorted(observed - declared)
    if missing or unexpected:
        raise SpiceError(
            "tracked-file trust governance: approval guard inventory drift; "
            f"missing={_render_paths(missing)}; "
            f"undeclared={_render_paths(unexpected)}"
        )


def _render_paths(paths: list[tuple[str, ...]]) -> str:
    return ", ".join(".".join(path) for path in paths) or "<none>"


def _package_source_root(repo_root: Path) -> Path:
    candidate = repo_root.expanduser().resolve() / "spice"
    return candidate if candidate.is_dir() else paths.runtime_spice_source()


@cache
def _parsed_package_sources(
    package_root: Path,
) -> tuple[tuple[Path, ast.Module, dict[str, str]], ...]:
    parsed: list[tuple[Path, ast.Module, dict[str, str]]] = []
    for path in sorted(package_root.rglob("*.py")):
        try:
            source = path.read_text(encoding="utf-8")
            tree = ast.parse(source, filename=str(path))
        except (OSError, SyntaxError) as exc:
            raise SpiceError(
                f"configuration governance could not inspect {path}: {exc}"
            ) from exc
        parsed.append((path, tree, _module_string_constants(tree)))
    return tuple(parsed)


def _module_string_constants(tree: ast.Module) -> dict[str, str]:
    constants: dict[str, str] = {}
    for statement in tree.body:
        if isinstance(statement, ast.Assign) and len(statement.targets) == 1:
            target = statement.targets[0]
            value = _string_value(statement.value, constants)
            if isinstance(target, ast.Name) and value is not None:
                constants[target.id] = value
        elif isinstance(statement, ast.AnnAssign):
            value = _string_value(statement.value, constants)
            if isinstance(statement.target, ast.Name) and value is not None:
                constants[statement.target.id] = value
    return constants


def _call_name(call: ast.Call) -> str:
    function = call.func
    if isinstance(function, ast.Name):
        return function.id
    if isinstance(function, ast.Attribute):
        return function.attr
    return ""


def _string_value(node: ast.expr | None, constants: dict[str, str]) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name):
        return constants.get(node.id)
    return None


def _literal_path(
    nodes: list[ast.expr], constants: dict[str, str]
) -> tuple[str, ...] | None:
    parts: list[str] = []
    for node in nodes:
        value = _string_value(node, constants)
        if value is None:
            return None
        parts.append(value)
    return tuple(parts)


def _path_prefix(node: ast.expr, constants: dict[str, str]) -> tuple[str, ...] | None:
    if not isinstance(node, (ast.Tuple, ast.List)):
        return None
    parts: list[str] = []
    for element in node.elts:
        if isinstance(element, ast.Starred):
            break
        value = _string_value(element, constants)
        if value is None:
            break
        parts.append(value)
    return tuple(parts) or None


def _false_disable_consumer_paths(package_root: Path) -> set[tuple[str, ...]]:
    observed: set[tuple[str, ...]] = set()
    for _path, tree, constants in _parsed_package_sources(package_root):
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            name = _call_name(node)
            if name not in _FALSE_DISABLE_RESOLVERS or len(node.args) < 2:
                continue
            config_path = _literal_path(list(node.args[1:]), constants)
            if config_path is not None:
                observed.add(config_path)
    return observed


def _approval_guard_paths(
    package_root: Path,
    declared: set[tuple[str, ...]],
) -> set[tuple[str, ...]]:
    prefixes: set[tuple[str, ...]] = set()
    for _path, tree, constants in _parsed_package_sources(package_root):
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            name = _call_name(node)
            if name == _APPROVAL_GUARD and len(node.args) >= 2:
                prefix = _path_prefix(node.args[1], constants)
                if prefix is not None:
                    prefixes.add(prefix)
                continue
            if name == _COMMAND_STEP_COLLECTION:
                config_key = next(
                    (
                        _string_value(keyword.value, constants)
                        for keyword in node.keywords
                        if keyword.arg == "config_key"
                    ),
                    None,
                )
                if config_key is not None:
                    prefixes.add(("policy", config_key))
                continue
            if name == _BUILTIN_STEP_CONFIGURATION:
                config_path = next(
                    (
                        _path_prefix(keyword.value, constants)
                        for keyword in node.keywords
                        if keyword.arg == "config_path"
                    ),
                    None,
                )
                if config_path is not None:
                    prefixes.add(config_path)

    return {
        config_path
        for config_path in declared
        if any(
            config_path[: len(prefix)] == prefix
            or prefix[: len(config_path)] == config_path
            for prefix in prefixes
        )
    }
