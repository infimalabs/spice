#!/usr/bin/env python3
"""Resolve and verify the release-proof container's complete toolchain."""

from __future__ import annotations

import argparse
import importlib.metadata
import json
import platform
import subprocess
from pathlib import Path


def _command(*arguments: str, root: Path) -> str:
    completed = subprocess.run(
        list(arguments),
        check=True,
        capture_output=True,
        cwd=root,
        text=True,
    )
    return completed.stdout.strip()


def _node_expression(expression: str, *, root: Path) -> str:
    return _command("node", "--print", expression, root=root)


def _browser_version(root: Path) -> str:
    executable = _node_expression(
        "require('playwright').chromium.executablePath()", root=root
    )
    return _command(executable, "--version", root=root)


def resolve(root: Path) -> dict[str, object]:
    declaration = json.loads(
        (root / "release-proof/toolchain.json").read_text(encoding="utf-8")
    )
    pinned = declaration["pinned"]
    resolved_pins = {
        "build": importlib.metadata.version("build"),
        "pip": importlib.metadata.version("pip"),
        "playwright": _node_expression(
            "require('playwright/package.json').version", root=root
        ),
        "setuptools": importlib.metadata.version("setuptools"),
        "twine": importlib.metadata.version("twine"),
        "uv": _command("uv", "--version", root=root).split()[1],
        "wheel": importlib.metadata.version("wheel"),
    }
    if resolved_pins != pinned:
        raise SystemExit(
            "resolved release-proof pins differ from declaration: "
            + json.dumps(
                {"declared": pinned, "resolved": resolved_pins}, sort_keys=True
            )
        )

    return {
        "schema_version": 1,
        "base": declaration["base"],
        "resolved": {
            "browser": _browser_version(root),
            "git": _command("git", "--version", root=root),
            "node": _command("node", "--version", root=root),
            "packaging": {
                name: resolved_pins[name]
                for name in ("build", "pip", "setuptools", "twine", "wheel")
            },
            "playwright": resolved_pins["playwright"],
            "python": platform.python_version(),
            "taskwarrior": _command("task", "--version", root=root),
            "uv": resolved_pins["uv"],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    payload = resolve(root)
    arguments.output.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(payload, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
