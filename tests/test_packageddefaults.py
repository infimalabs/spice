"""Packaged defaults, classification inventory, and wheel contracts."""

from __future__ import annotations

import ast
import importlib
import json
import re
import shutil
import subprocess
import tomllib
import zipfile
from collections.abc import Mapping, Sequence
from pathlib import Path

import pytest

from spice import defaultinventory, defaults, paths
from spice.config import layers, schema, values
from spice.config.layers import SYSTEM_SOURCE, load_packaged_config

PROJECT_ROOT = Path(__file__).resolve().parents[1]
_TOML_BARE_KEY_RE = re.compile(r"^[A-Za-z0-9_-]+$")
_DEFAULT_ACCESSORS = frozenset(
    {"integer", "number", "packaged_values", "string", "strings", "table", "value"}
)
_FUNCTION_SCOPE_PACKAGED_READ = (
    "spice/serve/web.py",
    "serve_branding",
    "table",
)
_FROZEN_DEFAULT_RESOLVER_FUNCTIONS = frozenset(
    {
        "spice.agent.driver.resolve_claude_model",
        "spice.agent.lifecycle._requested_launch_knobs",
        "spice.agent.maxims.render_maxim_prompt",
        "spice.config.values.configured_agent_personality",
        "spice.config.values.configured_say_backend",
        "spice.config.values.default_judge_bin",
        "spice.hooks.commitmsg.fold_commit_message_text",
        "spice.hooks.commitmsg.validate_commit_message_text",
        "spice.policyconfig._env_access",
        "spice.policyconfig._file_shape_paths",
        "spice.policyconfig._flex_jitter_percent",
        "spice.policyconfig._languages",
        "spice.policyconfig._lockfiles",
        "spice.policyconfig._markdown_depth_budget",
        "spice.policyconfig._policy_debt",
        "spice.policyconfig._policy_environment",
        "spice.policyconfig._policy_flex",
        "spice.policyconfig._policy_limits",
        "spice.policyconfig._policy_magic",
        "spice.policyconfig._policy_markdown_depth",
        "spice.policyconfig._resolve_policy",
        "spice.policyconfig._taste",
        "spice.policyconfig.jittered_flex_limit",
        "spice.resourcelocks.configured_lock_settings",
        "spice.serve.web.serve_branding",
        "spice.studies.complexity.collect_complexity_records",
        "spice.studies.complexity.complexity_hotspot_rows",
        "spice.studies.complexity.render_complexity_board",
        "spice.studies.complexity.render_complexity_hotspots",
        "spice.studies.complexity.scan_staged_complexity_violations",
        "spice.studies.envpolicy._literal_env_names_from_context_line",
        "spice.studies.envpolicy._standalone_waiver_line",
        "spice.studies.envpolicy._waived_line_numbers",
        "spice.studies.envpolicy.render_env_policy_board",
        "spice.studies.fileloc._breach_paths",
        "spice.studies.fileloc.is_generated_lockfile_path",
        "spice.studies.fileloc.render_loc_board",
        "spice.studies.fileloc.scan_loc_violations",
        "spice.studies.fileloc.scan_staged_loc_violations",
        "spice.studies.magicnums.detect_magic_regressions",
        "spice.studies.magicnums.render_magic_board",
        "spice.studies.magicnums.scan_text_magic_numbers",
        "spice.studies.taste.scan_taste",
        "spice.studies.taste.scan_taste_texts",
        "spice.tasks.config._hidden_stems",
        "spice.tasks.config._project_depth_bounds",
        "spice.tasks.wording.detect_task_creation_wording",
    }
)


def _packaged_leaves(
    value: object, path: tuple[str, ...] = ()
) -> list[tuple[tuple[str, ...], object]]:
    if isinstance(value, Mapping):
        leaves: list[tuple[tuple[str, ...], object]] = []
        for key, child in value.items():
            leaves.extend(_packaged_leaves(child, (*path, str(key))))
        return leaves
    return [(path, value)]


PACKAGED_LEAVES = _packaged_leaves(defaults.packaged_values())


