"""Task-add batch parser seam."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.errors import SpiceError
from spice.tasks import config, identity, ops, tw

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-taskbatch")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_parse_add_batch_returns_typed_requests_without_creating_tasks(task_repo):
    requests = ops.parse_add_batch(
        [
            "title=Typed batch | project=task.unit | description=Parser seam | "
            "priority=high | flow=todo,review | tags=parser,inline | "
            "acceptance=Parsed without creation | due=2026-06-30"
        ]
    )

    assert requests == [
        ops.TaskAddBatchRequest(
            title="Typed batch",
            description="Parser seam",
            project="task.unit",
            priority="high",
            flow=("todo", "review"),
            tags=("parser", "inline"),
            acceptance=("Parsed without creation",),
            due="2026-06-30",
        )
    ]
    assert tw.export(["status:pending"]) == []


def test_parse_add_batch_accepts_task_directive_prefix(task_repo):
    requests = ops.parse_add_batch(
        [
            "TASK: title=Prefixed batch | project=task.unit | "
            "acceptance=Same batch parser"
        ]
    )

    assert requests == [
        ops.TaskAddBatchRequest(
            title="Prefixed batch",
            description=None,
            project="task.unit",
            priority=config.DEFAULT_PRIORITY,
            flow=(),
            tags=(),
            acceptance=("Same batch parser",),
            due=None,
        )
    ]
    assert tw.export(["status:pending"]) == []


def test_add_batch_validates_all_lines_before_creating_tasks(task_repo):
    with pytest.raises(SpiceError, match="batch add rejected"):
        ops.add_batch(
            [
                "title=Would otherwise create | project=task.unit | acceptance=ok",
                "title=Invalid project depth | project=task | acceptance=bad",
            ]
        )

    assert not any(
        row.get("description") == "Would otherwise create"
        for row in tw.export(["status:pending"])
    )


def test_add_batch_creates_from_parsed_requests(task_repo):
    handles = ops.add_batch(
        [
            "title=Created batch | project=task.unit | description=Batch body | "
            "priority=low | acceptance=Batch creation still works"
        ]
    )
    row = identity.resolve(handles[0])

    assert row["description"] == "Created batch"
    assert row["task_description"] == "Batch body"
    assert row["project"] == "task.unit"
    assert row["priority"] == "L"
    assert row["acceptance"] == "Batch creation still works"


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
