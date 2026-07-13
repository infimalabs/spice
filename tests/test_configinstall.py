"""Installed-wheel and editable layered-configuration contracts."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
INSTALL_TIMEOUT_SECONDS = 120
PROBE_TIMEOUT_SECONDS = 30
PROBE = """
import json
import sys
from pathlib import Path

from spice import configlayer

root = Path(sys.argv[1])
loaded = configlayer.load_config(root)
print(json.dumps({
    "module_path": str(Path(configlayer.__file__).resolve()),
    "system_path": str(loaded.layer(configlayer.SYSTEM_SOURCE).path.resolve()),
    "system_personality": loaded.layer(configlayer.SYSTEM_SOURCE).values["agent"]["personality"],
    "model": loaded.effective["agent"]["model"],
    "effort": loaded.effective["agent"]["effort"],
    "brand": loaded.effective["serve"]["brand"],
    "model_source": loaded.source_for("agent.model").name,
    "effort_source": loaded.source_for("agent.effort").name,
    "brand_source": loaded.source_for("serve.brand").name,
}, sort_keys=True))
"""


def test_wheel_and_editable_installs_load_their_own_system_layer(
    tmp_path: Path,
) -> None:
    wheelhouse = tmp_path / "wheelhouse"
    wheelhouse.mkdir()
    _run(
        ["uv", "build", "--wheel", "--out-dir", str(wheelhouse)],
        cwd=PROJECT_ROOT,
        timeout=INSTALL_TIMEOUT_SECONDS,
    )
    wheel = next(wheelhouse.glob("*.whl"))
    fixture = tmp_path / "fixture"
    _write_fixture_layers(fixture)
    isolated_cwd = tmp_path / "probe"
    isolated_cwd.mkdir()

    observed: dict[str, dict[str, str]] = {}
    installations = {
        "wheel": [str(wheel)],
        "editable": ["--editable", str(PROJECT_ROOT)],
    }
    for kind, requirement in installations.items():
        environment = tmp_path / f"{kind}-venv"
        _run(
            ["uv", "venv", "--python", sys.executable, str(environment)],
            cwd=tmp_path,
            timeout=INSTALL_TIMEOUT_SECONDS,
        )
        python = _venv_python(environment)
        _run(
            [
                "uv",
                "pip",
                "install",
                "--no-deps",
                "--python",
                str(python),
                *requirement,
            ],
            cwd=tmp_path,
            timeout=INSTALL_TIMEOUT_SECONDS,
        )
        completed = _run(
            [str(python), "-c", PROBE, str(fixture)],
            cwd=isolated_cwd,
            timeout=PROBE_TIMEOUT_SECONDS,
            env={**os.environ, "PYTHONPATH": ""},  # env-policy: allow
        )
        observed[kind] = json.loads(completed.stdout)

    expected_effective = {
        "system_personality": "pragmatic",
        "model": "repository-model",
        "effort": "high",
        "brand": "Worktree Brand",
        "model_source": "repository",
        "effort_source": "worktree",
        "brand_source": "worktree",
    }
    for values in observed.values():
        assert {key: values[key] for key in expected_effective} == expected_effective
        assert Path(values["system_path"]).parent == Path(values["module_path"]).parent

    wheel_origin = (
        "checkout"
        if Path(observed["wheel"]["module_path"]).is_relative_to(PROJECT_ROOT)
        else "isolated-environment"
    )
    editable_origin = (
        "checkout"
        if Path(observed["editable"]["module_path"]).is_relative_to(PROJECT_ROOT)
        else "isolated-environment"
    )
    assert wheel_origin == "isolated-environment"
    assert editable_origin == "checkout"


def _write_fixture_layers(root: Path) -> None:
    root.mkdir()
    (root / "spice.toml").write_text(
        '[agent]\nmodel = "repository-model"\n\n[serve]\nbrand = "Repository Brand"\n',
        encoding="utf-8",
    )
    worktree = root / ".spice" / "config" / "spice.toml"
    worktree.parent.mkdir(parents=True)
    worktree.write_text(
        '[agent]\neffort = "high"\n\n[serve]\nbrand = "Worktree Brand"\n',
        encoding="utf-8",
    )


def _venv_python(environment: Path) -> Path:
    directory = "Scripts" if sys.platform == "win32" else "bin"
    executable = "python.exe" if sys.platform == "win32" else "python"
    return environment / directory / executable


def _run(
    argv: list[str],
    *,
    cwd: Path,
    timeout: int,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
    )
