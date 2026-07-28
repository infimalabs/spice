"""Layered RTK executable identity configuration."""

from __future__ import annotations

import json
import os.path
import shutil
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Callable

import pytest

from spice.config import edit, layers, values
from spice.configcli import handle_config
from spice.errors import SpiceError

pytestmark = pytest.mark.usefixtures("git_worktree_tmp_path")


def test_packaged_rtk_default_is_the_bare_executable(tmp_path: Path) -> None:
    assert values.DEFAULT_RTK_EXECUTABLE == "rtk"
    assert values.configured_rtk_executable(tmp_path) == "rtk"


@pytest.mark.parametrize("scope", layers.CONFIG_SCOPE_NAMES)
def test_each_rtk_configuration_layer_can_win_with_provenance(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, scope: str
) -> None:
    paths = _redirect_system_config(tmp_path, monkeypatch, "system-rtk")
    expected = "system-rtk"
    if scope == layers.PYPROJECT_SOURCE:
        expected = "pyproject-rtk"
        _write(
            paths[scope],
            f'[tool.spice.rtk]\nexecutable = "{expected}"\n',
        )
    elif scope == layers.REPOSITORY_SOURCE:
        expected = "repository-rtk"
        _write(paths[scope], f'[rtk]\nexecutable = "{expected}"\n')
    elif scope == layers.WORKTREE_SOURCE:
        expected = "worktree-rtk"
        _write(paths[scope], f'[rtk]\nexecutable = "{expected}"\n')

    overview = values.config_overview(tmp_path)

    assert values.configured_rtk_executable(tmp_path) == expected
    assert overview["effective"]["rtk"]["executable"] == expected
    assert overview["provenance"]["rtk.executable"] == {
        "scope": scope,
        "path": str(paths[scope]),
    }


def test_all_rtk_layers_resolve_in_declared_precedence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    paths = _redirect_system_config(tmp_path, monkeypatch, "system-rtk")
    _write(
        paths[layers.PYPROJECT_SOURCE],
        '[tool.spice.rtk]\nexecutable = "pyproject-rtk"\n',
    )
    _write(
        paths[layers.REPOSITORY_SOURCE],
        '[rtk]\nexecutable = "repository-rtk"\n',
    )
    _write(
        paths[layers.WORKTREE_SOURCE],
        '[rtk]\nexecutable = "worktree-rtk"\n',
    )

    overview = values.config_overview(tmp_path)

    assert values.configured_rtk_executable(tmp_path) == "worktree-rtk"
    assert overview["provenance"]["rtk.executable"] == {
        "scope": "worktree",
        "path": str(paths[layers.WORKTREE_SOURCE]),
    }


def test_config_show_reports_effective_rtk_identity_and_winning_layer(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    worktree = edit.worktree_config_path(tmp_path)
    _write(worktree, '[rtk]\nexecutable = "visible-rtk"\n')
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(SimpleNamespace(config_action="show"))
    overview = json.loads(capsys.readouterr().out)

    assert result == 0
    assert overview["effective"]["rtk"]["executable"] == "visible-rtk"
    assert overview["provenance"]["rtk.executable"] == {
        "scope": "worktree",
        "path": str(worktree),
    }


@pytest.mark.parametrize("executable", ["custom-rtk", "/opt/Spice Tools/rtk"])
def test_rtk_executable_identity_is_retained_exactly(
    tmp_path: Path, executable: str
) -> None:
    _write(
        tmp_path / "spice.toml",
        f"[rtk]\nexecutable = {json.dumps(executable)}\n",
    )

    assert values.configured_rtk_executable(tmp_path) == executable


@pytest.mark.parametrize(
    "configured",
    [
        'executable = ""',
        'executable = "rtk rewrite"',
        'executable = "tools/rtk"',
        'executable = ["rtk", "rewrite"]',
        "executable = 7",
    ],
)
def test_malformed_rtk_identity_reports_winning_source(
    tmp_path: Path, configured: str
) -> None:
    source = tmp_path / "spice.toml"
    _write(source, f"[rtk]\n{configured}\n")

    outcome = _resolution_outcome(lambda: values.configured_rtk_executable(tmp_path))

    assert outcome == {
        "state": "rejected",
        "message": (
            f"rtk.executable (source=repository path={source}): must be one "
            "non-empty executable basename or absolute path"
        ),
    }


def test_rtk_resolution_trusts_identity_without_availability_probe(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    configured = "/missing/by-contract/rtk"
    real_subprocess_run = subprocess.run
    real_path_exists = Path.exists

    def reject_configured_probe(candidate: object, *_args: object, **_kwargs: object):
        if str(candidate) == configured:
            raise AssertionError(
                "RTK resolution must not probe executable availability"
            )
        return None

    def allow_unrelated_command(*args: object, **kwargs: object):
        argv = args[0] if args else kwargs.get("args", ())
        if configured in tuple(str(part) for part in argv):
            raise AssertionError(
                "RTK resolution must not probe executable availability"
            )
        return real_subprocess_run(*args, **kwargs)

    def allow_unrelated_path(path: Path) -> bool:
        if str(path) == configured:
            raise AssertionError(
                "RTK resolution must not probe executable availability"
            )
        return real_path_exists(path)

    monkeypatch.setattr(shutil, "which", reject_configured_probe)
    monkeypatch.setattr(subprocess, "run", allow_unrelated_command)
    monkeypatch.setattr(os.path, "exists", reject_configured_probe)
    monkeypatch.setattr(Path, "exists", allow_unrelated_path)
    _write(
        tmp_path / "spice.toml",
        f'[rtk]\nexecutable = "{configured}"\n',
    )

    assert values.configured_rtk_executable(tmp_path) == configured


def _redirect_system_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, executable: str
) -> dict[str, Path]:
    system_root = tmp_path / "installed-spice"
    system_path = system_root / "spice.toml"
    _write(system_path, f'[rtk]\nexecutable = "{executable}"\n')
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: system_root)
    return {
        layers.SYSTEM_SOURCE: system_path,
        layers.PYPROJECT_SOURCE: tmp_path / "pyproject.toml",
        layers.REPOSITORY_SOURCE: tmp_path / "spice.toml",
        layers.WORKTREE_SOURCE: edit.worktree_config_path(tmp_path),
    }


def _resolution_outcome(operation: Callable[[], str]) -> dict[str, str]:
    try:
        value = operation()
    except SpiceError as exc:
        return {"state": "rejected", "message": str(exc)}
    return {"state": "accepted", "message": value}


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
