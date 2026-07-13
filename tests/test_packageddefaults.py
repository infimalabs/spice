"""Packaged defaults, classification inventory, and wheel contracts."""

from __future__ import annotations

import importlib
import subprocess
import tomllib
import zipfile
from pathlib import Path

from spice import config, defaultinventory, defaults, paths
from spice.configlayer import SYSTEM_SOURCE, load_packaged_config

PROJECT_ROOT = Path(__file__).resolve().parents[1]


def test_installed_and_layered_loaders_use_the_same_packaged_path(tmp_path):
    packaged = load_packaged_config()
    layered = config.load_config(tmp_path).layer(SYSTEM_SOURCE)

    assert packaged.path == paths.runtime_spice_source() / "spice.toml"
    assert layered.path == packaged.path
    assert packaged.present is True
    assert layered.values == packaged.values


def test_default_export_inventory_resolves_every_python_export_and_toml_leaf():
    assert config.default_classifications() == (
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
            path = defaultinventory.TOML_STATIC_EXPORT_PATHS[export].split(".")
            assert defaults.value(*path) is not None


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
        "say",
        "serve",
        "tasks",
        "wrappers",
    }
    assert families <= set(defaults.packaged_values())


def test_setuptools_package_data_and_built_wheel_ship_spice_toml(tmp_path):
    project = tomllib.loads(
        (PROJECT_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    )
    package_data = project["tool"]["setuptools"]["package-data"]["spice"]
    assert "spice.toml" in package_data

    subprocess.run(
        ["uv", "build", "--wheel", "--out-dir", str(tmp_path)],
        cwd=PROJECT_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    wheel = next(tmp_path.glob("*.whl"))
    with zipfile.ZipFile(wheel) as archive:
        assert "spice/spice.toml" in archive.namelist()
