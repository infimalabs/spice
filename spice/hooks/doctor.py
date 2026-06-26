"""`spice dev doctor` — whole-repo health checks with truthful fixes."""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from spice.agent.driver import DRIVER
from spice.agent.lifecycle import packaged_skill_path
from spice.config import (
    configured_judge_bin,
    configured_say_backend,
    configured_say_command,
    git_worktree_config_get,
)
from spice.errors import SpiceError
from spice.hooks.install import HOOK_ARGS, hook_shim_content, hooks_dir
from spice.paths import (
    find_tool,
    git_common_dir,
    runtime_spice_source,
    state_dir,
    worktree_spice_source,
)
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    FILE_BYTE_LIMIT,
    FILE_LOC_LIMIT,
    MAGIC_BASELINE_REF,
)
from spice.studies import complexity, envpolicy, fileloc, magicnums, shape
from spice.studies.walk import tracked_paths

DoctorStatus = str


@dataclass(frozen=True)
class DoctorCheck:
    name: str
    status: DoctorStatus
    detail: str
    command: str


@dataclass(frozen=True)
class DoctorReport:
    repo_root: Path
    checks: list[DoctorCheck]
    fixes: list[str]

    @property
    def failed(self) -> bool:
        return any(check.status == "fail" for check in self.checks)

    def render(self) -> str:
        lines = ["spice doctor", f"  repo_root={self.repo_root}"]
        lines.append(f"  state_dir={state_dir(self.repo_root)}")
        for fix in self.fixes:
            lines.append(f"  fixed {fix}")
        for check in self.checks:
            lines.append(
                f"  {check.status.upper():4} {check.name}: {check.detail} "
                f"(cmd: {check.command})"
            )
        return "\n".join(lines)


def run_doctor(repo_root: Path, *, fix: bool = False) -> DoctorReport:
    fixes: list[str] = []
    if fix:
        fixes.extend(_apply_safe_fixes(repo_root))
    paths = tracked_paths(repo_root)
    checks = [
        *_binary_checks(repo_root),
        _runtime_resolution_check(repo_root),
        _installed_spice_source_check(repo_root),
        _skill_check(repo_root),
        _policy_check(repo_root),
        _git_clean_check(repo_root),
        _hooks_check(repo_root),
        _shadowed_hooks_check(repo_root),
        _shape_check(repo_root),
        _file_loc_check(repo_root, paths),
        _complexity_check(repo_root, paths),
        _magic_numbers_check(repo_root, paths),
        _env_policy_check(repo_root, paths),
    ]
    return DoctorReport(repo_root=repo_root, checks=checks, fixes=fixes)


def render_doctor(repo_root: Path) -> str:
    return run_doctor(repo_root).render()


def _apply_safe_fixes(repo_root: Path) -> list[str]:
    from spice.hooks.install import install_hooks_for_repo

    # install_hooks_for_repo also materializes the `.spice/.gitignore` marker.
    rows = install_hooks_for_repo(repo_root)
    return [", ".join(rows)]


def _binary_checks(repo_root: Path) -> list[DoctorCheck]:
    from spice.serve.typecheck import serve_web_typecheck_targets

    checks: list[DoctorCheck] = []
    serve_web_present = bool(serve_web_typecheck_targets(repo_root))
    npm_note = (
        "serve web TypeScript checkJs backend"
        if serve_web_present
        else "optional; this repo has no serve web checkJs sources"
    )
    tts_binary, tts_note = _tts_binary_check_config(repo_root)
    for label, binary, required, note in (
        ("tool.git", "git", True, "required for repository checks"),
        ("tool.agent-driver", DRIVER.binary(), True, f"driver={DRIVER.name}"),
        ("tool.taskwarrior", "task", True, "required for the task control plane"),
        ("tool.judge", configured_judge_bin(repo_root), True, "maxim judging"),
        ("tool.tts", tts_binary, False, tts_note),
        ("tool.ruff", "ruff", True, "pre-commit formatter/linter"),
        ("tool.lizard", "lizard", True, "complexity scan backend"),
        ("tool.npm", "npm", serve_web_present, npm_note),
    ):
        located = find_tool(binary)
        if located:
            checks.append(
                _ok(label, f"{binary} -> {located}; {note}", "which " + binary)
            )
        elif required:
            checks.append(_fail(label, f"{binary} missing; {note}", "spice dev doctor"))
        else:
            checks.append(_warn(label, f"{binary} missing; {note}", "spice dev doctor"))
    return checks