def test_installed_and_layered_loaders_use_the_same_packaged_path(tmp_path):
    packaged = load_packaged_config()
    layered = layers.load_config(tmp_path).layer(SYSTEM_SOURCE)

    assert packaged.path == paths.runtime_spice_source() / "spice.toml"
    assert layered.path == packaged.path
    assert packaged.present is True
    assert layered.values == packaged.values


def test_default_export_inventory_resolves_every_python_export_and_toml_leaf():
    assert values.default_classifications() == (
        defaultinventory.EXPORTED_DEFAULT_CLASSIFICATION
    )
    assert set(defaultinventory.EXPORTED_DEFAULT_CLASSIFICATION.values()) == (
        defaultinventory.CLASSIFICATIONS
    )
    for (
        export,
        classification,
    ) in defaultinventory.EXPORTED_DEFAULT_CLASSIFICATION.items():
        module_name, attribute = export.rsplit(".", maxsplit=1)
        module = importlib.import_module(module_name)
        assert hasattr(module, attribute), export
        if classification == defaultinventory.TOML_STATIC:
            assert export in defaultinventory.TOML_STATIC_EXPORT_PATHS
    assert _static_default_mismatches() == []


def test_static_default_gate_fails_when_a_python_constant_drifts(monkeypatch):
    export = "spice.config.values.DEFAULT_SAY_BACKEND"
    monkeypatch.setattr(values, "DEFAULT_SAY_BACKEND", "drifted")

    assert _static_default_mismatches() == [
        (export, "drifted", defaults.value("say", "backend"))
    ]


def test_every_module_level_default_constant_is_classified():
    classified = set(defaultinventory.EXPORTED_DEFAULT_CLASSIFICATION)
    discovered = _module_level_default_exports()

    assert discovered <= classified, (
        "module-level default constants missing from default inventory: "
        + ", ".join(sorted(discovered - classified))
    )


def test_reverse_default_gate_discovers_every_authored_default_shape(tmp_path):
    source_root = tmp_path / "spice"
    source_root.mkdir()
    (source_root / "sample.py").write_text(
        "from spice import defaults\n"
        "DEFAULT_LEFT, DEFAULT_RIGHT = (1, 2)\n"
        "NAMED_WITHOUT_DEFAULT_PREFIX = defaults.string('sample', 'value')\n",
        encoding="utf-8",
    )

    assert _module_level_default_exports(source_root) == {
        "spice.sample.DEFAULT_LEFT",
        "spice.sample.DEFAULT_RIGHT",
        "spice.sample.NAMED_WITHOUT_DEFAULT_PREFIX",
    }


def _static_default_mismatches() -> list[tuple[str, object, object]]:
    mismatches = []
    for export, path_text in defaultinventory.TOML_STATIC_EXPORT_PATHS.items():
        module_name, attribute = export.rsplit(".", maxsplit=1)
        runtime = getattr(importlib.import_module(module_name), attribute)
        packaged = defaults.value(*path_text.split("."))
        normalizer = defaultinventory.TOML_STATIC_NORMALIZERS.get(export)
        normalized_runtime = normalizer(runtime) if normalizer else runtime
        normalized_packaged = normalizer(packaged) if normalizer else packaged
        if normalized_runtime != normalized_packaged:
            mismatches.append((export, runtime, packaged))
    return mismatches


def _module_level_default_exports(
    source_root: Path = PROJECT_ROOT / "spice",
) -> set[str]:
    exports = set()
    for path in source_root.rglob("*.py"):
        module_parts = path.with_suffix("").relative_to(source_root.parent).parts
        if module_parts[-1] == "__init__":
            module_parts = module_parts[:-1]
        module = ".".join(module_parts)
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in tree.body:
            targets: tuple[ast.expr, ...] = (
                tuple(node.targets) if isinstance(node, ast.Assign) else ()
            )
            if isinstance(node, ast.AnnAssign):
                targets = (node.target,)
            uses_packaged_default = _uses_packaged_default(node)
            for target in targets:
                for name in _target_names(target):
                    if not name.startswith("_") and (
                        name.startswith("DEFAULT_")
                        or (name.isupper() and uses_packaged_default)
                    ):
                        exports.add(f"{module}.{name}")
    return exports


