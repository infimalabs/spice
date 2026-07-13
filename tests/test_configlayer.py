"""Layered TOML configuration loading and merge law."""

from pathlib import Path
from types import MappingProxyType

from spice import config, configlayer


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
        configlayer.PACKAGED_SOURCE,
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
    assert loaded.source_for("agent.model") == loaded.layer(configlayer.PACKAGED_SOURCE)
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