def _tts_binary_check_config(repo_root: Path) -> tuple[str, str]:
    backend = configured_say_backend(repo_root)
    if backend == "external":
        command = configured_say_command(repo_root)
        try:
            argv = shlex.split(command) if command else []
        except ValueError:
            argv = []
        binary = argv[0] if argv else "external-speech-command"
        return binary, "optional external speech backend"
    return "say", "optional macOS speech; use external backend on Linux"


def _runtime_resolution_check(repo_root: Path) -> DoctorCheck:
    del repo_root
    runtime = runtime_spice_source()
    return _ok(
        "runtime.spice",
        f"installed spice package -> {runtime}",
        "spice dev doctor",
    )


def _installed_spice_source_check(repo_root: Path) -> DoctorCheck:
    worktree = worktree_spice_source(repo_root)
    installed = _installed_spice_package_source()
    if installed is None:
        return _warn(
            "runtime.installed-spice",
            "installed spice package source is unavailable",
            "python -m pip show spice",
        )
    if worktree is None:
        return _ok(
            "runtime.installed-spice",
            f"installed spice source -> {installed}",
            "python -m pip show spice",
        )
    if installed.resolve() == worktree.resolve():
        return _ok(
            "runtime.installed-spice",
            f"installed spice package matches worktree -> {installed}",
            "python -m pip show spice",
        )
    return _warn(
        "runtime.installed-spice",
        f"installed spice package is {installed}; worktree source is {worktree}",
        "spice dev doctor",
    )


def _installed_spice_package_source() -> Path | None:
    entrypoint = shutil.which("spice")
    if entrypoint is None:
        return None
    python = _python_from_script_shebang(Path(entrypoint))
    if python is None:
        return None
    return _spice_package_source_for_python(python)


def _spice_package_source_for_python(python: Path) -> Path | None:
    env = dict(os.environ)
    env.pop("PYTHONPATH", None)
    result = subprocess.run(
        [
            str(python),
            "-c",
            (
                "from pathlib import Path;"
                "import spice.cli.entry as entry;"
                "print(Path(entry.__file__).resolve().parents[1])"
            ),
        ],
        capture_output=True,
        check=False,
        cwd=Path("/"),
        env=env,
        text=True,
    )
    if result.returncode != 0:
        return None
    raw = result.stdout.strip()
    return Path(raw).expanduser().resolve() if raw else None


def _python_from_script_shebang(path: Path) -> Path | None:
    try:
        first_line = path.read_text(encoding="utf-8", errors="replace").splitlines()[0]
    except (OSError, IndexError):
        return None
    if not first_line.startswith("#!"):
        return None
    try:
        parts = shlex.split(first_line[2:].strip())
    except ValueError:
        return None
    if not parts:
        return None
    if Path(parts[0]).name == "env" and len(parts) > 1:
        located = shutil.which(parts[1])
        return Path(located).expanduser() if located else None
    return Path(parts[0]).expanduser()


def _skill_check(repo_root: Path) -> DoctorCheck:
    from spice.agent.lifecycle import WORKTREE_SKILL_RELATIVE_PATH

    packaged = packaged_skill_path()
    if not packaged.is_file():
        return _fail(
            "skill", f"packaged skill missing at {packaged}", "spice agent ensure"
        )
    target = repo_root / WORKTREE_SKILL_RELATIVE_PATH
    location = WORKTREE_SKILL_RELATIVE_PATH.as_posix()
    if not target.is_file():
        return _warn(
            "skill",
            f"{location} not materialized yet; activation writes it",
            "spice agent activation",
        )
    if target.read_text(encoding="utf-8") == packaged.read_text(encoding="utf-8"):
        return _ok("skill", f"{location} current", "spice agent activation")
    return _ok(
        "skill",
        f"{location} differs from packaged (tracked copy, or refreshed on "
        "next activation when generated)",
        "spice agent activation",
    )


def _policy_check(repo_root: Path) -> DoctorCheck:
    roots = shape.configured_package_roots(repo_root)
    if not roots:
        return _warn(
            "policy.package-roots",
            "no active package roots; Python package shape guards are inactive",
            "spice study shape",
        )
    listed = ", ".join(root.relative_to(repo_root).as_posix() for root in roots)
    return _ok("policy.package-roots", listed, "spice study shape")


