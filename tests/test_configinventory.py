"""The current layered loader and TOML editor are the only config seams."""

from __future__ import annotations

import ast
import tomllib
from pathlib import Path

from spice.config import layers

PROJECT_ROOT = Path(__file__).resolve().parents[1]
FORBIDDEN_CONFIGURATION_SYMBOLS = (
    "LEGACY_CONFIG_STATE_RELATIVE_PATH",
    "LEGACY_CONFIG_SCHEMA",
    "_ensure_worktree_config_migrated",
    "project_agent_config",
    "worktree_agent_config",
    "update_project_agent_config",
    "clear_project_agent_config",
    "set_worktree_section",
    "clear_worktree_section",
    "PACKAGED_SOURCE",
    "PYPROJECT_SOURCE",
    "_pyproject_spice_table",
)


def test_configuration_source_inventory_has_only_current_seams() -> None:
    python_paths = sorted((PROJECT_ROOT / "spice").rglob("*.py"))
    toml_importers: list[str] = []
    definitions: dict[str, set[str]] = {}
    source_text: dict[str, str] = {}
    for path in python_paths:
        relative = path.relative_to(PROJECT_ROOT).as_posix()
        text = path.read_text(encoding="utf-8")
        source_text[relative] = text
        tree = ast.parse(text, filename=relative)
        if any(
            isinstance(node, ast.Import)
            and any(alias.name == "tomllib" for alias in node.names)
            for node in tree.body
        ):
            toml_importers.append(relative)
        definitions[relative] = {
            node.name
            for node in tree.body
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        }

    compatibility_symbols = tuple(
        sorted(
            symbol
            for symbol in FORBIDDEN_CONFIGURATION_SYMBOLS
            if any(symbol in text for text in source_text.values())
        )
    )
    inventory = {
        "toml_importers": tuple(toml_importers),
        "retired_source_guards": tuple(
            sorted(
                definitions["spice/config/layers.py"]
                & {
                    "_reject_retired_pyproject_config",
                    "_retired_pyproject_has_spice_table",
                }
            )
        ),
        "loader_functions": tuple(
            sorted(
                definitions["spice/config/layers.py"]
                & {"load_config", "effective_mapping", "effective_table", "layer_table"}
            )
        ),
        "editor_functions": tuple(
            sorted(
                definitions["spice/config/edit.py"]
                & {"set_scope_section", "clear_scope_section"}
            )
        ),
        "scope_vocabulary": layers.CONFIG_SCOPE_NAMES,
        "compatibility_symbols": compatibility_symbols,
        "config_state_files": tuple(
            marker
            for marker in ("state.json", "spice.toml")
            if marker in source_text["spice/config/edit.py"]
        ),
    }

    assert inventory == {
        "toml_importers": (
            "spice/config/edit.py",
            "spice/config/layers.py",
            "spice/config/pyproject.py",
            "spice/configcli.py",
        ),
        "retired_source_guards": (
            "_reject_retired_pyproject_config",
            "_retired_pyproject_has_spice_table",
        ),
        "loader_functions": (
            "effective_mapping",
            "effective_table",
            "layer_table",
            "load_config",
        ),
        "editor_functions": ("clear_scope_section", "set_scope_section"),
        "scope_vocabulary": ("system", "repository", "worktree"),
        "compatibility_symbols": (),
        "config_state_files": ("spice.toml",),
    }


def test_this_repository_exercises_the_unwrapped_tracked_shape() -> None:
    pyproject_text = (PROJECT_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    with (PROJECT_ROOT / "spice.toml").open("rb") as handle:
        repository_config = tomllib.load(handle)

    assert "[tool.spice" not in pyproject_text
    assert repository_config["agent"]["wrappers"] == ["common", "spice-dev"]
    assert repository_config["commands"]["release"] == [
        "uv",
        "run",
        "python",
        "-m",
        "spice.release",
    ]
