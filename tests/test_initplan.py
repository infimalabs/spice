"""Receipt-shaped initialization planning across repository layouts and modes."""

from __future__ import annotations

import stat
import subprocess
from pathlib import Path

from spice.agent.lifecycle import WORKTREE_SKILL_RELATIVE_PATH
from spice.hooks.initplan import (
    InitOperation,
    InitOperationKind,
    InitOperationScope,
    InitializationMode,
    InitializationPlan,
    apply_initialization_plan,
    plan_initialization,
)


def test_full_plan_is_ordered_complete_deterministic_and_side_effect_free(tmp_path):
    repo = _git_init(tmp_path / "repo")
    config = repo / ".git" / "config"
    before_entries = tuple(sorted(path.name for path in repo.iterdir()))
    before_git_entries = tuple(sorted(path.name for path in (repo / ".git").iterdir()))
    before_config = config.read_text(encoding="utf-8")

    plan = plan_initialization(repo)
    repeated = plan_initialization(repo)

    assert plan == repeated
    assert tuple(operation.target for operation in plan.operations) == (
        "extensions.worktreeConfig",
        "core.bare",
        ".spice/hooks/pre-commit",
        ".spice/hooks/commit-msg",
        ".spice/hooks/reference-transaction",
        "core.hooksPath",
        ".spice/.gitignore",
        ".agents/skills/spice/.gitignore",
        ".agents/skills/spice/SKILL.md",
    )
    assert {operation.initialization_mode for operation in plan.operations} == {
        InitializationMode.FULL
    }
    assert tuple(sorted(path.name for path in repo.iterdir())) == before_entries
    assert tuple(sorted(path.name for path in (repo / ".git").iterdir())) == (
        before_git_entries
    )
    assert config.read_text(encoding="utf-8") == before_config
    assert {
        operation.target: operation.generated_mode
        for operation in plan.operations
        if operation.kind is InitOperationKind.FILE
    } == {
        ".spice/hooks/pre-commit": 0o755,
        ".spice/hooks/commit-msg": 0o755,
        ".spice/hooks/reference-transaction": 0o755,
        ".spice/.gitignore": 0o644,
        ".agents/skills/spice/.gitignore": 0o600,
        ".agents/skills/spice/SKILL.md": 0o600,
    }
    assert {len(operation.ownership_digest) for operation in plan.operations} == {64}
    assert {
        len(bytes.fromhex(operation.ownership_digest)) for operation in plan.operations
    } == {32}


def test_apply_consumes_the_existing_plan_and_realizes_every_managed_operation(
    tmp_path,
):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo)
    planned_operations = plan.operations

    result = apply_initialization_plan(plan)

    assert result is None
    assert plan.operations == planned_operations
    assert tuple(_realized_value(repo, operation) for operation in plan.operations) == (
        tuple(operation.generated_value for operation in plan.operations)
    )
    assert tuple(
        stat.S_IMODE((repo / operation.target).stat().st_mode)
        for operation in plan.operations
        if operation.kind is InitOperationKind.FILE
    ) == tuple(
        operation.generated_mode
        for operation in plan.operations
        if operation.kind is InitOperationKind.FILE
    )


def test_gates_only_plan_uses_the_same_model_for_its_bounded_surface(tmp_path):
    repo = _git_init(tmp_path / "repo")

    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
    apply_initialization_plan(plan)

    assert tuple(operation.target for operation in plan.operations) == (
        "extensions.worktreeConfig",
        "core.bare",
        ".spice/hooks/pre-commit",
        ".spice/hooks/commit-msg",
        "core.hooksPath",
        ".spice/.gitignore",
    )
    assert {operation.initialization_mode for operation in plan.operations} == {
        InitializationMode.GATES_ONLY
    }
    assert tuple(
        sorted(
            path.relative_to(repo).as_posix()
            for path in (repo / ".spice").rglob("*")
            if path.is_file()
        )
    ) == (
        ".spice/.gitignore",
        ".spice/hooks/commit-msg",
        ".spice/hooks/pre-commit",
    )


