"""The pre-commit gate: the constitution, executed.

Steps run in order, collecting every failure before raising, so one commit
attempt reports the whole picture:

1. repo shape — namespace packages, path shape, no generic split names;
2. staging — partially staged files are rejected (the fully-staged rule);
3. formatters — staged Python must satisfy `ruff format --check` and
   `ruff check`;
4. local paths — no committed absolute macOS user path literals;
5. serve web typecheck — static browser JavaScript must pass TypeScript
   `checkJs`;
6. python typecheck — the project's own package roots must pass `pyright`;
7. env policy — undeclared environment literals;
8. shape pressure — file LOC/bytes, routine complexity, magic-number
   regressions, all against staged paths with flex + sticky semantics.

A fully passing gate prunes sticky state that no longer measures over the
base limit — the gate forgives exactly when the code earns it.
"""

from __future__ import annotations

import os
import subprocess
import shlex
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Any, Callable

from spice.cli.mounts import mount_command_path, mounted_commands
from spice.errors import SpiceError
from spice.paths import find_tool
from spice.policy import (
    ASSERTION_FREE_TEST_LIMIT,
    PRIVATE_INTERNAL_COUPLING_LIMIT,
    REACHABILITY_TEST_ONLY_LIMIT,
    REPO_TRUTH_DOC_LIMIT,
    REPO_TRUTH_DOCS,
)
from spice.repocfg import policy_table, string_list
from spice.studies import (
    complexity,
    envpolicy,
    fileloc,
    localpaths,
    magicnums,
    reachability,
    shape,
    testquality,
)
from spice.studies.walk import partially_staged_paths, staged_paths

STAGED_PATHS_ENV = "SPICE_STAGED_PATHS"  # env-policy: allow


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


def handle_pre_commit(repo_root: Path) -> int:
    failures: list[PreCommitFailure] = []
    paths = staged_paths(repo_root)
    staging_verified = False
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
    clear_successful_sticky_state(repo_root)
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
    )


