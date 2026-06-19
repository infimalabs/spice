"""Task backend file helpers."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.tasks import config


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


def test_assignable_project_depth_defaults_require_child_and_cap_three(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    monkeypatch.chdir(repo)

    assert config.project_depth_bounds() == (2, 3)
    assert config.validate_project("task") == "task"
    assert config.validate_assignable_project("task.unit") == "task.unit"
    assert config.validate_assignable_project("task.unit.child") == "task.unit.child"

    with pytest.raises(
        SpiceError, match=r"at least 2 dotted segments, such as task\.example"
    ):
        config.validate_assignable_project("task")
    with pytest.raises(
        SpiceError, match=r"at most 3 dotted segments, such as task\.example"
    ):
        config.validate_assignable_project("task.unit.child.extra")
    with pytest.raises(
        SpiceError, match=r"at least 2 dotted segments, such as serve\.example"
    ):
        config.validate_manual_creation_project("serve")


def test_assignable_project_depth_uses_repo_config_overrides(tmp_path, monkeypatch):
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
    with pytest.raises(
        SpiceError, match=r"at most 2 dotted segments, such as task\.example"
    ):
        config.validate_assignable_project("task.unit.child")

    pyproject.write_text(
        "[tool.spice.tasks]\nproject_min_depth = 3\nproject_max_depth = 4\n",
        encoding="utf-8",
    )
    catalog = config.task_project_validation_catalog()

    assert config.project_depth_bounds() == (3, 4)
    assert config.validate_assignable_project("task.unit.child") == "task.unit.child"
    with pytest.raises(
        SpiceError, match=r"at least 3 dotted segments, such as task\.example\.unit"
    ):
        config.validate_assignable_project("task.unit")
    assert catalog["projectMinDepth"] == 3
    assert catalog["projectMaxDepth"] == 4
    assert "task.example.unit" in catalog["projectExamples"]


def test_assignable_project_depth_rejects_invalid_config(tmp_path, monkeypatch):
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