def _uses_packaged_default(node: ast.stmt) -> bool:
    return any(
        isinstance(item, ast.Call)
        and isinstance(item.func, ast.Attribute)
        and isinstance(item.func.value, ast.Name)
        and item.func.value.id == "defaults"
        for item in ast.walk(node)
    )


def _target_names(target: ast.expr) -> tuple[str, ...]:
    if isinstance(target, ast.Name):
        return (target.id,)
    if isinstance(target, (ast.List, ast.Tuple)):
        return tuple(name for element in target.elts for name in _target_names(element))
    if isinstance(target, ast.Starred):
        return _target_names(target.value)
    return ()


def test_every_declared_static_family_exists_in_packaged_configuration():
    inventory = defaults.table("inventory")
    families = set(inventory["toml_static"])

    assert families == {
        "agent",
        "commands",
        "judge",
        "locks",
        "maxim",
        "maxims",
        "policy",
        "rtk",
        "say",
        "serve",
        "tasks",
        "wrappers",
    }
    assert families <= set(defaults.packaged_values())


def test_every_packaged_key_round_trips_through_the_configuration_schema():
    path = PROJECT_ROOT / "spice" / "spice.toml"
    packaged = tomllib.loads(path.read_text(encoding="utf-8"))

    schema.validate_config_keys(
        packaged,
        source_name=SYSTEM_SOURCE,
        source_path=path,
    )
    assert set(packaged) == set(schema.CONFIG_SCHEMA.children)


@pytest.mark.parametrize(
    ("path", "packaged"),
    PACKAGED_LEAVES,
    ids=[".".join(path) for path, _value in PACKAGED_LEAVES],
)
def test_every_packaged_leaf_accepts_repository_override_through_resolver(
    tmp_path, path, packaged
):
    """The complete packaged partition is layered, leaf by leaf.

    This writes a real repository override and observes the shared resolver,
    rather than inferring layering from where a Python assignment happens.
    """
    override = _distinct_toml_value(packaged)
    table = ".".join(_toml_key(part) for part in path[:-1])
    key = _toml_key(path[-1])
    (tmp_path / "spice.toml").write_text(
        f"[{table}]\n{key} = {_toml_value(override)}\n",
        encoding="utf-8",
    )

    assert values.layered_value(tmp_path, *path) == override


def test_packaged_reads_cannot_bypass_a_runtime_resolver():
    """Freeze defaults only at module scope; consumers must call a resolver."""
    observed: list[tuple[str, str, str]] = []
    for path in sorted((PROJECT_ROOT / "spice").rglob("*.py")):
        relative = path.relative_to(PROJECT_ROOT).as_posix()
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        parents = {
            child: parent
            for parent in ast.walk(tree)
            for child in ast.iter_child_nodes(parent)
        }
        defaults_aliases = {
            alias.asname or alias.name
            for node in ast.walk(tree)
            if isinstance(node, ast.ImportFrom) and node.module == "spice"
            for alias in node.names
            if alias.name == "defaults"
        }
        for node in ast.walk(tree):
            if not (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id in defaults_aliases
                and node.func.attr in _DEFAULT_ACCESSORS
            ):
                continue
            owner = _enclosing_function(node, parents)
            if owner is not None:
                observed.append((relative, owner.name, node.func.attr))

    assert observed == [_FUNCTION_SCOPE_PACKAGED_READ]
    serve_source = (PROJECT_ROOT / _FUNCTION_SCOPE_PACKAGED_READ[0]).read_text(
        encoding="utf-8"
    )
    assert "one deliberate packaged-only" in serve_source
    assert "consumer: there is no repository layer to resolve" in serve_source


