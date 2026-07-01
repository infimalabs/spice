"""Task backend file helpers."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.serve.team.store import team_database_path
from spice.serve.team.schema import TEAM_DATABASE_FILENAME
from spice.tasks import config, render


def test_atomic_write_text_keeps_matching_file(tmp_path):
    path = tmp_path / "taskrc"
    config._atomic_write_text(path, "same\n")
    before = path.stat()

    config._atomic_write_text(path, "same\n")
    after = path.stat()

    assert (after.st_ino, after.st_mtime_ns, after.st_size) == (
        before.st_ino,
        before.st_mtime_ns,
        before.st_size,
    )


def test_ensure_task_event_file_preserves_existing_event(tmp_path):
    config.mark_task_backend_changed("unit", root=tmp_path)
    event_path = config.task_event_path(tmp_path)
    event_text = event_path.read_text(encoding="utf-8")

    ensured = config.ensure_task_event_file(tmp_path)

    assert ensured == event_path
    assert ensured.read_text(encoding="utf-8") == event_text


def test_default_backend_root_is_shared_spice_dir(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    monkeypatch.delenv(config.TASK_BACKEND_ENV, raising=False)
    config.set_backend(None)
    common = config.git_common_dir(repo)

    assert config.backend_root() == common / "spice"
    assert config.data_dir() == common / "spice" / "data"
    assert config.taskrc_path() == common / "spice" / "taskrc"
    assert team_database_path() == common / "spice" / "data" / TEAM_DATABASE_FILENAME


def test_task_backend_override_requires_absolute_path(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    config.set_backend("scratch")

    try:
        with pytest.raises(SpiceError, match="requires an absolute path"):
            config.backend_root()
    finally:
        config.set_backend(None)


def test_task_backend_absolute_override_is_backend_root(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "scratch-backend"
    monkeypatch.chdir(repo)
    config.set_backend(str(backend))

    try:
        assert config.backend_root() == backend.resolve()
    finally:
        config.set_backend(None)


def test_manual_project_depth_defaults_require_child_and_cap_three(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)

    assert config.project_depth_bounds() == (2, 3)
    assert config.validate_project("task") == "task"
    assert config.validate_assignable_project("task") == "task"
    assert config.validate_assignable_project("task.unit") == "task.unit"
    assert config.validate_assignable_project("task.unit.child") == "task.unit.child"
    assert (
        config.validate_assignable_project("task.unit.child.extra")
        == "task.unit.child.extra"
    )

    with pytest.raises(
        SpiceError, match=r"at least 2 dotted segments, such as task\.example"
    ):
        config.validate_manual_creation_project("task")
    with pytest.raises(
        SpiceError, match=r"at most 3 dotted segments, such as task\.example"
    ):
        config.validate_manual_creation_project("task.unit.child.extra")
    with pytest.raises(
        SpiceError, match=r"at least 2 dotted segments, such as serve\.example"
    ):
        config.validate_manual_creation_project("serve")


def test_hidden_oops_project_is_addressable_but_not_publicly_assignable(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)

    assert config.hidden_stems() == ("oops",)
    assert config.validate_project(".oops") == ".oops"
    assert config.validate_project(".oops.triage") == ".oops.triage"
    assert config.project_stem(".oops.triage") == "oops"
    assert config.is_hidden_project(".oops.triage")
    assert config.resolve_flow(None, ".oops") == ["todo"]
    assert "oops" not in config.approved_stems()
    assert "oops" not in config.APPROVED_PHASES

    catalog = config.task_project_validation_catalog()
    assert catalog["hiddenStems"] == ["oops"]
    assert catalog["hiddenProjectPrefix"] == "."

    with pytest.raises(SpiceError, match="hidden project stem 'scratch'"):
        config.validate_project(".scratch")
    with pytest.raises(SpiceError, match="not lane-filter assignable"):
        config.validate_assignable_project(".oops")
    with pytest.raises(SpiceError, match="reserved for system task creation"):
        config.validate_manual_creation_project(".oops")


def test_configured_hidden_project_stems_are_addressable_not_assignable(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    pyproject = repo / "pyproject.toml"
    pyproject.write_text(
        '[tool.spice.tasks]\nhidden_stems = ["scratch", "audit", "oops"]\n',
        encoding="utf-8",
    )

    assert config.hidden_stems() == ("oops", "scratch", "audit")
    assert config.validate_project(".scratch") == ".scratch"
    assert config.validate_project(".audit.triage") == ".audit.triage"
    assert config.project_stem(".audit.triage") == "audit"
    assert config.is_hidden_project(".scratch")
    assert config.resolve_flow(None, ".scratch") == ["todo"]
    assert "scratch" not in config.approved_stems()

    catalog = config.task_project_validation_catalog()
    assert catalog["hiddenStems"] == ["oops", "scratch", "audit"]

    with pytest.raises(SpiceError, match="not lane-filter assignable"):
        config.validate_assignable_project(".scratch")
    with pytest.raises(SpiceError, match="reserved for system task creation"):
        config.validate_manual_creation_project(".scratch")


def test_configured_hidden_project_stems_reject_invalid_definitions(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    pyproject = repo / "pyproject.toml"

    pyproject.write_text(
        '[tool.spice.tasks]\nhidden_stems = [".scratch"]\n',
        encoding="utf-8",
    )
    with pytest.raises(SpiceError, match="omit the leading"):
        config.hidden_stems()

    pyproject.write_text(
        '[tool.spice.tasks]\nhidden_stems = ["bad-stem"]\n',
        encoding="utf-8",
    )
    with pytest.raises(SpiceError, match="lowercase letters, digits, and underscores"):
        config.hidden_stems()

    pyproject.write_text(
        '[tool.spice.tasks]\nhidden_stems = ["task"]\n',
        encoding="utf-8",
    )
    with pytest.raises(SpiceError, match="conflicts with an approved public"):
        config.hidden_stems()


def test_manual_project_depth_uses_repo_config_overrides(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    pyproject = repo / "pyproject.toml"
    pyproject.write_text(
        "[tool.spice.tasks]\nproject_min_depth = 1\nproject_max_depth = 2\n",
        encoding="utf-8",
    )

    assert config.project_depth_bounds() == (1, 2)
    assert config.validate_assignable_project("task") == "task"
    assert config.validate_assignable_project("task.unit") == "task.unit"
    assert config.validate_manual_creation_project("task") == "task"
    assert config.validate_manual_creation_project("task.unit") == "task.unit"
    with pytest.raises(
        SpiceError, match=r"at most 2 dotted segments, such as task\.example"
    ):
        config.validate_manual_creation_project("task.unit.child")

    pyproject.write_text(
        "[tool.spice.tasks]\nproject_min_depth = 3\nproject_max_depth = 4\n",
        encoding="utf-8",
    )
    catalog = config.task_project_validation_catalog()

    assert config.project_depth_bounds() == (3, 4)
    assert (
        render.public_task_project_depth_label()
        == "public task project depth 3..4 dotted segments"
    )
    assert config.validate_assignable_project("task.unit.child") == "task.unit.child"
    assert config.validate_assignable_project("task.unit") == "task.unit"
    assert (
        config.validate_manual_creation_project("task.unit.child") == "task.unit.child"
    )
    with pytest.raises(
        SpiceError, match=r"at least 3 dotted segments, such as task\.example\.unit"
    ):
        config.validate_manual_creation_project("task.unit")
    assert catalog["projectMinDepth"] == 3
    assert catalog["projectMaxDepth"] == 4
    assert "task.example.unit" in catalog["projectExamples"]


def test_project_depth_rejects_invalid_config(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)
    pyproject = repo / "pyproject.toml"

    pyproject.write_text(
        "[tool.spice.tasks]\nproject_min_depth = 0\n",
        encoding="utf-8",
    )
    with pytest.raises(SpiceError, match=re.escape("project_min_depth")):
        config.project_depth_bounds()

    pyproject.write_text(
        "[tool.spice.tasks]\nproject_min_depth = 4\nproject_max_depth = 3\n",
        encoding="utf-8",
    )
    with pytest.raises(SpiceError, match=re.escape("project_max_depth")):
        config.project_depth_bounds()


def test_phase_launch_overrides_reads_per_driver_phase_table(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    (repo / "pyproject.toml").write_text(
        "[tool.spice.tasks.phase_models.claude.plan]\n"
        'model = "claude-opus-4-8"\n'
        'effort = "high"\n'
        "\n"
        "[tool.spice.tasks.phase_models.claude.todo]\n"
        'model = "claude-sonnet-5"\n',
        encoding="utf-8",
    )

    assert config.phase_launch_overrides(repo, "claude", "plan") == {
        "model": "claude-opus-4-8",
        "effort": "high",
    }
    assert config.phase_launch_overrides(repo, "claude", "todo") == {
        "model": "claude-sonnet-5",
    }


def test_phase_launch_overrides_falls_back_for_unmapped_phase_or_driver(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    (repo / "pyproject.toml").write_text(
        '[tool.spice.tasks.phase_models.claude.plan]\nmodel = "claude-opus-4-8"\n',
        encoding="utf-8",
    )

    assert config.phase_launch_overrides(repo, "claude", "verify") == {}
    assert config.phase_launch_overrides(repo, "codex", "plan") == {}
    assert config.phase_launch_overrides(repo, "claude", "") == {}
    assert config.phase_launch_overrides(repo, "", "plan") == {}


def test_study_phase_is_approved_and_catalogued(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)

    assert "study" in config.APPROVED_PHASES
    assert config.resolve_flow(
        ["study", "plan", "todo", "verify", "review"], "task.unit"
    ) == ["study", "plan", "todo", "verify", "review"]
    assert "study" in config.uda_schema()["phase"]["values"]
    assert "study" in config.task_project_validation_catalog()["approvedPhases"]


def _init_repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "-b", "main"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    )
    return path