def _builtin_pre_commit_steps(
    repo_root: Path, paths: list[Path]
) -> list[PreCommitStep]:
    return [
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
            "serve-web-typecheck",
            "serve web typecheck",
            lambda: _run_serve_web_typecheck_guard(repo_root),
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
            "reachability",
            "reachability",
            lambda: _run_reachability_guard(repo_root),
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


def _configured_builtin_steps(
    repo_root: Path, builtin_steps: list[PreCommitStep]
) -> list[PreCommitStep]:
    policy = policy_table(repo_root)
    raw_overrides = policy.get("pre_commit_builtins")
    if raw_overrides is None:
        return builtin_steps
    if not isinstance(raw_overrides, dict):
        raise SpiceError(
            "[tool.spice.policy] pre_commit_builtins must be a table of "
            "built-in pre-commit step overrides"
        )

    by_key = {step.key: step for step in builtin_steps}
    overrides = {
        _normalize_step_key(raw_key): raw_value
        for raw_key, raw_value in raw_overrides.items()
    }
    unknown = sorted(key for key in overrides if key not in by_key)
    if unknown:
        known = ", ".join(step.key for step in builtin_steps)
        listed = ", ".join(unknown)
        raise SpiceError(
            "[tool.spice.policy.pre_commit_builtins] unknown step(s): "
            f"{listed}; known steps: {known}"
        )

    configured: list[PreCommitStep] = []
    for step in builtin_steps:
        replacement = overrides.get(step.key)
        if replacement is None:
            configured.append(step)
            continue
        configured_step = _configured_builtin_step(repo_root, step, replacement)
        if configured_step is not None:
            configured.append(configured_step)
    return configured


def _configured_builtin_step(
    repo_root: Path, step: PreCommitStep, raw: Any
) -> PreCommitStep | None:
    if raw is True:
        return step
    if raw is False:
        return None
    if isinstance(raw, str):
        command = _mounted_command_step(repo_root, raw)
        return _command_pre_commit_step(step.key, command)
    if not isinstance(raw, dict):
        raise SpiceError(
            f"[tool.spice.policy.pre_commit_builtins] {step.key!r} must be "
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
    )


def _configured_command_steps(
    repo_root: Path,
    staged: list[Path],
    *,
    config_key: str,
    key_prefix: str,
) -> list[PreCommitStep]:
    raw_steps = policy_table(repo_root).get(config_key)
    if raw_steps is None:
        return []
    if not isinstance(raw_steps, list):
        raise SpiceError(f"[tool.spice.policy] {config_key} must be a list")
    steps: list[PreCommitStep] = []
    for index, raw in enumerate(raw_steps, start=1):
        context = f"{config_key}[{index}]"
        when: tuple[str, ...] = ()
        if isinstance(raw, str):
            command = _mounted_command_step(repo_root, raw)
        elif isinstance(raw, dict):
            command = _command_step_from_table(repo_root, raw, context=context)
            when = _when_patterns_from_table(raw, context=context)
        else:
            raise SpiceError(
                f"[tool.spice.policy] {config_key} entries must be mounted command "
                "names or { label = ..., run = [...] } tables"
            )
        paths = _matching_staged_paths(staged, when) if when else tuple(staged)
        if when and not paths:
            continue
        command = CommandStep(
            label=command.label,
            argv=command.argv,
            repo_root=command.repo_root,
            staged_paths=paths,
            formatter=command.formatter,
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
        raise SpiceError("[tool.spice.policy] mounted pre-commit command is empty")
    argv = mounted_commands(repo_root).get(mount_command_path(label))
    if argv is None:
        raise SpiceError(
            f"[tool.spice.policy] pre-commit command {label!r} is not declared "
            "in [tool.spice.commands]"
        )
    return CommandStep(label=label, argv=argv, repo_root=repo_root)


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
    env = os.environ.copy()
    env[STAGED_PATHS_ENV] = "\n".join(path.as_posix() for path in command.staged_paths)
    result = subprocess.run(
        list(command.argv),
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
    subprocess.run(
        ["git", "add", "--", *(path.as_posix() for path in command.staged_paths)],
        capture_output=True,
        cwd=command.repo_root,
        check=True,
    )


def _normalize_step_key(raw: Any) -> str:
    return str(raw).strip().lower().replace("_", "-").replace(" ", "-")


def _when_patterns_from_table(raw: dict[str, Any], *, context: str) -> tuple[str, ...]:
    if "when" not in raw:
        return ()
    when = raw["when"]
    if not isinstance(when, list):
        raise SpiceError(f"{context}: when must be a non-empty glob list")
    patterns = tuple(
        item.strip() for item in when if isinstance(item, str) and item.strip()
    )
    if len(patterns) != len(when) or not patterns:
        raise SpiceError(f"{context}: when must be a non-empty glob list")
    return patterns


def _matching_staged_paths(
    staged: list[Path], patterns: tuple[str, ...]
) -> tuple[Path, ...]:
    return tuple(
        path
        for path in staged
        if any(_path_matches_when(path, pattern) for pattern in patterns)
    )


def _path_matches_when(path: Path, pattern: str) -> bool:
    normalized_path = path.as_posix().strip().removeprefix("./")
    normalized_pattern = pattern.strip().replace("\\", "/").removeprefix("./")
    return fnmatchcase(normalized_path, normalized_pattern)


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


def repo_truth_docs(repo_root: Path) -> list[str]:
    declared = string_list(policy_table(repo_root).get("repo_truth_docs"))
    return declared or list(REPO_TRUTH_DOCS)


def _run_repo_truth_doc_guard(repo_root: Path) -> None:
    """Doctrine docs ride in every agent's context; cap them hard."""
    over: list[str] = []
    for name in repo_truth_docs(repo_root):
        path = repo_root / name
        if not path.is_file():
            continue
        count = len(path.read_text(encoding="utf-8", errors="replace"))
        if count > REPO_TRUTH_DOC_LIMIT:
            over.append(f"  {name}: {count} characters (cap {REPO_TRUTH_DOC_LIMIT})")
    if over:
        raise SpiceError(
            "repo-truth docs exceed the character cap; tighten the doctrine:\n"
            + "\n".join(over)
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
    subprocess.run(
        [ruff, "format", *targets],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=True,
    )
    subprocess.run(
        [ruff, "check", "--fix", *targets],
        capture_output=True,
        text=True,
        cwd=repo_root,
        check=False,
    )
    subprocess.run(
        ["git", "add", "--", *targets], capture_output=True, cwd=repo_root, check=True
    )
    lint = subprocess.run(
        [ruff, "check", *targets],
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
    findings = envpolicy.scan_env_policy(paths, root=repo_root)
    if findings:
        raise SpiceError(envpolicy.render_env_policy_board(findings))


def _run_local_path_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = localpaths.scan_local_path_literals(paths, root=repo_root)
    if findings:
        raise SpiceError(localpaths.render_local_path_board(findings))


def _run_file_loc_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = fileloc.scan_staged_loc_violations(paths, root=repo_root)
    if findings:
        raise SpiceError(fileloc.render_loc_board(findings))


def _run_complexity_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = complexity.scan_staged_complexity_violations(paths, root=repo_root)
    if findings:
        raise SpiceError(complexity.render_complexity_board(findings))


def _run_magic_numbers_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = magicnums.detect_magic_regressions(paths, root=repo_root)
    if findings:
        raise SpiceError(magicnums.render_magic_board(findings))


def _run_reachability_guard(repo_root: Path) -> None:
    findings = reachability.scan_reachability(repo_root)
    count = len(findings)
    if count > REACHABILITY_TEST_ONLY_LIMIT:
        board = "\n".join(reachability.render_reachability_board(findings))
        raise SpiceError(
            f"{board}\n"
            f"reachability: {count} test-only module(s) exceed"
            f" REACHABILITY_TEST_ONLY_LIMIT={REACHABILITY_TEST_ONLY_LIMIT};"
            " wire in or delete-both, then lower the constant"
        )


def _run_assertion_free_test_guard(repo_root: Path) -> None:
    findings = testquality.scan_assertion_free_tests(
        testquality.test_paths(repo_root), root=repo_root
    )
    count = len(findings)
    if count > ASSERTION_FREE_TEST_LIMIT:
        board = testquality.render_assertion_free_board(findings)
        raise SpiceError(
            f"{board}\n"
            f"assertion-free-tests: {count} test(s) exceed"
            f" ASSERTION_FREE_TEST_LIMIT={ASSERTION_FREE_TEST_LIMIT};"
            " add assertions or lower the constant after cleanup"
        )


def _run_private_internal_coupling_guard(repo_root: Path) -> None:
    findings = testquality.scan_private_internal_coupling(
        testquality.test_paths(repo_root), root=repo_root
    )
    count = len(findings)
    if count > PRIVATE_INTERNAL_COUPLING_LIMIT:
        board = testquality.render_private_internal_board(findings)
        raise SpiceError(
            f"{board}\n"
            f"private-internals: {count} coupling(s) exceed"
            f" PRIVATE_INTERNAL_COUPLING_LIMIT={PRIVATE_INTERNAL_COUPLING_LIMIT};"
            " use public seams or lower the constant after cleanup"
        )


def clear_successful_sticky_state(repo_root: Path) -> None:
    fileloc.clear_file_loc_sticky_state(root=repo_root)
    complexity.clear_complexity_sticky_state(root=repo_root)
