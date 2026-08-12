"""Install the git hook shims spice owns into a target repo.

Shims are generated under `.spice/hooks/` and activated through the
worktree-local `core.hooksPath`, so nothing spice writes collides with hooks
the repo may already track. The shims invoke the ambient `spice` command
directly; runtime resolution belongs to that command, not to generated hook
files.
"""

from __future__ import annotations

from pathlib import Path

from spice.hooks.initplan import (
    GATE_HOOK_ARGS as GATE_HOOK_ARGS,
    HOOK_ARGS as HOOK_ARGS,
    HOOKS_DIRNAME,
    STATE_GITIGNORE_CONTENT,
    InitOperation,
    InitializationPlan,
    InitializationMode,
    apply_initialization_plan,
    hook_shim_content as hook_shim_content,
    initialization_detail_rows,
    plan_initialization,
)
from spice.paths import STATE_DIRNAME


def hooks_dir(repo_root: Path) -> Path:
    return repo_root / STATE_DIRNAME / HOOKS_DIRNAME


def plan_hook_installation(repo_root: Path) -> InitializationPlan:
    """Inspect the shared initialization repair surface without changing it."""
    return plan_initialization(
        repo_root,
        InitializationMode.FULL,
        include_agent_skill=False,
    )


def install_hooks_for_repo(repo_root: Path) -> list[str]:
    """Write the shims and point `core.hooksPath` at them; return detail rows."""
    plan = plan_hook_installation(repo_root)
    apply_initialization_plan(plan)
    return initialization_detail_rows(plan, include_ready=False)


def observe_hooks_for_repo(repo_root: Path) -> tuple[str, list[str]]:
    """Report the effective hook selection without repairing or claiming it."""
    plan = plan_hook_installation(repo_root)
    expected_path = f"{STATE_DIRNAME}/{HOOKS_DIRNAME}"
    by_target = {operation.target: operation for operation in plan.operations}
    selection = by_target["core.hooksPath"]
    effective_path = selection.previous_effective_value
    if effective_path != expected_path:
        state = "external" if effective_path is not None else "unconfigured"
        return state, [f"core.hooksPath={effective_path or '-'}"]

    hook_operations = [
        (name, by_target[f"{expected_path}/{name}"]) for name in HOOK_ARGS
    ]
    incomplete = [
        _incomplete_hook_detail(name, operation)
        for name, operation in hook_operations
        if operation.will_change
    ]
    if incomplete:
        return "incomplete", [*incomplete, f"core.hooksPath={effective_path}"]
    return "configured", [
        *(f"hook {name} -> {expected_path}/{name}" for name in HOOK_ARGS),
        f"core.hooksPath={effective_path}",
    ]


def _incomplete_hook_detail(name: str, operation: InitOperation) -> str:
    if operation.previous_value is None:
        state = "missing"
    elif operation.previous_value != operation.generated_value:
        state = "stale"
    elif operation.previous_mode != operation.generated_mode:
        state = "not-executable"
    else:  # pragma: no cover - every planned file delta is classified above
        state = "different"
    return f"hook {name}={state}"


def materialize_state_gitignore(repo_root: Path) -> bool:
    """Exclude `.spice/` via a generated directory-local `.gitignore`.

    Mirrors the generated skill-copy ignore file so every spice-owned worktree
    directory carries its own self-ignoring marker instead of relying on the
    git-dir `info/exclude`. Returns True when it (re)wrote the file.
    """
    target = repo_root / STATE_DIRNAME / ".gitignore"
    try:
        if (
            target.is_file()
            and target.read_text(encoding="utf-8") == STATE_GITIGNORE_CONTENT
        ):
            return False
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(STATE_GITIGNORE_CONTENT, encoding="utf-8")
    except OSError:
        return False
    return True
