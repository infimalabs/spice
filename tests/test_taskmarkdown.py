"""Task DAG markdown import/export."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.cli.parser import build_parser
from spice.tasks import config, create, identity, markdown, ops, tw

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
    monkeypatch.setenv("CODEX_TURN_ID", "turn-taskmarkdown")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)


def test_canonical_markdown_round_trips_parser_shape(task_repo):
    dag = markdown.MarkdownTaskDag(
        root="root",
        nodes=(
            markdown.MarkdownTaskNode(
                id="child",
                title="Implement child",
                project="task.unit",
                acceptance=("child acceptance",),
                annotations=("```python\nprint('ok')\n```",),
            ),
            markdown.MarkdownTaskNode(
                id="root",
                title="Plan root",
                project="task.unit",
                flow=("plan", "todo", "review"),
                acceptance=("root acceptance",),
                after=("child",),
            ),
        ),
    )

    rendered = markdown.render_canonical(dag)
    parsed = markdown.parse_markdown(rendered)

    assert markdown.render_canonical(parsed) == rendered


def test_freeform_markdown_maps_nodes_edges_and_annotations(task_repo):
    dag = markdown.parse_freeform_markdown(
        """
# Root task
Acceptance: parent accepted

Parent description.

## Child task
- Grandchild task
  Acceptance: grandchild accepted

> decision note
```text
raw note
```
""",
        default_project="task.unit",
    )
    nodes = {node.id: node for node in dag.nodes}

    assert dag.root == "root-task"
    assert nodes["root-task"].after == ("child-task",)
    assert nodes["child-task"].after == ("grandchild-task",)
    assert nodes["root-task"].description == "Parent description."
    assert nodes["root-task"].acceptance == ("parent accepted",)
    assert nodes["grandchild-task"].acceptance == ("grandchild accepted",)
    assert "> decision note" in nodes["grandchild-task"].annotations
    assert "```text\nraw note\n```" in nodes["grandchild-task"].annotations


def test_task_ingest_creates_tasks_edges_and_annotations(task_repo, tmp_path, capsys):
    source = tmp_path / "backlog.md"
    source.write_text(
        "# Parent task\n"
        "Acceptance: parent accepted\n\n"
        "## Child task\n"
        "Acceptance: child accepted\n"
        "> child note\n",
        encoding="utf-8",
    )
    args = build_parser().parse_args(
        ["task", "ingest", str(source), "--project", "task.unit"]
    )
    args.backend = str(config.backend_root())

    assert args.func(args) == 0
    output = capsys.readouterr().out
    root_handle = next(
        line.split()[1] for line in output.splitlines() if line.startswith("root ")
    )
    parent = identity.resolve(root_handle)
    child_uuid = parent["depends"][0]
    child = tw.export([child_uuid])[0]

    assert parent["description"] == "Parent task"
    assert parent["acceptance"] == "parent accepted"
    assert child["description"] == "Child task"
    assert child["acceptance"] == "child accepted"
    assert any(
        ann.get("description") == "markdown-id: child-task"
        for ann in child.get("annotations") or []
    )
    assert any(
        ann.get("description") == "> child note"
        for ann in child.get("annotations") or []
    )


def test_task_ledger_exports_dependency_closure(task_repo, capsys):
    child = create.add(
        "Ledger child",
        project="task.unit",
        acceptance=["child accepted"],
    )
    parent = create.add(
        "Ledger parent",
        project="task.unit",
        acceptance=["parent accepted"],
        after=[child],
    )
    ops.note(child, "markdown-id: child-node")

    args = build_parser().parse_args(["task", "ledger", parent])
    args.backend = str(config.backend_root())

    assert args.func(args) == 0
    exported = capsys.readouterr().out
    dag = markdown.parse_markdown(exported)
    nodes = {node.id: node for node in dag.nodes}

    assert dag.root == parent
    assert nodes[parent].after == ("child-node",)
    assert nodes["child-node"].title == "Ledger child"
    assert nodes["child-node"].acceptance == ("child accepted",)


def test_canonical_ingest_preserves_absent_priority(task_repo):
    dag = markdown.MarkdownTaskDag(
        root="root",
        nodes=(
            markdown.MarkdownTaskNode(
                id="root",
                title="No priority root",
                project="task.unit",
                flow=("todo", "review"),
            ),
        ),
    )

    output = markdown.create_task_dag(dag)
    handle = next(
        line.split()[1] for line in output.splitlines() if line.startswith("root ")
    )
    row = identity.resolve(handle)
    exported = markdown.parse_markdown(markdown.render_ledger(handle))
    exported_root = next(node for node in exported.nodes if node.id == "root")

    assert str(row.get("priority") or "") == ""
    assert exported_root.priority == ""


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
