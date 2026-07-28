"""The pre-commit gate: the constitution, executed.

Steps run in order, collecting every failure before raising, so one commit
attempt reports the whole picture:

1. merge integrity — a merge commit cannot discard a different computed merge
   tree through an index that still equals the first parent;
2. repo shape — namespace packages, path shape, no generic split names;
3. staging — partially staged files are rejected (the fully-staged rule);
4. formatters — staged Python must satisfy `ruff format --check` and
   `ruff check`;
5. local paths — no committed absolute macOS user path literals;
6. serve web typecheck — static browser JavaScript must pass TypeScript
   `checkJs`;
7. python typecheck — the project's own package roots must pass `pyright`;
8. env policy — undeclared environment literals (and, when
   `env_access_gate` is on, undeclared env-access sites);
9. env name ledger — exact manifest accounting for literal env names;
10. shape pressure — file LOC/bytes, routine complexity, magic-number
   regressions, all against staged paths with flex + sticky semantics.

Flex + sticky latches self-heal in-scan: every gate run drops any file,
routine, or doc that measures back at or under its base limit from its own
verdict — the gate forgives exactly when the code earns it, even on an
otherwise failing run. The ledgers behind those latches are rewritten only by
a run this hook accepted, so a refused commit leaves them exactly as it found
them and the author's next attempt meets the same limits this one reported.
"""

from __future__ import annotations

import os
import shlex
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Protocol, TypeVar

from spice.cli.mounts import (
    MOUNTED_COMMAND_ENV,
    VISIBLE_PROG_ENV,
    mount_command_path,
    mounted_commands,
)
from spice.config.values import (
    AGENT_DRIVER_KEY,
    AGENT_MODEL_KEY,
    effective_agent_config,
)
from spice.config.layers import (
    contextualize_config_error,
    effective_table,
    enabled_registry_entries,
)
from spice.errors import SpiceError
from spice.flexstate import FlexSliceClaim
from spice.process.git import run_git_command
from spice.paths import find_tool
from spice.policy import (
    JAVASCRIPT_UNUSED_DECLARATION_EXEMPTIONS,
    LEGITIMATE_INTERNAL_COUPLINGS,
)
from spice.policyconfig import resolve_policy
from spice.process.tool import run_tool_command
from spice.scopes import (
    PRE_COMMIT_STEP_SCOPES,
    SCOPES_KEY,
    ScopeContext,
    ScopeSelector,
)
from spice.studies import (
    complexity,
    envpolicy,
    fileloc,
    gates,
    javascriptunused,
    links,
    localpaths,
    magicnums,
    pythonunused,
    reachability,
    shape,
    taste,
    testquality,
)
from spice.studies.repodocs import (
    render_repo_truth_doc_guard_error,
    repo_truth_doc_candidate_paths,
    repo_truth_doc_findings,
)
from spice.studies.walk import (
    partially_staged_paths,
    staged_paths,
    tracked_paths,
)

STAGED_PATHS_ENV = "SPICE_STAGED_PATHS"  # env-policy: allow
COMMAND_STEP_KEYS = frozenset(
    {"label", "mount", "run", "argv", SCOPES_KEY, "formatter", "enabled"}
)


@dataclass(frozen=True)
class PreCommitFailure:
    label: str
    message: str


@dataclass(frozen=True)
class PreCommitStep:
    key: str
    label: str
    action: Callable[[], None]


@dataclass(frozen=True)
class CommandStep:
    label: str
    argv: tuple[str, ...]
    repo_root: Path
    staged_paths: tuple[Path, ...] = ()
    formatter: bool = False
    # Set for steps that run a mounted command, so the gate step accurately
    # presents as that spice mount (mount env) the same as `spice <name>` does.
    visible_prog: str | None = None


def handle_pre_commit(repo_root: Path) -> int:
    failures: list[PreCommitFailure] = []
    paths = staged_paths(repo_root)
    staging_verified = False
    # A sticky latch records what a landed commit must live with, so it persists
    # only once every step below has accepted this run. A rejected run leaves the
    # ledgers exactly as it found them, rather than latching a breach on work the
    # author is being told to go back and change.
    with gates.deferred_sticky_writes() as pending_sticky:
        for step in pre_commit_steps(repo_root, paths):
            if step.key.startswith("extension-") and not staging_verified:
                continue
            passed = _run_step(failures, step.label, step.action)
            if step.key == "staging" and passed:
                staging_verified = True
        if failures:
            raise _pre_commit_failure_error(failures)
        success_failures: list[PreCommitFailure] = []
        for step in post_success_pre_commit_steps(repo_root, paths):
            _run_step(success_failures, step.label, step.action)
        if success_failures:
            raise _pre_commit_failure_error(success_failures)
        pending_sticky.commit()
    return 0


