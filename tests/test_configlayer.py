"""Layered TOML configuration loading and merge law."""

from pathlib import Path
from types import MappingProxyType

import pytest

from spice import config, configlayer
from spice.errors import SpiceError


@pytest.mark.parametrize("scope", configlayer.CONFIG_SCOPE_NAMES)
def test_each_configuration_layer_can_win_independently(tmp_path, monkeypatch, scope):
    system_root = tmp_path / "runtime"
    system_root.mkdir()
    monkeypatch.setattr(configlayer.paths, "runtime_spice_source", lambda: system_root)
    _write(
        system_root / "spice.toml",
        f'[agent]\nmodel = "{"system-only" if scope == "system" else "system-base"}"\n',
    )
    expected_model = "system-only"
    if scope == configlayer.PYPROJECT_SOURCE:
        expected_model = "pyproject-only"
        _write(
            tmp_path / "pyproject.toml",
            f'[tool.spice.agent]\nmodel = "{expected_model}"\n',
        )
    elif scope == configlayer.REPOSITORY_SOURCE:
        expected_model = "repository-only"
        _write(tmp_path / "spice.toml", f'agent.model = "{expected_model}"\n')
    elif scope == configlayer.WORKTREE_SOURCE:
        expected_model = "worktree-only"
        _write(
            tmp_path / ".spice" / "config" / "spice.toml",
            f'agent.model = "{expected_model}"\n',
        )

    loaded = configlayer.load_config(tmp_path)

    assert loaded.effective["agent"]["model"] == expected_model
    assert loaded.source_for("agent.model") == loaded.layer(scope)


@pytest.mark.parametrize("scope", configlayer.CONFIG_SCOPE_NAMES)
def test_parse_error_names_the_exact_layer_and_path(tmp_path, monkeypatch, scope):
    system_root = tmp_path / "runtime"
    system_root.mkdir()
    monkeypatch.setattr(configlayer.paths, "runtime_spice_source", lambda: system_root)
    paths = {
        configlayer.SYSTEM_SOURCE: system_root / "spice.toml",
        configlayer.PYPROJECT_SOURCE: tmp_path / "pyproject.toml",
        configlayer.REPOSITORY_SOURCE: tmp_path / "spice.toml",
        configlayer.WORKTREE_SOURCE: tmp_path / ".spice" / "config" / "spice.toml",
    }
    _write(system_root / "spice.toml", '[agent]\nmodel = "system"\n')
    _write(paths[scope], "broken = [\n")

    outcome = _load_outcome(tmp_path)

    assert outcome["state"] == "rejected"
    assert outcome["message"].startswith(
        f"invalid TOML for configuration source={scope} path={paths[scope]}:"
    )


def test_loader_exposes_four_immutable_layers_and_leaf_provenance(
    tmp_path, monkeypatch
):
    packaged = tmp_path / "installed-spice"
    packaged.mkdir()
    monkeypatch.setattr(configlayer.paths, "runtime_spice_source", lambda: packaged)
    _write(
        packaged / "spice.toml",
        """
        [agent]
        model = "packaged-model"
        wrappers = ["common"]

        [policy.limits]
        file_loc = 100
        file_bytes = 1000
        """,
    )
    _write(
        tmp_path / "pyproject.toml",
        """
        [project]
        name = "fixture"

        [tool.spice.agent]
        effort = "high"

        [tool.spice.policy.limits]
        file_loc = 200
        """,
    )
    _write(tmp_path / "spice.toml", "agent.wrappers = []\n")

    loaded = config.load_config(tmp_path)

    assert tuple(layer.name for layer in loaded.layers) == (
        configlayer.SYSTEM_SOURCE,
        configlayer.PYPROJECT_SOURCE,
        configlayer.REPOSITORY_SOURCE,
        configlayer.WORKTREE_SOURCE,
    )
    assert loaded.effective == {
        "agent": {
            "model": "packaged-model",
            "wrappers": (),
            "effort": "high",
        },
        "policy": {"limits": {"file_loc": 200, "file_bytes": 1000}},
    }
    assert loaded.layer(configlayer.WORKTREE_SOURCE).values == {}
    assert loaded.layer(configlayer.WORKTREE_SOURCE).present is False
    assert loaded.source_for("agent.model") == loaded.layer(configlayer.SYSTEM_SOURCE)
    assert loaded.source_for(("agent", "wrappers")) == loaded.layer(
        configlayer.REPOSITORY_SOURCE
    )
    assert loaded.source_for("policy.limits.file_loc") == loaded.layer(
        configlayer.PYPROJECT_SOURCE
    )
    assert isinstance(loaded.effective, MappingProxyType)
    assert isinstance(loaded.effective["agent"], MappingProxyType)


def test_loader_recursively_merges_tables_and_replaces_every_leaf_kind(
    tmp_path, monkeypatch
):
    packaged = tmp_path / "runtime"
    packaged.mkdir()
    monkeypatch.setattr(configlayer.paths, "runtime_spice_source", lambda: packaged)
    _write(
        packaged / "spice.toml",
        """
        [wrappers.common.rtk]
        argv = ["rtk"]
        match = [{ head = "grep", argv = ["rg"] }]

        [policy]
        mode = "strict"

        [[policy.internal_couplings]]
        path = "packaged.py"
        test = "test_packaged"
        target = "_packaged"
        """,
    )
    _write(
        tmp_path / "pyproject.toml",
        """
        [tool.spice.wrappers.common.rtk]
        executable = "configured-rtk"

        [[tool.spice.policy.internal_couplings]]
        path = "project.py"
        test = "test_project"
        target = "_project"
        """,
    )
    _write(
        tmp_path / "spice.toml",
        """
        [wrappers.common.rtk]
        argv = ["repo-rtk"]

        [policy]
        mode = { level = 2 }
        """,
    )
    _write(
        tmp_path / ".spice" / "config" / "spice.toml",
        """
        [wrappers.common.rtk]
        match = []

        [policy]
        mode = ["worktree"]
        """,
    )

    loaded = config.load_config(tmp_path)

    assert loaded.effective["wrappers"]["common"]["rtk"] == {"match": ()}
    assert loaded.effective["policy"] == {
        "mode": ("worktree",),
        "internal_couplings": (
            {
                "path": "project.py",
                "test": "test_project",
                "target": "_project",
            },
        ),
    }
    assert loaded.source_for("wrappers.common.rtk.match") == loaded.layer(
        configlayer.WORKTREE_SOURCE
    )
    assert loaded.source_for("policy.mode") == loaded.layer(configlayer.WORKTREE_SOURCE)


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _load_outcome(repo_root: Path) -> dict[str, str]:
    try:
        configlayer.load_config(repo_root)
    except SpiceError as exc:
        return {"state": "rejected", "message": str(exc)}
    return {"state": "accepted", "message": "configuration loaded"}