def test_custom_common_hooks_path_is_preserved_as_effective_prior_state(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _git(repo, "config", "core.hooksPath", ".custom-hooks")

    plan = plan_initialization(repo)
    operation = _operation(
        plan,
        "core.hooksPath",
        InitOperationScope.WORKTREE_GIT_CONFIG,
    )

    assert (
        operation.previous_value,
        operation.previous_effective_value,
        operation.generated_value,
        operation.introduced,
    ) == (None, ".custom-hooks", ".spice/hooks", True)
    apply_initialization_plan(plan)
    assert _git_config_file(repo / ".git" / "config", "core.hooksPath") == (
        ".custom-hooks"
    )
    assert _git_config_file(repo / ".git" / "config.worktree", "core.hooksPath") == (
        ".spice/hooks"
    )


def test_preexisting_shared_and_worktree_values_are_not_marked_introduced(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _git(repo, "config", "extensions.worktreeConfig", "true")
    _git(repo, "config", "--worktree", "core.bare", "false")
    _git(repo, "config", "--worktree", "core.hooksPath", ".spice/hooks")

    plan = plan_initialization(repo)
    provenance = tuple(
        (
            operation.target,
            operation.scope,
            operation.previous_value,
            operation.introduced,
            operation.will_change,
        )
        for operation in plan.operations
        if operation.kind is InitOperationKind.GIT_CONFIG
    )

    assert provenance == (
        (
            "extensions.worktreeConfig",
            InitOperationScope.COMMON_GIT_CONFIG,
            "true",
            False,
            False,
        ),
        (
            "core.bare",
            InitOperationScope.WORKTREE_GIT_CONFIG,
            "false",
            False,
            False,
        ),
        (
            "core.hooksPath",
            InitOperationScope.WORKTREE_GIT_CONFIG,
            ".spice/hooks",
            False,
            False,
        ),
    )


def test_linked_bare_common_worktree_can_be_planned_before_git_bootstrap(tmp_path):
    from spice.hooks.cli import init_repo_root

    seed = _git_init(tmp_path / "seed")
    (seed / "README.md").write_text("seed\n", encoding="utf-8")
    _git(seed, "add", "README.md")
    _git(seed, "commit", "-m", "seed")
    common = tmp_path / "common.git"
    _run(["git", "clone", "--bare", str(seed), str(common)])
    lane = tmp_path / "lane"
    _git(common, "worktree", "add", str(lane), "main")
    common_config_before = (common / "config").read_text(encoding="utf-8")

    discovered = init_repo_root(lane)
    plan = plan_initialization(discovered)
    bare = _operation(plan, "core.bare", InitOperationScope.WORKTREE_GIT_CONFIG)

    assert (
        bare.scope_path,
        bare.previous_value,
        bare.previous_effective_value,
        bare.generated_value,
        bare.introduced,
    ) == (
        common / "worktrees" / "lane" / "config.worktree",
        None,
        "true",
        "false",
        True,
    )
    assert (discovered, (common / "config").read_text(encoding="utf-8")) == (
        lane,
        common_config_before,
    )
    apply_initialization_plan(plan)
    assert _git(lane, "rev-parse", "--is-bare-repository").stdout.strip() == "false"
    assert _git(
        lane, "config", "--worktree", "--get", "core.hooksPath"
    ).stdout.strip() == (".spice/hooks")


def test_tracked_custom_skill_is_inventoried_but_preserved(tmp_path):
    repo = _git_init(tmp_path / "repo")
    skill = repo / WORKTREE_SKILL_RELATIVE_PATH
    skill.parent.mkdir(parents=True)
    skill.write_text("# Repository-owned spice skill\n", encoding="utf-8")
    skill.chmod(0o644)
    _git(repo, "add", WORKTREE_SKILL_RELATIVE_PATH.as_posix())
    _git(repo, "commit", "-m", "repository skill")

    plan = plan_initialization(repo)
    operation = _operation(
        plan,
        WORKTREE_SKILL_RELATIVE_PATH.as_posix(),
        InitOperationScope.WORKTREE_FILE,
    )

    assert (
        operation.previous_value,
        operation.previous_mode,
        operation.generated_mode,
        operation.introduced,
        operation.managed,
        operation.will_change,
    ) == ("# Repository-owned spice skill\n", 0o644, 0o644, False, False, False)
    apply_initialization_plan(plan)
    assert skill.read_text(encoding="utf-8") == "# Repository-owned spice skill\n"


def _operation(
    plan: InitializationPlan,
    target: str,
    scope: InitOperationScope,
) -> InitOperation:
    return next(
        operation
        for operation in plan.operations
        if (operation.target, operation.scope) == (target, scope)
    )


def _realized_value(repo: Path, operation: InitOperation) -> str:
    if operation.kind is InitOperationKind.FILE:
        return (repo / operation.target).read_text(encoding="utf-8")
    return _git_config_file(operation.scope_path, operation.target)


def _git_config_file(path: Path, key: str) -> str:
    return _run(["git", "config", "--file", str(path), "--get", key]).stdout.strip()


def _git_init(repo: Path) -> Path:
    _run(["git", "init", "-b", "main", str(repo)])
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Spice Test")
    return repo


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return _run(["git", "-C", str(repo), *args])


def _run(argv: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        check=True,
        capture_output=True,
        text=True,
    )
