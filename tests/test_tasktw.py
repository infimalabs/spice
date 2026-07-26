"""Taskwarrior process-layer event signaling."""

from __future__ import annotations

import shutil
import subprocess

import pytest

from spice.tasks import config, create, tw
from tests.test_reposcaffolding import (
    init_committed_repo as _init_repo,
)
from tests.test_reposcaffolding import (
    make_task_repo_fixture,
)

pytestmark = pytest.mark.skipif(
    shutil.which("task") is None, reason="Taskwarrior binary is required"
)

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


task_repo = make_task_repo_fixture(lambda path: _init_repo(path), actor=ACTOR_A)


def test_task_event_file_advances_on_mutation_and_stays_stable_on_export(task_repo):
    event_path = config.ensure_task_event_file()
    before = event_path.read_text(encoding="utf-8")
    before_revision = config.task_event_revision()

    create.add("event signal", project="task.unit", origin="ack:1jN54zJJ")
    after_add = event_path.read_text(encoding="utf-8")
    after_add_revision = config.task_event_revision()
    tw.export(["status:pending"])
    after_export = event_path.read_text(encoding="utf-8")
    after_export_revision = config.task_event_revision()

    assert len({before, after_add}) == 2
    assert after_export == after_add
    assert len({before_revision, after_add_revision}) == 2
    assert after_export_revision == after_add_revision


def test_task_run_disables_bulk_confirmation(monkeypatch, tmp_path):
    seen: dict[str, list[str]] = {}

    class Result:
        returncode = 0
        stdout = "[]"
        stderr = ""

    def fake_run(command, **_kwargs):
        seen["command"] = command
        return Result()

    monkeypatch.setattr(tw, "require_task_binary", lambda: None)
    monkeypatch.setattr(config, "bootstrap", lambda: tmp_path / "taskrc")
    monkeypatch.setattr(config, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(subprocess, "run", fake_run)

    tw.run(["export"])

    assert "rc.bulk=0" in seen["command"]