def _pre_commit_failure_error(failures: list[PreCommitFailure]) -> SpiceError:
    detail = "\n\n".join(
        f"[{failure.label}]\n{failure.message}" for failure in failures
    )
    return SpiceError(f"pre-commit gate failed:\n{detail}")


def _run_step(
    failures: list[PreCommitFailure], label: str, action: Callable[[], None]
) -> bool:
    try:
        action()
        return True
    except SpiceError as exc:
        failures.append(PreCommitFailure(label=label, message=str(exc)))
        return False
    except subprocess.CalledProcessError as exc:
        failures.append(PreCommitFailure(label=label, message=f"command failed: {exc}"))
        return False


def pre_commit_steps(repo_root: Path, paths: list[Path]) -> list[PreCommitStep]:
    """The ordered pre-commit gate after tracked repo policy is applied."""
    try:
        return _pre_commit_steps(repo_root, paths)
    except SpiceError as exc:
        raise contextualize_config_error(
            repo_root, exc, "policy", "pre_commit"
        ) from exc


def _pre_commit_steps(repo_root: Path, paths: list[Path]) -> list[PreCommitStep]:
    steps = _configured_builtin_steps(
        repo_root, _builtin_pre_commit_steps(repo_root, paths)
    )
    steps.extend(_extension_pre_commit_steps(repo_root, paths))
    return steps


def post_success_pre_commit_steps(
    repo_root: Path, paths: list[Path]
) -> list[PreCommitStep]:
    return _configured_command_steps(
        repo_root,
        paths,
        config_key="pre_commit_success",
        key_prefix="post-success",
        phase="pre-commit-success",
    )


def _builtin_pre_commit_steps(
    repo_root: Path, paths: list[Path]
) -> list[PreCommitStep]:
    return [
        PreCommitStep(
            "merge-integrity",
            "merge integrity",
            lambda: _run_merge_integrity_guard(repo_root),
        ),
        PreCommitStep(
            "plan-phase",
            "plan phase",
            lambda: _run_plan_phase_mutation_guard(repo_root),
        ),
        PreCommitStep("repo-shape", "repo shape", lambda: _run_shape_guards(repo_root)),
        PreCommitStep("staging", "staging", lambda: _run_staging_guard(repo_root)),
        PreCommitStep(
            "repo-docs", "repo docs", lambda: _run_repo_truth_doc_guard(repo_root)
        ),
        PreCommitStep(
            "formatters",
            "formatters",
            lambda: _run_python_format_guard(repo_root, paths),
        ),
        PreCommitStep(
            "local-paths",
            "local paths",
            lambda: _run_local_path_guard(repo_root, paths),
        ),
        PreCommitStep(
            "taste",
            "taste",
            lambda: _run_taste_guard(repo_root, paths),
        ),
        PreCommitStep(
            "serve-web-typecheck",
            "serve web typecheck",
            lambda: _run_serve_web_typecheck_guard(repo_root),
        ),
        PreCommitStep(
            "javascript-unused",
            "javascript unused",
            lambda: _run_javascript_unused_guard(repo_root),
        ),
        PreCommitStep(
            "python-typecheck",
            "python typecheck",
            lambda: _run_python_typecheck_guard(repo_root),
        ),
        PreCommitStep(
            "env-policy",
            "env policy",
            lambda: _run_env_policy_guard(repo_root, paths),
        ),
        PreCommitStep(
            "env-name-ledger",
            "env name ledger",
            lambda: _run_env_name_ledger_guard(repo_root),
        ),
        PreCommitStep(
            "file-shape",
            "file shape",
            lambda: _run_file_loc_guard(repo_root, paths),
        ),
        PreCommitStep(
            "complexity",
            "complexity",
            lambda: _run_complexity_guard(repo_root, paths),
        ),
        PreCommitStep(
            "magic-numbers",
            "magic numbers",
            lambda: _run_magic_numbers_guard(repo_root, paths),
        ),
        PreCommitStep(
            "markdown-links",
            "markdown links",
            lambda: _run_markdown_links_guard(repo_root),
        ),
        PreCommitStep(
            "reachability",
            "reachability",
            lambda: _run_reachability_guard(repo_root, paths),
        ),
        PreCommitStep(
            "symbol-reachability",
            "symbol reachability",
            lambda: _run_symbol_reachability_guard(repo_root, paths),
        ),
        PreCommitStep(
            "python-unused",
            "python unused",
            lambda: _run_python_unused_guard(repo_root),
        ),
        PreCommitStep(
            "assertion-free-tests",
            "assertion-free tests",
            lambda: _run_assertion_free_test_guard(repo_root),
        ),
        PreCommitStep(
            "private-internals",
            "private internals",
            lambda: _run_private_internal_coupling_guard(repo_root),
        ),
    ]


