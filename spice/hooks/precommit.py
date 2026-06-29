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
7. env policy — undeclared environment literals (and, when
   `env_access_gate` is on, undeclared env-access sites);
8. env name ledger — exact manifest accounting for literal env names;
9. shape pressure — file LOC/bytes, routine complexity, magic-number
   regressions, all against staged paths with flex + sticky semantics.

A fully passing gate prunes sticky state that no longer measures over the
base limit — the gate forgives exactly when the code earns it.
"""

from __future__ import annotations

import os
import shlex
import subprocess
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import Any, Callable

from spice.cli.mounts import (
    MOUNTED_COMMAND_ENV,
    VISIBLE_PROG_ENV,
    mount_command_path,
    mounted_commands,
)
from spice.errors import SpiceError
from spice.paths import find_tool
from spice.policy import (
    ASSERTION_FREE_TEST_LIMIT,
    LEGITIMATE_INTERNAL_COUPLINGS,
    REACHABILITY_TEST_ONLY_LIMIT,
    REPO_TRUTH_DOC_LIMIT,
    REPO_TRUTH_DOCS,
)
from spice.policyconfig import resolve_policy
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
    # Set for steps that run a mounted command, so the gate step accurately
    # presents as that spice mount (mount env) the same as `spice <name>` does.
    visible_prog: str | None = None


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
        raise SpiceError("[tool.spice.policy] mounted pre-commit command is empty")
    path = mount_command_path(label)
    argv = mounted_commands(repo_root).get(path)
    if argv is None:
        raise SpiceError(
            f"[tool.spice.policy] pre-commit command {label!r} is not declared "
            "in [tool.spice.commands]"
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


def repo_truth_doc_violations(repo_root: Path) -> list[str]:
    """Return one ``name: count characters (cap N)`` line per over-cap doc.

    Public seam: doctrine docs ride in every agent's context, so the cap is a
    real product rule worth asserting on directly; the guard below is the thin
    raising wrapper.
    """
    over: list[str] = []
    for name in repo_truth_docs(repo_root):
        path = repo_root / name
        if not path.is_file():
            continue
        count = len(path.read_text(encoding="utf-8", errors="replace"))
        if count > REPO_TRUTH_DOC_LIMIT:
            over.append(f"  {name}: {count} characters (cap {REPO_TRUTH_DOC_LIMIT})")
    return over


def _run_repo_truth_doc_guard(repo_root: Path) -> None:
    """Doctrine docs ride in every agent's context; cap them hard."""
    over = repo_truth_doc_violations(repo_root)
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


def _run_env_name_ledger_guard(repo_root: Path) -> None:
    from spice.studies.walk import tracked_paths

    findings = envpolicy.scan_env_name_ledger(tracked_paths(repo_root), root=repo_root)
    if findings:
        raise SpiceError(envpolicy.render_env_name_ledger_board(findings))


def _run_local_path_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = localpaths.scan_local_path_literals(paths, root=repo_root)
    if findings:
        raise SpiceError(localpaths.render_local_path_board(findings))


def _run_file_loc_guard(repo_root: Path, paths: list[Path]) -> None:
    bounds = resolve_policy(repo_root).file_shape
    findings = fileloc.scan_staged_loc_violations(
        paths,
        root=repo_root,
        limit=bounds.line_limit,
        flex_limit_value=bounds.line_flex_limit,
        byte_limit=bounds.byte_limit,
        byte_flex_limit_value=bounds.byte_flex_limit,
        persist=True,
    )
    if findings:
        raise SpiceError(
            fileloc.render_loc_board(
                findings,
                limit=bounds.line_limit,
                flex_limit_value=bounds.line_flex_limit,
                byte_limit=bounds.byte_limit,
                byte_flex_limit_value=bounds.byte_flex_limit,
            )
        )


def _run_complexity_guard(repo_root: Path, paths: list[Path]) -> None:
    bounds = resolve_policy(repo_root).complexity
    findings = complexity.scan_staged_complexity_violations(
        paths,
        root=repo_root,
        max_ccn=bounds.max_ccn,
        max_length=bounds.max_length,
        ccn_flex_limit_value=bounds.ccn_flex_limit,
        length_flex_limit_value=bounds.length_flex_limit,
        persist=True,
    )
    if findings:
        raise SpiceError(
            complexity.render_complexity_board(
                findings,
                max_ccn=bounds.max_ccn,
                max_length=bounds.max_length,
            )
        )


def _run_magic_numbers_guard(repo_root: Path, paths: list[Path]) -> None:
    findings = magicnums.detect_magic_regressions(paths, root=repo_root)
    if findings:
        raise SpiceError(magicnums.render_magic_board(findings))


def _run_reachability_guard(repo_root: Path, paths: list[Path] | None = None) -> None:
    findings = reachability.scan_reachability(repo_root, staged_paths=paths)
    count = len(findings)
    if count > REACHABILITY_TEST_ONLY_LIMIT:
        board = "\n".join(reachability.render_reachability_board(findings))
        raise SpiceError(
            f"{board}\n"
            f"reachability: {count} test-only finding(s) not reachable from"
            " production roots; zero are allowed - wire each in or delete-both"
            " (`spice study reachability --create-tasks` files decisions)"
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


def _coupling_key(
    finding: testquality.PrivateInternalCouplingFinding,
) -> tuple[str, str, str]:
    return testquality.private_internal_coupling_key(finding)


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
                "justified entry to [tool.spice.policy].internal_couplings"
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


def clear_successful_sticky_state(repo_root: Path) -> None:
    bounds = resolve_policy(repo_root)
    file_shape = bounds.file_shape
    routine = bounds.complexity
    fileloc.clear_file_loc_sticky_state(
        root=repo_root,
        limit=file_shape.line_limit,
        byte_limit=file_shape.byte_limit,
    )
    complexity.clear_complexity_sticky_state(
        root=repo_root,
        max_ccn=routine.max_ccn,
        max_length=routine.max_length,
    )
