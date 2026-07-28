"""Operator-owned worktree state has one Git-dir path and one migration."""

from __future__ import annotations

import ast
import json
import subprocess
from pathlib import Path

import pytest

from spice.config import edit, layers
from spice.errors import SpiceError
from spice.hooks.initplan import (
    InitializationMode,
    apply_initialization_plan,
    initialization_receipt_path,
    load_initialization_receipt,
    plan_initialization,
)
from spice.operatorstate import (
    INITIALIZATION_RECEIPT_PATH,
    OPERATOR_STATE_RELOCATION_RELEASE,
    WORKTREE_CONFIG_PATH,
    operator_state_migration_marker,
    operator_state_path,
)
from spice.paths import git_dir


def test_untracked_worktree_configuration_migrates_once_then_refuses(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    withdrawn = repo / WORKTREE_CONFIG_PATH.withdrawn_relative
    _write(withdrawn, 'agent.model = "operator-local"\n')

    loaded = layers.load_config(repo)
    canonical = edit.worktree_config_path(repo)
    marker = operator_state_migration_marker(repo, WORKTREE_CONFIG_PATH)

    assert loaded.effective["agent"]["model"] == "operator-local"
    assert canonical == git_dir(repo) / ".spice" / "config" / "spice.toml"
    assert canonical.read_text(encoding="utf-8") == 'agent.model = "operator-local"\n'
    assert not withdrawn.exists()
    assert json.loads(marker.read_text(encoding="utf-8")) == {
        "canonical_path": str(canonical),
        "kind": "worktree-config",
        "release": OPERATOR_STATE_RELOCATION_RELEASE,
        "schema_version": 1,
        "withdrawn_path": ".spice/config/spice.toml",
    }

    _write(withdrawn, 'agent.model = "second-path"\n')
    with pytest.raises(
        SpiceError,
        match=rf"withdrawn in {OPERATOR_STATE_RELOCATION_RELEASE}.*already been migrated",
    ):
        layers.load_config(repo)
    assert canonical.read_text(encoding="utf-8") == 'agent.model = "operator-local"\n'


def test_untracked_initialization_receipt_migrates_once_then_refuses(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    expected = apply_initialization_plan(
        plan_initialization(repo, InitializationMode.GATES_ONLY)
    )
    canonical = initialization_receipt_path(repo)
    withdrawn = repo / INITIALIZATION_RECEIPT_PATH.withdrawn_relative
    withdrawn.parent.mkdir(parents=True, exist_ok=True)
    canonical.replace(withdrawn)

    loaded = load_initialization_receipt(repo)

    assert loaded == expected
    assert canonical.is_file()
    assert not withdrawn.exists()
    assert operator_state_migration_marker(repo, INITIALIZATION_RECEIPT_PATH).is_file()

    withdrawn.write_bytes(canonical.read_bytes())
    with pytest.raises(
        SpiceError,
        match=rf"withdrawn in {OPERATOR_STATE_RELOCATION_RELEASE}.*already been migrated",
    ):
        load_initialization_receipt(repo)


def test_clone_shipping_each_withdrawn_path_never_honors_either(tmp_path):
    source = _init_repo(tmp_path / "source")
    _write(
        source / WORKTREE_CONFIG_PATH.withdrawn_relative,
        'agent.model = "repository-smuggled"\n',
    )
    _write(
        source / INITIALIZATION_RECEIPT_PATH.withdrawn_relative,
        '{"repository": "repository-smuggled"}\n',
    )
    _git(
        source,
        "add",
        "-f",
        WORKTREE_CONFIG_PATH.withdrawn_relative.as_posix(),
        INITIALIZATION_RECEIPT_PATH.withdrawn_relative.as_posix(),
    )
    _git(source, "commit", "-m", "ship withdrawn operator state")
    clone = tmp_path / "clone"
    subprocess.run(
        ["git", "clone", "-q", str(source), str(clone)],
        check=True,
        capture_output=True,
        text=True,
    )

    with pytest.raises(
        SpiceError,
        match=rf"tracked worktree configuration path .*withdrawn in "
        rf"{OPERATOR_STATE_RELOCATION_RELEASE}.*never honored",
    ):
        layers.load_config(clone)
    with pytest.raises(
        SpiceError,
        match=rf"tracked initialization receipt path .*withdrawn in "
        rf"{OPERATOR_STATE_RELOCATION_RELEASE}.*never honored",
    ):
        load_initialization_receipt(clone)

    assert not operator_state_path(clone, WORKTREE_CONFIG_PATH).exists()
    assert not operator_state_path(clone, INITIALIZATION_RECEIPT_PATH).exists()


def test_operator_state_paths_are_distinct_for_linked_worktrees(tmp_path):
    primary = _init_repo(tmp_path / "primary")
    (primary / "README.md").write_text("linked\n", encoding="utf-8")
    _git(primary, "add", "README.md")
    _git(primary, "commit", "-m", "seed")
    linked = tmp_path / "linked"
    _git(primary, "worktree", "add", "-b", "linked", str(linked))

    assert edit.worktree_config_path(primary) == (
        git_dir(primary) / ".spice" / "config" / "spice.toml"
    )
    assert edit.worktree_config_path(linked) == (
        git_dir(linked) / ".spice" / "config" / "spice.toml"
    )
    assert initialization_receipt_path(primary) == (
        git_dir(primary) / ".spice" / "init-receipt.json"
    )
    assert initialization_receipt_path(linked) == (
        git_dir(linked) / ".spice" / "init-receipt.json"
    )
    assert edit.worktree_config_path(primary) != edit.worktree_config_path(linked)
    assert initialization_receipt_path(primary) != initialization_receipt_path(linked)


def test_source_tree_outside_git_has_no_worktree_configuration_path(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "project-model"\n',
        encoding="utf-8",
    )
    withdrawn = tmp_path / WORKTREE_CONFIG_PATH.withdrawn_relative
    _write(withdrawn, 'agent.model = "visible-predecessor"\n')

    loaded = layers.load_config(tmp_path)

    assert loaded.effective["agent"]["model"] == "project-model"
    assert loaded.layer(layers.WORKTREE_SOURCE).path is None
    assert loaded.layer(layers.WORKTREE_SOURCE).present is False
    assert withdrawn.read_text(encoding="utf-8") == (
        'agent.model = "visible-predecessor"\n'
    )


def test_bare_worktree_state_path_gate_has_only_live_or_generated_owners():
    root = Path(__file__).parents[1]

    assert _bare_worktree_state_references(root) == {
        (
            "spice/agent/runinbox.py",
            "inbox_pending_signature",
            "worktree_inbox_dir",
        ),
        ("spice/hooks/install.py", "hooks_dir", "STATE_DIRNAME"),
        ("spice/hooks/install.py", "materialize_state_gitignore", "STATE_DIRNAME"),
        ("spice/mail/inbox.py", "inbox_dir", "worktree_inbox_dir"),
        (
            "spice/mail/inbox.py",
            "inbox_event_path",
            "worktree_runtime_state_root",
        ),
        (
            "spice/operatorstate.py",
            "_remove_empty_withdrawn_parent",
            "STATE_DIRNAME",
        ),
        ("spice/paths.py", "worktree_inbox_dir", "worktree_runtime_state_root"),
        ("spice/paths.py", "worktree_runtime_state_root", "STATE_DIRNAME"),
        (
            "spice/serve/browser/artifacts.py",
            "serve_browser_artifact_path",
            "worktree_runtime_state_root",
        ),
        (
            "spice/sessions/learnings.py",
            "learning_store_path",
            "worktree_runtime_state_root",
        ),
    }


class _BareWorktreeStateVisitor(ast.NodeVisitor):
    def __init__(self, relative: str) -> None:
        self.relative = relative
        self.functions: list[str] = []
        self.references: set[tuple[str, str, str]] = set()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self.functions.append(node.name)
        self.generic_visit(node)
        self.functions.pop()

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_Call(self, node: ast.Call) -> None:
        name = _call_name(node.func)
        if name in {"worktree_runtime_state_root", "worktree_inbox_dir"}:
            self._record(name)
        self.generic_visit(node)

    def visit_BinOp(self, node: ast.BinOp) -> None:
        if (
            isinstance(node.op, ast.Div)
            and _is_state_dir(node.right)
            and not _is_git_state_root(node.left)
        ):
            self._record("STATE_DIRNAME")
        self.generic_visit(node)

    def _record(self, symbol: str) -> None:
        self.references.add(
            (
                self.relative,
                self.functions[-1] if self.functions else "<module>",
                symbol,
            )
        )


def _bare_worktree_state_references(
    root: Path,
) -> set[tuple[str, str, str]]:
    references: set[tuple[str, str, str]] = set()
    for source in sorted((root / "spice").rglob("*.py")):
        relative = source.relative_to(root).as_posix()
        visitor = _BareWorktreeStateVisitor(relative)
        visitor.visit(ast.parse(source.read_text(encoding="utf-8"), filename=relative))
        references.update(visitor.references)
    return references


def _call_name(node: ast.expr) -> str:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return node.attr
    return ""


def _is_state_dir(node: ast.expr) -> bool:
    return (
        isinstance(node, ast.Name)
        and node.id == "STATE_DIRNAME"
        or isinstance(node, ast.Constant)
        and node.value == ".spice"
    )


def _is_git_state_root(node: ast.expr) -> bool:
    return isinstance(node, ast.Call) and _call_name(node.func) in {
        "git_common_dir",
        "git_dir",
    }


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _init_repo(path: Path) -> Path:
    path.mkdir()
    subprocess.run(
        ["git", "init", "-q", "-b", "main"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    )
    _git(path, "config", "user.email", "spice@example.test")
    _git(path, "config", "user.name", "Spice Tests")
    return path


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
