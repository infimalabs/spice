"""Packaging-toolchain contracts for the release rehearsal.

The artifact chain runs on the same locked surface as every other gate and on
the exact versions release-proof/toolchain.json declares. Both facts are gates
here rather than conventions, because a host run and a container run only
produce interchangeable evidence while they agree.
"""

from __future__ import annotations

import json
import subprocess
import tomllib
from pathlib import Path

import pytest

from tests.test_releaseproofhelpers import (
    PROJECT_ROOT,
    REHEARSAL,
    TOOLCHAIN_DECLARATION,
    _write_test_wheel,
)

TOOLCHAIN_RELATIVE_PATH = "release-proof/toolchain.json"
PYPROJECT = PROJECT_ROOT / "pyproject.toml"
# The packaging pins fixtures declare, held apart from the real declaration so
# a fixture keeps stating what it expects even as the real pins move.
DECLARED_PACKAGING_PINS = {
    "build": "1.3.0",
    "setuptools": "80.9.0",
    "twine": "6.1.0",
    "wheel": "0.45.1",
}


def test_locked_packaging_pins_match_the_container_toolchain_declaration():
    """One declaration governs both runs, so host evidence is container evidence.

    The container installs its packaging tools from Containerfile ARGs while a
    host run resolves them from uv.lock. They are only interchangeable while
    both agree with release-proof/toolchain.json, so that agreement is a gate
    rather than a convention.
    """
    declared = json.loads(TOOLCHAIN_DECLARATION.read_text(encoding="utf-8"))["pinned"]
    project = tomllib.loads(PYPROJECT.read_text(encoding="utf-8"))
    group = project["dependency-groups"]["dev"]
    distributions = set(REHEARSAL.PACKAGING_TOOLCHAIN.values())

    assert sorted(
        requirement
        for requirement in group
        if requirement.split("==")[0] in distributions
    ) == sorted(f"{name}=={declared[name]}" for name in distributions)


def _toolchain_source(tmp_path: Path) -> Path:
    root = tmp_path / "declared"
    (root / "release-proof").mkdir(parents=True)
    (root / TOOLCHAIN_RELATIVE_PATH).write_text(
        json.dumps({"schema_version": 1, "pinned": dict(DECLARED_PACKAGING_PINS)}),
        encoding="utf-8",
    )
    return root


def _resolved_packaging(missing: list[str] | None = None, **overrides: str):
    report = {
        "missing": missing or [],
        "resolved": dict(DECLARED_PACKAGING_PINS) | overrides,
    }

    def probe(command, **_kwargs):
        return subprocess.CompletedProcess(
            command, 0, stdout=json.dumps(report), stderr=""
        )

    return probe


def test_packaging_preflight_names_the_tool_whose_version_drifted(
    tmp_path, monkeypatch
):
    """Drift is reported by name, because the pins are the proof's whole point.

    A host resolving a different twine than the container declares would still
    build artifacts, just not artifacts the container's evidence describes.
    """
    root = _toolchain_source(tmp_path)
    monkeypatch.setattr(REHEARSAL, "_run", _resolved_packaging(twine="5.0.0"))

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert (
        "twine" in message,
        TOOLCHAIN_RELATIVE_PATH in message,
        '"twine": "5.0.0"' in message,
        '"twine": "6.1.0"' in message,
    ) == (True, True, True, True)


def test_packaging_preflight_names_uv_before_spending_a_subprocess(
    tmp_path, monkeypatch
):
    """Without uv there is no locked surface at all, so say so and stop.

    This is the host-checkout failure the container never sees, and it has to
    read as an install instruction rather than as a missing-module traceback
    from somewhere deep in the artifact phase.
    """
    root = _toolchain_source(tmp_path)

    def refuse_to_run(command, **_kwargs):
        raise AssertionError(f"spent a subprocess without uv present: {command}")

    monkeypatch.setattr(REHEARSAL.shutil, "which", lambda _name: None)
    monkeypatch.setattr(REHEARSAL, "_run", refuse_to_run)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert ("uv" in message, "PATH" in message, "locked toolchain" in message) == (
        True,
        True,
        True,
    )


def test_packaging_steps_run_from_the_locked_project_toolchain(tmp_path, monkeypatch):
    project = tmp_path / "checkout"
    source = tmp_path / "exported"
    artifacts = tmp_path / "artifacts"
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    calls: list[tuple[str, ...]] = []

    def build_tools(command, *, cwd, **_kwargs):
        argv = tuple(command)
        calls.append(argv)
        if "--sdist" in argv:
            (artifacts / "spice_harness-1.2.3.tar.gz").write_bytes(b"sdist\n")
        if "--wheel" in argv:
            _write_test_wheel(
                cwd / "spice_harness-1.2.3-py3-none-any.whl",
                {"spice/__init__.py": b"namespace package\n"},
                year=2024,
            )
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(REHEARSAL, "_run", build_tools)
    monkeypatch.setattr(
        REHEARSAL,
        "_extract_sdist",
        lambda _sdist, destination, _version: destination,
    )

    sdist, _wheel = REHEARSAL._build_canonical_artifacts(
        source, artifacts, "1.2.3", None, project_root=project
    )
    rebuilt = REHEARSAL._rebuild_wheel_from_sdist(
        sdist, "1.2.3", scratch, None, project_root=project
    )

    locked = REHEARSAL.packaging_python_command(project)
    assert (
        [command[: len(locked)] for command in calls],
        rebuilt.name,
    ) == ([locked] * 4, "spice_harness-1.2.3-py3-none-any.whl")
    assert locked == (
        "uv",
        "run",
        "--locked",
        "--project",
        str(project),
        "python",
        "-P",
    )


def test_packaging_preflight_names_every_missing_module(tmp_path, monkeypatch):
    root = tmp_path / "checkout"

    missing_modules = _resolved_packaging(missing=["build.__main__", "twine"])

    monkeypatch.setattr(REHEARSAL, "_run", missing_modules)

    with pytest.raises(REHEARSAL.RehearsalError) as failure:
        REHEARSAL.verify_packaging_toolchain(root)

    message = str(failure.value)
    assert (
        "build.__main__, twine failed to import" in message,
        "uv lock" in message,
    ) == (True, True)


def test_packaging_preflight_records_the_toolchain_it_proved(tmp_path, monkeypatch):
    root = _toolchain_source(tmp_path)
    probes: list[tuple[str, ...]] = []
    resolve = _resolved_packaging()

    def present_modules(command, **kwargs):
        probes.append(tuple(command))
        return resolve(command, **kwargs)

    monkeypatch.setattr(REHEARSAL, "_run", present_modules)

    proven = REHEARSAL.verify_packaging_toolchain(root)

    prefix = REHEARSAL.packaging_python_command(root)
    assert (proven, probes[0][: len(prefix)], probes[0][len(prefix)]) == (
        DECLARED_PACKAGING_PINS,
        prefix,
        "-c",
    )