def test_frozen_exports_cannot_gain_a_new_function_scope_consumer():
    """A frozen base may appear only inside its named resolver or pure seam."""
    exports: dict[str, set[str]] = {}
    for qualified in defaultinventory.TOML_STATIC_EXPORT_PATHS:
        module, name = qualified.rsplit(".", maxsplit=1)
        exports.setdefault(module, set()).add(name)

    observed: set[str] = set()
    for path in sorted((PROJECT_ROOT / "spice").rglob("*.py")):
        module = ".".join(path.relative_to(PROJECT_ROOT).with_suffix("").parts)
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        parents = {
            child: parent
            for parent in ast.walk(tree)
            for child in ast.iter_child_nodes(parent)
        }
        names = {name: (module, name) for name in exports.get(module, ())}
        modules: dict[str, str] = {}
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module in exports:
                for alias in node.names:
                    if alias.name in exports[node.module]:
                        names[alias.asname or alias.name] = (
                            node.module,
                            alias.name,
                        )
            elif isinstance(node, ast.ImportFrom) and node.module == "spice":
                for alias in node.names:
                    imported = f"spice.{alias.name}"
                    if imported in exports:
                        modules[alias.asname or alias.name] = imported
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name in exports:
                        modules[alias.asname or alias.name.rsplit(".", 1)[-1]] = (
                            alias.name
                        )

        for node in ast.walk(tree):
            frozen = (
                isinstance(node, ast.Name)
                and isinstance(node.ctx, ast.Load)
                and node.id in names
            )
            frozen_attribute = (
                isinstance(node, ast.Attribute)
                and isinstance(node.value, ast.Name)
                and node.value.id in modules
                and node.attr in exports[modules[node.value.id]]
            )
            if not frozen and not frozen_attribute:
                continue
            owner = _enclosing_function(node, parents)
            if owner is not None:
                observed.add(f"{module}.{owner.name}")

    unexpected = sorted(observed - _FROZEN_DEFAULT_RESOLVER_FUNCTIONS)
    assert unexpected == []


def _enclosing_function(
    node: ast.AST, parents: dict[ast.AST, ast.AST]
) -> ast.FunctionDef | ast.AsyncFunctionDef | None:
    current = parents.get(node)
    while current is not None:
        if isinstance(current, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return current
        current = parents.get(current)
    return None


def _distinct_toml_value(value: object) -> object:
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return [] if value else ["layered-override"]
    if isinstance(value, str):
        return value + "-layered-override"
    if isinstance(value, bool):
        return not value
    if isinstance(value, int):
        return value + 1
    if isinstance(value, float):
        return value + 0.25
    raise AssertionError(f"unsupported packaged leaf type: {type(value).__name__}")


def _toml_key(value: str) -> str:
    return (
        value
        if _TOML_BARE_KEY_RE.fullmatch(value)
        else json.dumps(value, ensure_ascii=False)
    )


def _toml_value(value: object) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return repr(value)
    return json.dumps(value, ensure_ascii=False)


def test_setuptools_package_data_and_built_wheel_ship_only_runtime_config(tmp_path):
    project = tomllib.loads(
        (PROJECT_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    )
    package_data = project["tool"]["setuptools"]["package-data"]["spice"]
    assert package_data == ["spice.toml"]

    source = tmp_path / "source"
    source.mkdir()
    shutil.copy2(PROJECT_ROOT / "pyproject.toml", source / "pyproject.toml")
    shutil.copy2(PROJECT_ROOT / "README.md", source / "README.md")
    shutil.copytree(
        PROJECT_ROOT / "spice",
        source / "spice",
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    wheel_dir = tmp_path / "wheel"
    subprocess.run(
        ["uv", "build", "--wheel", "--out-dir", str(wheel_dir)],
        cwd=source,
        check=True,
        capture_output=True,
        text=True,
    )
    wheel = next(wheel_dir.glob("*.whl"))
    with zipfile.ZipFile(wheel) as archive:
        root_package_data = {
            name
            for name in archive.namelist()
            if name.startswith("spice/")
            and "/" not in name.removeprefix("spice/")
            and not name.endswith(".py")
        }
    assert root_package_data == {"spice/spice.toml"}