def _run_merge_integrity_guard(repo_root: Path) -> None:
    diagnostic = _merge_integrity_diagnostic(repo_root)
    if diagnostic is not None:
        raise SpiceError(diagnostic)


def _merge_integrity_diagnostic(repo_root: Path) -> str | None:
    merge_head_path = Path(
        _git_stdout(repo_root, "rev-parse", "--git-path", "MERGE_HEAD")
    )
    if not merge_head_path.is_absolute():
        merge_head_path = repo_root / merge_head_path
    if not merge_head_path.is_file():
        return None

    staged_tree = _git_stdout(repo_root, "write-tree")
    head_tree = _git_stdout(repo_root, "rev-parse", "HEAD^{tree}")
    if staged_tree != head_tree:
        return None

    expected = run_git_command(
        [
            "git",
            "merge-tree",
            "--write-tree",
            "-z",
            "--name-only",
            "HEAD",
            "MERGE_HEAD",
        ],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    expected_fields = expected.stdout.split("\0")
    expected_tree = expected_fields[0].strip() if expected_fields else ""
    if expected.returncode not in (0, 1) or not expected_tree:
        detail = "\n".join(
            part.strip() for part in (expected.stdout, expected.stderr) if part.strip()
        )
        message = (
            "cannot verify merge integrity: "
            "git merge-tree --write-tree HEAD MERGE_HEAD produced no tree"
        )
        if detail:
            message += f"\n{detail}"
        return message
    if expected_tree == head_tree:
        return None

    refused = (
        "empty merge refused: MERGE_HEAD exists, but the staged tree still "
        f"equals HEAD ({head_tree}) while git merge-tree computes "
        f"{expected_tree}. Committing now would discard integrated content.\n"
    )
    if expected.returncode == 0:
        return refused + (
            "Recover without removing MERGE_HEAD:\n"
            "  (\n"
            "    merge_tree=$(git merge-tree --write-tree HEAD MERGE_HEAD) "
            "|| exit 1\n"
            '    git read-tree --reset -u "$merge_tree" || exit 1\n'
            "    git diff --cached --check || exit 1\n"
            "    git status --short || exit 1\n"
            "  )\n"
            "Verify the staged merge content, then retry git commit with "
            "MERGE_HEAD intact."
        )
    # A conflicted merge-tree result is a multi-line report, not a tree to
    # read; installing its top tree would stage conflict markers as if
    # resolved. The index equals HEAD here, so no staged resolution exists
    # to lose: restart the merge and resolve it for real. The checkout line
    # names every conflicted path so only merge debris is reset.
    conflicted_paths = _merge_tree_conflicted_paths(expected.stdout)
    if not conflicted_paths:
        return refused + (
            "Cannot print a safe restart recipe because git merge-tree did not "
            "name the conflicted paths. Re-run the merge manually and resolve "
            "its conflicts before committing."
        )
    head_paths = _head_tree_paths(repo_root, conflicted_paths)
    if head_paths is None:
        return refused + (
            "Cannot print a safe restart recipe because the conflicted paths "
            "could not be classified against HEAD. Re-run the merge manually "
            "and resolve its conflicts before committing."
        )
    paths_text = " ".join(shlex.quote(path) for path in conflicted_paths)
    restore_head = ""
    if head_paths:
        head_paths_text = " ".join(shlex.quote(path) for path in head_paths)
        restore_head = (
            "    git --literal-pathspecs checkout HEAD -- "
            f"{head_paths_text} || exit 1\n"
        )
    return refused + (
        "The discarded merge is conflicted; restart it:\n"
        "  (\n"
        "    merge_head=$(git rev-parse MERGE_HEAD) || exit 1\n"
        "    git --literal-pathspecs clean -f -d -x -- "
        f"{paths_text} || exit 1\n"
        f"{restore_head}"
        "    git merge --abort || exit 1\n"
        '    git merge --no-ff "$merge_head"\n'
        "    merge_status=$?\n"
        "    git status --short || exit 1\n"
        '    if [ "$merge_status" -eq 0 ] || '
        '[ -f "$(git rev-parse --git-path MERGE_HEAD)" ]; then\n'
        "      exit 0\n"
        "    fi\n"
        '    exit "$merge_status"\n'
        "  )\n"
        "Resolve the conflicts the restarted merge reports, git add each "
        "resolution, then retry git commit with MERGE_HEAD intact."
    )


def _merge_tree_conflicted_paths(output: str) -> list[str]:
    """Conflicted paths from NUL-delimited `git merge-tree --name-only` output.

    The tree id is first, followed by one path per conflict and an empty field
    before the informational-message records."""
    paths: set[str] = set()
    for path in output.split("\0")[1:]:
        if not path:
            break
        paths.add(path)
    return sorted(paths)


def _head_tree_paths(repo_root: Path, paths: list[str]) -> list[str] | None:
    """Return conflict paths present in HEAD, preserving literal path identity."""
    result = run_git_command(
        [
            "git",
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--name-only",
            "HEAD",
            "--",
            *paths,
        ],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    if result.returncode != 0:
        return None
    present = set(result.stdout.split("\0"))
    return [path for path in paths if path in present]


def _git_stdout(repo_root: Path, *args: str) -> str:
    result = run_git_command(
        ["git", *args],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    return result.stdout.strip()


def _run_plan_phase_mutation_guard(repo_root: Path) -> None:
    if find_tool("task") is None:
        return
    from spice.tasks import claimstate

    cwd = Path.cwd()
    try:
        os.chdir(repo_root)
        claimstate.require_no_active_plan_phase_implementation("git commit")
    finally:
        os.chdir(cwd)


def _configured_builtin_steps(
    repo_root: Path, builtin_steps: list[PreCommitStep]
) -> list[PreCommitStep]:
    policy = effective_table(repo_root, "policy")
    raw_overrides = policy.get("pre_commit_builtins")
    if raw_overrides is None:
        return builtin_steps
    if not isinstance(raw_overrides, dict):
        raise SpiceError(
            "[policy] pre_commit_builtins must be a table of "
            "built-in pre-commit step overrides"
        )

    by_key = {step.key: step for step in builtin_steps}
    normalized = {
        _normalize_step_key(raw_key): raw_value
        for raw_key, raw_value in raw_overrides.items()
    }
    unknown = sorted(key for key in normalized if key not in by_key)
    if unknown:
        known = ", ".join(step.key for step in builtin_steps)
        listed = ", ".join(unknown)
        raise SpiceError(
            "[policy.pre_commit_builtins] unknown step(s): "
            f"{listed}; known steps: {known}"
        )
    overrides = enabled_registry_entries(normalized, "policy", "pre_commit_builtins")

    configured: list[PreCommitStep] = []
    for step in builtin_steps:
        if step.key not in overrides:
            continue
        replacement = overrides[step.key]
        configured_step = _configured_builtin_step(repo_root, step, replacement)
        if configured_step is not None:
            configured.append(configured_step)
    return configured


def _configured_builtin_step(
    repo_root: Path, step: PreCommitStep, raw: Any
) -> PreCommitStep | None:
    if raw is True:
        return step
    if isinstance(raw, str):
        command = _mounted_command_step(repo_root, raw)
        return _command_pre_commit_step(step.key, command)
    if not isinstance(raw, dict):
        raise SpiceError(
            f"[policy.pre_commit_builtins] {step.key!r} must be "
            "true, false, a mounted command name, or a replacement table"
        )
    if raw.get("enabled") is False:
        return None
    if any(name in raw for name in ("mount", "run", "argv")):
        command = _command_step_from_table(
            repo_root, raw, default_label=step.label, context=step.key
        )
        return _command_pre_commit_step(step.key, command)
    label = _label_from_table(raw, default=step.label, context=step.key)
    return PreCommitStep(step.key, label, step.action)


def _extension_pre_commit_steps(
    repo_root: Path, staged: list[Path]
) -> list[PreCommitStep]:
    return _configured_command_steps(
        repo_root,
        staged,
        config_key="pre_commit",
        key_prefix="extension",
        phase="pre-commit",
    )


def _configured_command_steps(
    repo_root: Path,
    staged: list[Path],
    *,
    config_key: str,
    key_prefix: str,
    phase: str,
) -> list[PreCommitStep]:
    raw_steps = effective_table(repo_root, "policy").get(config_key)
    if raw_steps is None:
        return []
    if not isinstance(raw_steps, list):
        raise SpiceError(f"[policy] {config_key} must be a list")
    agent_config = effective_agent_config(repo_root)
    steps: list[PreCommitStep] = []
    for index, raw in enumerate(raw_steps, start=1):
        context = f"{config_key}[{index}]"
        scope = ScopeSelector()
        if isinstance(raw, str):
            command = _mounted_command_step(repo_root, raw)
        elif isinstance(raw, dict):
            extra = sorted(set(raw) - COMMAND_STEP_KEYS)
            if extra:
                raise SpiceError(f"{context}: unsupported keys: {', '.join(extra)}")
            if raw.get("enabled") is False:
                continue
            command = _command_step_from_table(repo_root, raw, context=context)
            scope = PRE_COMMIT_STEP_SCOPES.parse(raw.get(SCOPES_KEY))
        else:
            raise SpiceError(
                f"[policy] {config_key} entries must be mounted command "
                "names or { label = ..., run = [...] } tables"
            )
        paths = _scoped_staged_paths(
            scope,
            staged,
            driver=agent_config[AGENT_DRIVER_KEY],
            model=agent_config[AGENT_MODEL_KEY],
            phase=phase,
        )
        if paths is None:
            continue
        command = CommandStep(
            label=command.label,
            argv=command.argv,
            repo_root=command.repo_root,
            staged_paths=paths,
            formatter=command.formatter,
            visible_prog=command.visible_prog,
        )
        key = f"{key_prefix}-{index}"
        steps.append(_command_pre_commit_step(key, command))
    return steps


def _command_pre_commit_step(key: str, command: CommandStep) -> PreCommitStep:
    return PreCommitStep(
        key,
        command.label,
        lambda command=command: _run_policy_command_step(command),
    )


def _mounted_command_step(repo_root: Path, name: str) -> CommandStep:
    label = name.strip()
    if not label:
        raise SpiceError("[policy] mounted pre-commit command is empty")
    path = mount_command_path(label)
    argv = mounted_commands(repo_root).get(path)
    if argv is None:
        raise SpiceError(
            f"[policy] pre-commit command {label!r} is not declared in [commands]"
        )
    return CommandStep(
        label=label,
        argv=argv,
        repo_root=repo_root,
        visible_prog="spice " + " ".join(path),
    )


def _command_step_from_table(
    repo_root: Path,
    raw: dict[str, Any],
    *,
    context: str,
    default_label: str | None = None,
) -> CommandStep:
    label = _label_from_table(raw, default=default_label, context=context)
    mount = raw.get("mount")
    if mount is not None:
        if not isinstance(mount, str):
            raise SpiceError(f"{context}: mount must be a mounted command name")
        command = _mounted_command_step(repo_root, mount)
        return CommandStep(
            label=label,
            argv=command.argv,
            repo_root=repo_root,
            formatter=_formatter_from_table(raw, context=context),
            visible_prog=command.visible_prog,
        )

    raw_argv = raw.get("run", raw.get("argv"))
    return CommandStep(
        label=label,
        argv=_command_argv(raw_argv, context=context),
        repo_root=repo_root,
        formatter=_formatter_from_table(raw, context=context),
    )


def _label_from_table(
    raw: dict[str, Any], *, default: str | None = None, context: str
) -> str:
    label = raw.get("label", default)
    if not isinstance(label, str) or not label.strip():
        raise SpiceError(f"{context}: label must be a non-empty string")
    return label.strip()


def _command_argv(raw: Any, *, context: str) -> tuple[str, ...]:
    if isinstance(raw, str):
        argv = tuple(shlex.split(raw))
    elif isinstance(raw, list) and all(isinstance(item, str) for item in raw):
        argv = tuple(raw)
    else:
        raise SpiceError(f"{context}: run must be a command string or argv list")
    if not argv:
        raise SpiceError(f"{context}: run is empty")
    return argv


def _formatter_from_table(raw: dict[str, Any], *, context: str) -> bool:
    formatter = raw.get("formatter", False)
    if isinstance(formatter, bool):
        return formatter
    raise SpiceError(f"{context}: formatter must be true or false")


def _run_policy_command_step(command: CommandStep) -> None:
    env = os.environ.copy()  # env-policy: allow
    env[STAGED_PATHS_ENV] = "\n".join(path.as_posix() for path in command.staged_paths)
    if command.visible_prog is not None:
        # A mounted command run as a gate step is still that spice mount, so it
        # carries the same mount environment `spice <name>` exports.
        env[MOUNTED_COMMAND_ENV] = "1"
        env[VISIBLE_PROG_ENV] = command.visible_prog
    result = run_tool_command(
        list(command.argv),
        policy="extension",
        operation=command.label,
        capture_output=True,
        env=env,
        text=True,
        cwd=command.repo_root,
        check=False,
    )
    if result.returncode == 0:
        if command.formatter:
            _restage_command_paths(command)
        return
    output = "\n".join(
        part for part in (result.stdout.strip(), result.stderr.strip()) if part
    )
    message = f"{shlex.join(command.argv)} exited {result.returncode}"
    if output:
        message += ":\n" + output
    raise SpiceError(message)


def _restage_command_paths(command: CommandStep) -> None:
    if not command.staged_paths:
        return
    run_git_command(
        ["git", "add", "--", *(path.as_posix() for path in command.staged_paths)],
        capture_output=True,
        cwd=command.repo_root,
        check=True,
    )


def _normalize_step_key(raw: Any) -> str:
    return str(raw).strip().lower().replace("_", "-").replace(" ", "-")


def _scoped_staged_paths(
    scope: ScopeSelector,
    staged: list[Path],
    *,
    driver: str,
    model: str,
    phase: str,
) -> tuple[Path, ...] | None:
    entry_scope = ScopeSelector(
        drivers=scope.drivers,
        models=scope.models,
        phases=scope.phases,
    )
    if not entry_scope.matches(ScopeContext(driver=driver, model=model, phase=phase)):
        return None
    if not scope.paths:
        return tuple(staged)
    paths = tuple(
        path
        for path in staged
        if scope.matches(
            ScopeContext(
                path=path,
                driver=driver,
                model=model,
                phase=phase,
            )
        )
    )
    return paths or None


def _run_shape_guards(repo_root: Path) -> None:
    errors = [
        error
        for error in (
            shape.namespace_policy_error(repo_root),
            shape.path_shape_error(repo_root),
            shape.name_cluster_error(repo_root),
        )
        if error
    ]
    if errors:
        raise SpiceError("\n".join(errors))


def _run_staging_guard(repo_root: Path) -> None:
    partial = partially_staged_paths(repo_root)
    if partial:
        listed = "\n".join(f"  {path.as_posix()}" for path in partial)
        raise SpiceError(
            "partially staged files; stage the whole file or stash the rest:\n" + listed
        )


def _run_repo_truth_doc_guard(repo_root: Path) -> None:
    """Doctrine docs ride in every agent's context; cap them hard."""
    resolved = resolve_policy(repo_root)
    findings = repo_truth_doc_findings(
        repo_root,
        persist=True,
        flex_actor=resolved.flex_actor_id,
    )
    _raise_or_inform_flex_findings(
        findings,
        render=render_repo_truth_doc_guard_error,
    )


def _run_python_format_guard(repo_root: Path, paths: list[Path]) -> None:
    """Format and safe-fix staged Python in place, restage, then lint.

    The gate does what it can do itself instead of bouncing the commit back:
    the fully-staged rule has already passed, so rewriting and restaging the
    same paths loses nothing, and the agent spends its crank on real findings.
    """
    python_paths = [path for path in paths if path.suffix == ".py"]
    if not python_paths:
        return
    ruff = find_tool("ruff")
    if not ruff:
        raise SpiceError(
            "ruff is required to gate staged Python; it installs with spice, "
            "so the installation is broken or incomplete"
        )
    targets = [str(path) for path in python_paths if (repo_root / path).exists()]
    if not targets:
        return
    run_tool_command(
        [ruff, "format", *targets],
        policy="hook",
        operation="format staged Python",
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    run_tool_command(
        [ruff, "check", "--fix", *targets],
        policy="hook",
        operation="fix staged Python lint",
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    run_git_command(
        ["git", "add", "--", *targets], capture_output=True, cwd=repo_root, check=True
    )
    lint = run_tool_command(
        [ruff, "check", *targets],
        policy="hook",
        operation="check staged Python lint",
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    if lint.returncode != 0:
        raise SpiceError("ruff check failed:\n" + (lint.stdout or lint.stderr).strip())


def _run_serve_web_typecheck_guard(repo_root: Path) -> None:
    from spice.serve.typecheck import run_serve_web_typecheck

    run_serve_web_typecheck(repo_root)


def _run_python_typecheck_guard(repo_root: Path) -> None:
    from spice.studies.typecheck import run_python_typecheck

    run_python_typecheck(repo_root)


def _run_env_policy_guard(repo_root: Path, paths: list[Path]) -> None:
    resolved = resolve_policy(repo_root)
    findings = envpolicy.scan_env_policy(
        paths, root=repo_root, suffixes=resolved.languages.env
    )
    if findings:
        raise SpiceError(envpolicy.render_env_policy_board(findings))


def _run_env_name_ledger_guard(repo_root: Path) -> None:
    from spice.studies.walk import tracked_paths

    resolved = resolve_policy(repo_root)
    findings = envpolicy.scan_env_name_ledger(
        tracked_paths(repo_root), root=repo_root, suffixes=resolved.languages.env
    )
    if findings:
        raise SpiceError(envpolicy.render_env_name_ledger_board(findings))


def _run_local_path_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = localpaths.scan_local_path_literals(paths, root=repo_root)
    if findings:
        raise SpiceError(localpaths.render_local_path_board(findings))


def _run_taste_guard(repo_root: Path, paths: list[Path]) -> None:
    resolved = resolve_policy(repo_root)
    findings = taste.scan_taste(paths, root=repo_root, words=dict(resolved.taste.words))
    if findings:
        raise SpiceError(taste.render_taste_board(findings))


def _run_file_loc_guard(repo_root: Path, paths: list[Path]) -> None:
    resolved = resolve_policy(repo_root)
    bounds = resolved.file_shape
    repo_doc_paths = set(repo_truth_doc_candidate_paths(repo_root, resolved))
    generated_patterns = (
        *resolved.file_shape_paths.generated_patterns,
        *shape.generated_path_patterns(repo_root),
    )
    findings = fileloc.scan_staged_loc_violations(
        paths,
        root=repo_root,
        limit=bounds.line_limit,
        flex_limit_value=bounds.line_flex_limit,
        byte_limit=bounds.byte_limit,
        byte_flex_limit_value=bounds.byte_flex_limit,
        bounds_for_path=resolved.jittered_file_shape_for_path,
        source_suffixes=resolved.file_shape_paths.source_suffixes,
        generated_patterns=generated_patterns,
        repo_doc_paths=repo_doc_paths,
        lockfile_suffixes=resolved.lockfiles.suffixes,
        lockfile_names=resolved.lockfiles.names,
        persist=True,
        flex_actor=resolved.flex_actor_id,
    )
    _raise_or_inform_flex_findings(
        findings,
        render=lambda subset: fileloc.render_loc_board(
            subset,
            limit=bounds.line_limit,
            flex_limit_value=bounds.line_flex_limit,
            byte_limit=bounds.byte_limit,
            byte_flex_limit_value=bounds.byte_flex_limit,
        ),
    )


class _FlexClaimFinding(Protocol):
    @property
    def flex_slice_claim(self) -> FlexSliceClaim | None: ...


_FlexClaimFindingT = TypeVar("_FlexClaimFindingT", bound=_FlexClaimFinding)


def _raise_or_inform_flex_findings(
    findings: list[_FlexClaimFindingT],
    *,
    render: Callable[[list[_FlexClaimFindingT]], str],
) -> None:
    """Fail only on findings this worktree must fix; inform on peer-held ones.

    A finding annotated with a peer-held flex-slice claim means another worktree
    already holds the split for that path. Per the flex-slice contract that is
    informational, not a gate failure: surface the redirect and let this commit
    through, so the first worktree to land the fix clears the breach for all.
    """
    if not findings:
        return
    blocking = [finding for finding in findings if finding.flex_slice_claim is None]
    redirects = [
        finding for finding in findings if finding.flex_slice_claim is not None
    ]
    if redirects:
        print(render(redirects))
    if blocking:
        raise SpiceError(render(blocking))


def _run_complexity_guard(repo_root: Path, paths: list[Path]) -> None:
    resolved = resolve_policy(repo_root)
    bounds = resolved.complexity
    findings = complexity.scan_staged_complexity_violations(
        paths,
        root=repo_root,
        max_ccn=bounds.max_ccn,
        max_length=bounds.max_length,
        ccn_flex_limit_value=bounds.ccn_flex_limit,
        length_flex_limit_value=bounds.length_flex_limit,
        bounds_for_path=resolved.jittered_complexity_for_path,
        suffixes=resolved.languages.complexity,
        persist=True,
        flex_actor=resolved.flex_actor_id,
    )
    _raise_or_inform_flex_findings(
        findings,
        render=lambda subset: complexity.render_complexity_board(
            subset,
            max_ccn=bounds.max_ccn,
            max_length=bounds.max_length,
        ),
    )


def _run_magic_numbers_guard(repo_root: Path, paths: list[Path]) -> None:
    resolved = resolve_policy(repo_root)
    findings = magicnums.detect_magic_regressions(
        paths,
        root=repo_root,
        baseline_ref=resolved.magic.baseline_ref,
        examine_threshold=resolved.magic.examine_threshold,
        examine_threshold_for_path=resolved.magic_examine_threshold_for_path,
        suffixes=resolved.languages.magic,
        c_grammar_suffixes=resolved.languages.c_grammar,
    )
    if findings:
        raise SpiceError(
            magicnums.render_magic_board(
                findings, baseline_ref=resolved.magic.baseline_ref
            )
        )


def _run_javascript_unused_guard(repo_root: Path) -> None:
    findings = javascriptunused.scan_javascript_unused_symbols(
        tracked_paths(repo_root),
        root=repo_root,
        declaration_exemptions=JAVASCRIPT_UNUSED_DECLARATION_EXEMPTIONS,
    )
    if findings:
        raise SpiceError(
            javascriptunused.render_javascript_unused_board(findings)
            + "\njavascript-unused: candidate-unused and test-only declarations "
            "are actionable; wire them into production, move them into tests, or "
            "name an exact (path, symbol) exemption with a reason"
        )


def _run_markdown_links_guard(repo_root: Path) -> None:
    findings = links.markdown_link_case_findings(repo_root)
    if findings:
        raise SpiceError(links.render_markdown_link_case_board(findings))


def _run_reachability_guard(repo_root: Path, paths: list[Path] | None = None) -> None:
    debt_limit = resolve_policy(repo_root).debt.reachability_test_only
    findings = reachability.scan_reachability(repo_root, staged_paths=paths)
    count = len(findings)
    if count > debt_limit:
        board = "\n".join(reachability.render_reachability_board(findings))
        raise SpiceError(
            f"{board}\n"
            f"reachability: {count} test-only finding(s) exceed "
            "[policy.debt] "
            f"reachability_test_only={debt_limit}; 0 means clean, non-zero is "
            "explicit drainable cleanup debt - findings are not reachable "
            "from production roots, so wire each in or delete-both "
            "(`spice study reachability --create-tasks` files decisions)"
        )


def _run_symbol_reachability_guard(
    repo_root: Path, paths: list[Path] | None = None
) -> None:
    findings = reachability.scan_symbol_reachability(repo_root, staged_paths=paths)
    if findings:
        board = "\n".join(reachability.render_symbol_reachability_board(findings))
        raise SpiceError(
            f"{board}\n"
            "symbol-reachability: zero test-only symbols are allowed; "
            "wire in or delete-both"
        )


def _run_python_unused_guard(repo_root: Path) -> None:
    findings = pythonunused.scan_python_unused_symbols(repo_root)
    if findings:
        board = pythonunused.render_python_unused_board(findings)
        raise SpiceError(
            f"{board}\n"
            "python-unused: zero candidate-unused or test-only top-level symbols "
            "are allowed; delete dead definitions, wire production references, "
            "or record an exact dynamic-dispatch exemption"
        )


def _run_assertion_free_test_guard(repo_root: Path) -> None:
    debt_limit = resolve_policy(repo_root).debt.assertion_free_tests
    findings = testquality.scan_assertion_free_tests(
        testquality.test_paths(repo_root), root=repo_root
    )
    count = len(findings)
    if count > debt_limit:
        board = testquality.render_assertion_free_board(findings)
        raise SpiceError(
            f"{board}\n"
            f"assertion-free-tests: {count} test(s) exceed "
            "[policy.debt] "
            f"assertion_free_tests={debt_limit}; 0 means clean, non-zero is "
            "explicit drainable cleanup debt - add assertions or lower "
            "configured debt after cleanup"
        )


def _run_private_internal_coupling_guard(repo_root: Path) -> None:
    """Fail on any test/production-internal coupling that is not named in the
    built-in or tracked allowlist. Allowlists are named entries, never tolerated
    counts; a coupling not listed must be replaced with a public seam.
    """
    findings = testquality.scan_private_internal_coupling(
        testquality.test_paths(repo_root), root=repo_root
    )
    offenders, stale = testquality.unmanaged_private_internal_couplings(
        findings,
        repo_root=repo_root,
        built_in_couplings=LEGITIMATE_INTERNAL_COUPLINGS,
    )
    if offenders or stale:
        details: list[str] = []
        if offenders:
            details.append(testquality.render_private_internal_board(offenders))
            details.append(
                f"private-internals: {len(offenders)} coupling(s) are not "
                "allowlisted; add a public seam and switch the test to it, or "
                "— only if the test genuinely must observe an internal — add a "
                "justified entry to [policy].internal_couplings"
            )
        if stale:
            details.append(testquality.render_stale_internal_couplings(stale))
        raise SpiceError("\n".join(details))


# Quality gates a task can bind its completion to. A task tagged ``gate:<key>``
# cannot be marked done while the matching gate is not clean — the metric is
# read live, never asserted in prose. Keys are stable; the guards are the same
# ones the pre-commit gate runs.
QUALITY_GATE_GUARDS: dict[str, Callable[[Path], None]] = {
    "coupling": _run_private_internal_coupling_guard,
    "reachability": _run_reachability_guard,
    "symbol-reachability": _run_symbol_reachability_guard,
    "python-unused": _run_python_unused_guard,
    "assertion-free": _run_assertion_free_test_guard,
}

GATE_TAG_PREFIX = "gate:"


def quality_gate_failure(repo_root: Path, key: str) -> str | None:
    """Run one named quality gate; return its failure text, or None if clean."""
    guard = QUALITY_GATE_GUARDS.get(key)
    if guard is None:
        known = ", ".join(sorted(QUALITY_GATE_GUARDS))
        raise SpiceError(f"unknown quality gate {key!r}; known gates: {known}")
    try:
        guard(repo_root)
    except SpiceError as exc:
        return str(exc)
    return None


def quality_gate_failures_for_tags(repo_root: Path, tags: list[str]) -> list[str]:
    """Return a failure block per ``gate:<key>`` tag whose gate is not clean."""
    failures: list[str] = []
    for tag in tags:
        if not tag.startswith(GATE_TAG_PREFIX):
            continue
        key = tag[len(GATE_TAG_PREFIX) :]
        message = quality_gate_failure(repo_root, key)
        if message:
            failures.append(f"[{tag}]\n{message}")
    return failures
