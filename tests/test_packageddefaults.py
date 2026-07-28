"""Packaged defaults, classification inventory, and wheel contracts."""

from __future__ import annotations

import ast
import importlib
import shutil
import subprocess
import tomllib
import zipfile
from pathlib import Path

from spice import defaultinventory, defaults, paths
from spice.config import layers, schema, values
from spice.config.layers import SYSTEM_SOURCE, load_packaged_config

PROJECT_ROOT = Path(__file__).resolve().parents[1]


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
