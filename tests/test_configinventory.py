"""The current layered loader and TOML editor are the only config seams."""

from __future__ import annotations

import ast
from pathlib import Path

from spice import configlayer

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
)


def test_configuration_source_inventory_has_only_current_seams() -> None:
    python_paths = sorted((PROJECT_ROOT / "spice").rglob("*.py"))
    toml_importers: list[str] = []
    spice_table_readers: list[str] = []
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
        if relative in toml_importers and any(
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "get"
            and node.args
            and isinstance(node.args[0], ast.Constant)
            and node.args[0].value == "spice"
            for node in ast.walk(tree)
        ):
            spice_table_readers.append(relative)
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
        "spice_table_readers": tuple(spice_table_readers),
        "loader_functions": tuple(
            sorted(
                definitions["spice/configlayer.py"]
                & {"load_config", "effective_mapping", "effective_table", "layer_table"}
            )
        ),
        "editor_functions": tuple(
            sorted(
                definitions["spice/config.py"]
                & {"set_scope_section", "clear_scope_section"}
            )
        ),
        "scope_vocabulary": configlayer.CONFIG_SCOPE_NAMES,
        "compatibility_symbols": compatibility_symbols,
        "config_state_files": tuple(
            marker
            for marker in ("state.json", "spice.toml")
            if marker in source_text["spice/config.py"]
        ),
    }

    assert inventory == {
        "toml_importers": (
            "spice/config.py",
            "spice/configlayer.py",
            "spice/repocfg.py",
            "spice/serve/web.py",
        ),
        "spice_table_readers": ("spice/configlayer.py",),
        "loader_functions": (
            "effective_mapping",
            "effective_table",
            "layer_table",
            "load_config",
        ),
        "editor_functions": ("clear_scope_section", "set_scope_section"),
        "scope_vocabulary": ("system", "pyproject", "repository", "worktree"),
        "compatibility_symbols": (),
        "config_state_files": ("spice.toml",),
    }
