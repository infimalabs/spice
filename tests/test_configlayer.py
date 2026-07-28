"""Layered TOML configuration loading and merge law."""

from pathlib import Path
from types import MappingProxyType

import pytest

from spice.config import layers
from spice.errors import SpiceError


@pytest.mark.parametrize("scope", layers.CONFIG_SCOPE_NAMES)
def test_each_configuration_layer_can_win_independently(tmp_path, monkeypatch, scope):
    system_root = tmp_path / "runtime"
    system_root.mkdir()
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: system_root)
    _write(
        system_root / "spice.toml",
        f'[agent]\nmodel = "{"system-only" if scope == "system" else "system-base"}"\n',
    )
    expected_model = "system-only"
    if scope == layers.PYPROJECT_SOURCE:
        expected_model = "pyproject-only"
        _write(
            tmp_path / "pyproject.toml",
            f'[tool.spice.agent]\nmodel = "{expected_model}"\n',
        )
    elif scope == layers.REPOSITORY_SOURCE:
        expected_model = "repository-only"
        _write(tmp_path / "spice.toml", f'agent.model = "{expected_model}"\n')
    elif scope == layers.WORKTREE_SOURCE:
        expected_model = "worktree-only"
        _write(
            tmp_path / ".spice" / "config" / "spice.toml",
            f'agent.model = "{expected_model}"\n',
        )

    loaded = layers.load_config(tmp_path)

    assert loaded.effective["agent"]["model"] == expected_model
    assert loaded.source_for("agent.model") == loaded.layer(scope)


@pytest.mark.parametrize("scope", layers.CONFIG_SCOPE_NAMES)
def test_parse_error_names_the_exact_layer_and_path(tmp_path, monkeypatch, scope):
    system_root = tmp_path / "runtime"
    system_root.mkdir()
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: system_root)
    paths = {
        layers.SYSTEM_SOURCE: system_root / "spice.toml",
        layers.PYPROJECT_SOURCE: tmp_path / "pyproject.toml",
        layers.REPOSITORY_SOURCE: tmp_path / "spice.toml",
        layers.WORKTREE_SOURCE: tmp_path / ".spice" / "config" / "spice.toml",
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
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: packaged)
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

    loaded = layers.load_config(tmp_path)

    assert tuple(layer.name for layer in loaded.layers) == (
        layers.SYSTEM_SOURCE,
        layers.PYPROJECT_SOURCE,
        layers.REPOSITORY_SOURCE,
        layers.WORKTREE_SOURCE,
    )
    assert loaded.effective == {
        "agent": {
            "model": "packaged-model",
            "wrappers": (),
            "effort": "high",
        },
        "policy": {"limits": {"file_loc": 200, "file_bytes": 1000}},
    }
    assert loaded.layer(layers.WORKTREE_SOURCE).values == {}
    assert loaded.layer(layers.WORKTREE_SOURCE).present is False
    assert loaded.source_for("agent.model") == loaded.layer(layers.SYSTEM_SOURCE)
    assert loaded.source_for(("agent", "wrappers")) == loaded.layer(
        layers.REPOSITORY_SOURCE
    )
    assert loaded.source_for("policy.limits.file_loc") == loaded.layer(
        layers.PYPROJECT_SOURCE
    )
    assert isinstance(loaded.effective, MappingProxyType)
    assert isinstance(loaded.effective["agent"], MappingProxyType)


def test_unchanged_layers_parse_once_and_reload_after_source_revision(
    tmp_path, monkeypatch
):
    packaged = tmp_path / "runtime"
    packaged.mkdir()
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: packaged)
    system_path = packaged / "spice.toml"
    project_path = tmp_path / "pyproject.toml"
    _write(system_path, '[agent]\nmodel = "system"\n')
    _write(project_path, '[tool.spice.agent]\nmodel = "first"\n')
    parsed: list[Path] = []
    read_toml = layers._read_toml

    def track_parse(path, source_name):
        parsed.append(path)
        return read_toml(path, source_name)

    monkeypatch.setattr(layers, "_read_toml", track_parse)
    repeats = 40

    models = [
        layers.load_config(tmp_path).effective["agent"]["model"] for _ in range(repeats)
    ]
    _write(project_path, '[tool.spice.agent]\nmodel = "second"\n')
    models.append(layers.load_config(tmp_path).effective["agent"]["model"])
    source_paths = [
        system_path,
        project_path,
        tmp_path / "spice.toml",
        tmp_path / ".spice" / "config" / "spice.toml",
    ]

    assert models == ["first"] * repeats + ["second"]
    assert parsed == source_paths + source_paths


def test_loader_recursively_merges_tables_and_replaces_every_leaf_kind(
    tmp_path, monkeypatch
):
    packaged = tmp_path / "runtime"
    packaged.mkdir()
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: packaged)
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

    loaded = layers.load_config(tmp_path)

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
        layers.WORKTREE_SOURCE
    )
    assert loaded.source_for("policy.mode") == loaded.layer(layers.WORKTREE_SOURCE)


def test_contextualization_preserves_table_grammar_and_repository_source(tmp_path):
    _write(tmp_path / "spice.toml", 'serve = "invalid"\n')

    contextual = layers.contextualize_config_error(
        tmp_path,
        SpiceError("[tool.spice.serve] must be a table"),
        "serve",
    )

    assert str(contextual) == (
        f"serve (source=repository path={tmp_path / 'spice.toml'}): must be a table"
    )


def test_registry_resolver_refuses_an_undeclared_false_disable_contract():
    with pytest.raises(SpiceError, match="does not declare false-disable semantics"):
        layers.enabled_registry_entries({"sample": False}, "future", "registry")


def test_contextualization_identifies_leaf_key_and_worktree_source(tmp_path):
    worktree = tmp_path / ".spice" / "config" / "spice.toml"
    _write(worktree, '[serve]\nbrand = ""\n')

    contextual = layers.contextualize_config_error(
        tmp_path,
        SpiceError("[tool.spice.serve] brand must be a non-empty string"),
        "serve",
        "brand",
    )

    assert str(contextual) == (
        f"serve.brand (source=worktree path={worktree}): must be a non-empty string"
    )


def test_already_contextualized_error_keeps_detail_with_distinct_cause(
    tmp_path, monkeypatch
):
    system_root = tmp_path / "runtime"
    system_root.mkdir()
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: system_root)
    _write(system_root / "spice.toml", '[agent]\nmodel = "system"\n')
    original = SpiceError(
        f"serve (source=repository path={tmp_path / 'spice.toml'}): must be a table"
    )
    contextual = layers.contextualize_config_error(tmp_path, original, "serve")
    try:
        raise contextual from original
    except SpiceError as raised:
        outcome = (str(raised), raised.__cause__)

    assert outcome == (str(original), original)
    partial = SpiceError("source=repository without a path")
    contextualized = layers.contextualize_config_error(tmp_path, partial, "serve")
    assert str(contextualized) == "serve: source=repository without a path"


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def _load_outcome(repo_root: Path) -> dict[str, str]:
    try:
        layers.load_config(repo_root)
    except SpiceError as exc:
        return {"state": "rejected", "message": str(exc)}
    return {"state": "accepted", "message": "configuration loaded"}