def _git_clean_check(repo_root: Path) -> DoctorCheck:
    result = _git(repo_root, "status", "--porcelain")
    if result.returncode != 0:
        return _fail("git.clean", _command_problem(result), "git status --short")
    dirty = [line for line in result.stdout.splitlines() if line.strip()]
    if dirty:
        return _fail(
            "git.clean",
            f"{len(dirty)} dirty path(s); commit, stash, or remove local changes",
            "git status --short",
        )
    return _ok("git.clean", "working tree clean", "git status --short")


def _hooks_check(repo_root: Path) -> DoctorCheck:
    configured = git_worktree_config_get(repo_root, "core.hooksPath") or "-"
    expected = hooks_dir(repo_root).relative_to(repo_root).as_posix()
    hook_errors = _hook_installation_errors(repo_root)
    if configured != expected:
        hook_errors.insert(0, f"core.hooksPath={configured}; expected {expected}")
    if hook_errors:
        return _fail(
            "hooks.installed",
            "; ".join(hook_errors),
            "spice dev install-hooks",
        )
    return _ok(
        "hooks.installed", f"core.hooksPath={expected}", "spice dev install-hooks"
    )


def _hook_installation_errors(repo_root: Path) -> list[str]:
    errors: list[str] = []
    directory = hooks_dir(repo_root)
    for name, args in HOOK_ARGS.items():
        path = directory / name
        expected = hook_shim_content(args)
        try:
            actual = path.read_text(encoding="utf-8")
        except OSError:
            errors.append(f"{path.relative_to(repo_root).as_posix()} missing")
            continue
        if actual != expected:
            errors.append(f"{path.relative_to(repo_root).as_posix()} stale")
    return errors


def _shadowed_hooks_check(repo_root: Path) -> DoctorCheck:
    configured = git_worktree_config_get(repo_root, "core.hooksPath") or "-"
    expected = hooks_dir(repo_root).relative_to(repo_root).as_posix()
    if configured != expected:
        return _ok(
            "hooks.shadowed",
            "spice hooksPath is not active",
            "spice dev install-hooks",
        )
    shadowed = _unique_hook_paths(
        [
            *_shadowed_default_hooks(repo_root),
            *_shadowed_configured_hooks(repo_root, expected),
        ]
    )
    if not shadowed:
        return _ok(
            "hooks.shadowed",
            "no executable .git/hooks entries shadowed",
            "spice dev install-hooks",
        )
    listed = ", ".join(shadowed)
    return _warn(
        "hooks.shadowed",
        f"core.hooksPath={expected} shadows executable hook(s): {listed}",
        "spice dev install-hooks",
    )


def _shadowed_default_hooks(repo_root: Path) -> list[str]:
    directory = git_common_dir(repo_root) / "hooks"
    return _executable_hook_paths(repo_root, directory)


def _shadowed_configured_hooks(repo_root: Path, expected: str) -> list[str]:
    shadowed: list[str] = []
    spice_hooks = hooks_dir(repo_root)
    for raw in _git_local_config_values(repo_root, "core.hooksPath"):
        if not raw or raw == expected:
            continue
        directory = _configured_hooks_path(repo_root, raw)
        if _same_path(directory, spice_hooks):
            continue
        shadowed.extend(_executable_hook_paths(repo_root, directory))
    return shadowed


def _git_local_config_values(repo_root: Path, key: str) -> list[str]:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo_root), "config", "--local", "--get-all", key],
            capture_output=True,
            check=False,
            text=True,
        )
    except OSError:
        return []
    if result.returncode != 0:
        return []
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def _configured_hooks_path(repo_root: Path, raw: str) -> Path:
    path = Path(raw).expanduser()
    return path if path.is_absolute() else repo_root / path


def _same_path(left: Path, right: Path) -> bool:
    return left.resolve(strict=False) == right.resolve(strict=False)


def _executable_hook_paths(repo_root: Path, directory: Path) -> list[str]:
    if not directory.is_dir():
        return []
    shadowed: list[str] = []
    for path in sorted(directory.iterdir(), key=lambda item: item.name):
        if not path.is_file() or path.name.endswith(".sample"):
            continue
        if os.access(path, os.X_OK):
            shadowed.append(_display_path(repo_root, path))
    return shadowed


def _display_path(repo_root: Path, path: Path) -> str:
    try:
        return path.relative_to(repo_root).as_posix()
    except ValueError:
        return str(path)


def _unique_hook_paths(paths: list[str]) -> list[str]:
    unique: list[str] = []
    for path in paths:
        if path not in unique:
            unique.append(path)
    return unique


def _shape_check(repo_root: Path) -> DoctorCheck:
    return _spice_error_check(
        "shape",
        "spice study shape",
        lambda: "\n".join(
            error
            for error in (
                shape.namespace_policy_error(repo_root),
                shape.path_shape_error(repo_root),
                shape.name_cluster_error(repo_root),
            )
            if error
        ),
    )


def _file_loc_check(repo_root: Path, paths: list[Path]) -> DoctorCheck:
    findings = fileloc.scan_loc_violations(paths, root=repo_root)
    line_flex = _flex(FILE_LOC_LIMIT)
    byte_flex = _flex(FILE_BYTE_LIMIT)
    if findings:
        return _fail(
            "file-loc",
            f"{len(findings)} violation(s); line_limit {FILE_LOC_LIMIT} "
            f"flex {line_flex} byte_limit {FILE_BYTE_LIMIT} byte_flex {byte_flex}",
            "spice study file-loc",
        )
    return _ok(
        "file-loc",
        f"ok; line_limit {FILE_LOC_LIMIT} flex {line_flex} "
        f"byte_limit {FILE_BYTE_LIMIT} byte_flex {byte_flex}",
        "spice study file-loc",
    )


def _complexity_check(repo_root: Path, paths: list[Path]) -> DoctorCheck:
    try:
        records = complexity.collect_complexity_records(paths, root=repo_root)
    except SpiceError as exc:
        return _fail("complexity", str(exc), "spice study complexity")
    ccn_flex = _flex(COMPLEXITY_MAX_CCN)
    length_flex = _flex(COMPLEXITY_MAX_LENGTH)
    findings = [
        record
        for record in records
        if record.ccn > ccn_flex or record.length > length_flex
    ]
    if findings:
        return _fail(
            "complexity",
            f"{len(findings)} violation(s); ccn_limit {COMPLEXITY_MAX_CCN} "
            f"flex {ccn_flex} length_limit {COMPLEXITY_MAX_LENGTH} "
            f"length_flex {length_flex}",
            "spice study complexity",
        )
    return _ok(
        "complexity",
        f"ok; ccn_limit {COMPLEXITY_MAX_CCN} flex {ccn_flex} "
        f"length_limit {COMPLEXITY_MAX_LENGTH} length_flex {length_flex}",
        "spice study complexity",
    )


def _magic_numbers_check(repo_root: Path, paths: list[Path]) -> DoctorCheck:
    findings = magicnums.detect_magic_regressions(paths, root=repo_root)
    if findings:
        return _fail(
            "magic-numbers",
            f"{len(findings)} regression(s) vs {MAGIC_BASELINE_REF}",
            "spice study magic-numbers",
        )
    return _ok(
        "magic-numbers",
        f"ok vs {MAGIC_BASELINE_REF}",
        "spice study magic-numbers",
    )


def _env_policy_check(repo_root: Path, paths: list[Path]) -> DoctorCheck:
    findings = envpolicy.scan_env_policy(paths, root=repo_root)
    if findings:
        return _fail(
            "env-policy",
            f"{len(findings)} undeclared environment literal(s)",
            "spice study env-policy",
        )
    return _ok("env-policy", "ok", "spice study env-policy")


def _spice_error_check(
    name: str, command: str, probe: Callable[[], str]
) -> DoctorCheck:
    try:
        detail = probe()
    except SpiceError as exc:
        return _fail(name, str(exc), command)
    if detail:
        return _fail(name, detail.splitlines()[0], command)
    return _ok(name, "ok", command)


def _ok(name: str, detail: str, command: str) -> DoctorCheck:
    return DoctorCheck(name=name, status="ok", detail=detail, command=command)


def _warn(name: str, detail: str, command: str) -> DoctorCheck:
    return DoctorCheck(name=name, status="warn", detail=detail, command=command)


def _fail(name: str, detail: str, command: str) -> DoctorCheck:
    return DoctorCheck(name=name, status="fail", detail=detail, command=command)


def _git(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        check=False,
        text=True,
    )


def _command_problem(result: subprocess.CompletedProcess[str]) -> str:
    text = (result.stderr or result.stdout).strip()
    return text or f"command exited {result.returncode}"


def _flex(value: int) -> int:
    from spice.flexstate import flex_limit

    return flex_limit(value)
