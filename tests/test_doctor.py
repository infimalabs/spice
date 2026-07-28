"""Doctor checks: first-run gaps, fixable generated state, and dirty trees."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from spice.config import edit, layers, values
from spice.agent.rtkhealth import RtkHealth
from spice.hooks import doctor
from spice.hooks.install import hooks_dir, install_hooks_for_repo
from spice.paths import shared_state_root, worktree_state_root
from spice.studies.walk import staged_paths, tracked_paths
import pytest


def test_doctor_renders_supported_state_roots_for_linked_worktrees(tmp_path):
    repo = _repo(tmp_path)
    linked = tmp_path / "linked"
    _run(repo, "git", "worktree", "add", "-q", "-b", "linked", str(linked))

    primary = doctor.DoctorReport(repo_root=repo, checks=[], fixes=[]).render()
    peer = doctor.DoctorReport(repo_root=linked, checks=[], fixes=[]).render()

    assert _state_root_lines(primary) == [
        f"worktree_config_state_root={worktree_state_root(repo) / 'config'}",
        f"shared_state_root={shared_state_root(repo)}",
        f"worktree_state_root={worktree_state_root(repo)}",
    ]
    assert _state_root_lines(peer) == [
        f"worktree_config_state_root={worktree_state_root(linked) / 'config'}",
        f"shared_state_root={shared_state_root(linked)}",
        f"worktree_state_root={worktree_state_root(linked)}",
    ]


def test_doctor_reports_missing_hooks_and_fix_installs_them(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    _patch_non_hook_checks(monkeypatch)

    report = doctor.run_doctor(repo)

    hook_check = _check(report, "hooks.installed")
    assert report.failed
    assert hook_check.status == "fail"
    assert "core.hooksPath=-" in hook_check.detail
    assert "cmd: spice dev install-hooks" in report.render()

    fixed = doctor.run_doctor(repo, fix=True)

    assert not fixed.failed
    assert _check(fixed, "hooks.installed").status == "ok"
    assert "fixed hook pre-commit" in fixed.render()
    assert (hooks_dir(repo) / "pre-commit").is_file()
    assert (hooks_dir(repo) / "commit-msg").is_file()
    assert (hooks_dir(repo) / "reference-transaction").is_file()


def test_doctor_fails_dirty_worktree_with_investigation_command(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    install_hooks_for_repo(repo)
    _patch_non_hook_checks(monkeypatch)
    (repo / "pkg" / "module.py").write_text("VALUE = 2\n", encoding="utf-8")

    report = doctor.run_doctor(repo)

    clean = _check(report, "git.clean")
    assert report.failed
    assert clean.status == "fail"
    assert "dirty path" in clean.detail
    assert "cmd: git status --short" in report.render()


def test_doctor_reports_builtin_shadowing_mount_as_refused(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    (repo / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["pkg"]\n'
        '[tool.spice.commands]\ntask = ["./scripts/task"]\n',
        encoding="utf-8",
    )
    install_hooks_for_repo(repo)
    _patch_non_hook_checks(monkeypatch)

    report = doctor.run_doctor(repo)

    mounts = _check(report, "commands.mounts")
    assert report.failed
    assert mounts.status == "fail"
    assert "refused 1 mount(s)" in mounts.detail
    assert (
        f"commands (source=pyproject path={repo / 'pyproject.toml'})" in mounts.detail
    )
    assert "entry 'task'" in mounts.detail
    assert "shadows a built-in spice command" in mounts.detail


def test_doctor_warns_about_executable_default_hooks_shadowed_by_spice(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    install_hooks_for_repo(repo)
    _patch_non_hook_checks(monkeypatch)
    for name in ("pre-push", "post-merge"):
        path = repo / ".git" / "hooks" / name
        path.write_text("#!/usr/bin/env sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)

    report = doctor.run_doctor(repo)

    shadowed = _check(report, "hooks.shadowed")
    assert not report.failed
    assert shadowed.status == "warn"
    assert ".git/hooks/pre-push" in shadowed.detail
    assert ".git/hooks/post-merge" in shadowed.detail
    assert "core.hooksPath=.spice/hooks shadows" in shadowed.detail


def test_doctor_warns_about_repo_local_hooks_path_shadowed_by_spice(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    configured = repo / ".githooks"
    configured.mkdir()
    pre_push = configured / "pre-push"
    pre_push.write_text("#!/usr/bin/env sh\nexit 0\n", encoding="utf-8")
    pre_push.chmod(0o755)
    _run(repo, "git", "add", ".githooks/pre-push")
    _run(repo, "git", "commit", "-m", "add repo hooks")
    _run(repo, "git", "config", "core.hooksPath", ".githooks")
    install_hooks_for_repo(repo)
    _patch_non_hook_checks(monkeypatch)

    report = doctor.run_doctor(repo)

    shadowed = _check(report, "hooks.shadowed")
    assert not report.failed
    assert shadowed.status == "warn"
    assert ".githooks/pre-push" in shadowed.detail
    assert "core.hooksPath=.spice/hooks shadows" in shadowed.detail


def test_dev_doctor_parser_exposes_fix_flag():
    from spice.cli.parser import build_parser

    args = build_parser().parse_args(["dev", "doctor", "--fix"])

    assert args.dev_command == "doctor"
    assert args.fix


@pytest.mark.parametrize(
    ("health", "expected_status"),
    [
        (
            RtkHealth(
                "alternate-rtk",
                "active",
                "rewrite protocol valid (exit 3)",
                "0.42.4",
            ),
            "ok",
        ),
        (RtkHealth("missing-rtk", "missing", "launch failed"), "skip"),
        (
            RtkHealth("/opt/old-rtk", "obsolete", "RTK 0.41.0 is obsolete", "0.41.0"),
            "warn",
        ),
        (
            RtkHealth("broken-rtk", "protocol-invalid", "rewrite probe invalid"),
            "warn",
        ),
    ],
)
def test_doctor_rtk_check_reports_health_without_failing(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    health: RtkHealth,
    expected_status: str,
) -> None:
    monkeypatch.setattr(doctor, "probe_rtk_health", lambda _repo: health)

    check = doctor._rtk_check(tmp_path)

    assert {
        "status": check.status,
        "required": check.required,
        "detail_has_executable": f"executable={health.executable!r}" in check.detail,
        "detail_has_mode": f"mode={health.mode}" in check.detail,
        "command": check.command,
    } == {
        "status": expected_status,
        "required": False,
        "detail_has_executable": True,
        "detail_has_mode": True,
        "command": health.verification_command(),
    }


@pytest.mark.parametrize(
    "health",
    [
        RtkHealth("rtk", "active", "valid", "0.42.4"),
        RtkHealth("missing-rtk", "missing", "launch failed"),
        RtkHealth("old-rtk", "obsolete", "obsolete", "0.41.0"),
        RtkHealth("broken-rtk", "protocol-invalid", "invalid"),
    ],
)
def test_doctor_runs_remaining_checks_for_every_rtk_health_state(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    health: RtkHealth,
) -> None:
    repo = _repo(tmp_path)
    monkeypatch.setattr(doctor, "probe_rtk_health", lambda _repo: health)

    report = doctor.run_doctor(repo)
    names = [check.name for check in report.checks]
    expected_rtk = (
        "ok" if health.active else "skip" if health.state == "missing" else "warn"
    )

    assert {
        "rtk": _check(report, "tool.rtk").status,
        "remaining_check": names[-1],
        "check_count": len(names),
    } == {
        "rtk": expected_rtk,
        "remaining_check": "env-name-ledger",
        "check_count": 26,
    }


def test_doctor_runs_committed_mutation_ratchet_through_study_engine(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    ratchet = repo / doctor.mutations.STANDING_MUTATION_RATCHET_PATH
    ratchet.parent.mkdir(parents=True, exist_ok=True)
    ratchet.write_text(
        json.dumps(
            {
                "version": 1,
                "modules": {"pkg/module.py": {"score": 0.5}},
                "standing": {
                    "surface": "spice dev doctor",
                    "targets": ["pkg/module.py"],
                    "tests": ["tests/test_module.py"],
                    "maxMutantsPerModule": 4,
                    "timeoutSeconds": 7,
                    "equivalentMutants": [
                        {
                            "path": "pkg/module.py",
                            "mutationIndex": 0,
                            "reason": "equivalent fixture mutant",
                        }
                    ],
                    "lowInformationMutants": [
                        {
                            "path": "pkg/module.py",
                            "mutationIndex": 1,
                            "reason": "fixture-wide error",
                        }
                    ],
                    "retainedZeroConstraintTests": {
                        "tests/test_module.py::test_public_contract": (
                            "public fixture contract"
                        )
                    },
                },
            }
        ),
        encoding="utf-8",
    )
    calls = {}

    def run_mutation_study(paths, **kwargs):
        calls["paths"] = paths
        calls.update(kwargs)
        return doctor.mutations.MutationStudy(
            reports=(
                doctor.mutations.ModuleMutationReport(
                    path="pkg/module.py",
                    mutants=4,
                    killed=2,
                    survived=2,
                    timed_out=0,
                    results=(),
                    zero_constraint_tests=(
                        "tests/test_module.py::test_public_contract",
                    ),
                ),
            )
        )

    monkeypatch.setattr(doctor.mutations, "run_mutation_study", run_mutation_study)

    report = doctor.run_doctor(repo)
    check = _check(report, "mutation-ratchet")

    assert check.status == "ok"
    assert check.command == "spice dev doctor"
    assert "pkg/module.py=50%" in check.detail
    assert "handled equivalent=1 low-information=1" in check.detail
    assert calls == {
        "paths": [Path("pkg/module.py")],
        "root": repo,
        "test_paths": [Path("tests/test_module.py")],
        "max_mutants_per_module": 4,
        "timeout_seconds": 7,
        "ratchet_path": ratchet,
    }


def test_doctor_reports_installed_runtime_for_spice_checkout(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    installed = tmp_path / "installed" / "spice"
    repo.mkdir()
    installed.mkdir(parents=True)
    _write_spice_product_shape(repo)
    monkeypatch.setattr(doctor, "runtime_spice_source", lambda: installed)

    check = doctor._runtime_resolution_check(repo)

    assert check.status == "ok"
    assert f"installed spice package -> {installed}" == check.detail


def test_doctor_reports_single_spice_namespace_portion(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    checkout = tmp_path / "checkout"
    package = checkout / "spice"
    module = package / "hooks" / "doctor.py"
    finder_path_hook = "__editable__.spice_harness-0.16.0.finder.__path_hook__"
    repo.mkdir()
    module.parent.mkdir(parents=True)
    ordinary_cwd_portions = doctor._spice_namespace_portions_from(
        [
            package,
            package,
            finder_path_hook,
        ],
        [module],
    )
    cwd_beneath_spice = tmp_path / "unrelated" / "spice" / "nested"
    cwd_beneath_spice.mkdir(parents=True)
    monkeypatch.chdir(cwd_beneath_spice)
    portions = doctor._spice_namespace_portions_from(
        [package, package, finder_path_hook],
        [module],
    )
    monkeypatch.setattr(doctor, "_spice_namespace_portions", lambda: portions)

    check = doctor._spice_namespace_portions_check(repo)

    assert ordinary_cwd_portions == [checkout.resolve()]
    assert portions == [checkout.resolve()]
    assert check.status == "ok"
    assert f"single spice namespace portion -> {checkout.resolve()}" == check.detail


def test_doctor_reports_mixed_spice_namespace_portions(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    first = tmp_path / "first"
    second = tmp_path / "second"
    repo.mkdir()
    (first / "spice" / "hooks").mkdir(parents=True)
    (second / "spice" / "cli").mkdir(parents=True)
    portions = doctor._spice_namespace_portions_from(
        [first / "spice", second / "spice"],
        [
            first / "spice" / "hooks" / "doctor.py",
            second / "spice" / "cli" / "entry.py",
        ],
    )
    monkeypatch.setattr(doctor, "_spice_namespace_portions", lambda: portions)

    check = doctor._spice_namespace_portions_check(repo)

    assert portions == [first.resolve(), second.resolve()]
    assert check.status == "fail"
    assert "conflicting spice namespace portions" in check.detail
    assert str(first.resolve()) in check.detail
    assert str(second.resolve()) in check.detail


def test_doctor_reports_installed_tool_runtime_for_spice_checkout(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    entrypoint = tmp_path / "tool" / "bin" / "spice"
    python = tmp_path / "tool" / "bin" / "python"
    installed = tmp_path / "tool" / "spice"
    repo.mkdir()
    entrypoint.parent.mkdir(parents=True)
    installed.mkdir()
    _write_spice_product_shape(repo)
    monkeypatch.setattr(
        doctor,
        "_installed_spice_runtime",
        lambda: _installed_runtime(entrypoint, python, installed, editable=True),
    )

    check = doctor._installed_spice_source_check(repo)

    assert check.status == "ok"
    assert (
        f"installed spice tool -> {entrypoint}; "
        f"interpreter -> {python}; package -> {installed}; "
        "version 0.25.0 (editable)"
    ) == check.detail


def test_doctor_fails_non_editable_installed_tool_runtime(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    entrypoint = tmp_path / "tool" / "bin" / "spice"
    python = tmp_path / "tool" / "bin" / "python"
    installed = tmp_path / "tool" / "spice"
    repo.mkdir()
    _write_spice_product_shape(repo)
    monkeypatch.setattr(
        doctor,
        "_installed_spice_runtime",
        lambda: _installed_runtime(entrypoint, python, installed, editable=True),
    )
    editable_check = doctor._installed_spice_source_check(repo)
    monkeypatch.setattr(
        doctor,
        "_installed_spice_runtime",
        lambda: _installed_runtime(entrypoint, python, installed, editable=False),
    )

    frozen_check = doctor._installed_spice_source_check(repo)

    assert editable_check.status == "ok"
    assert frozen_check.status == "fail"
    assert frozen_check.detail != editable_check.detail
    assert "reinstall with `uv tool install -e <checkout>`" in frozen_check.detail


def test_doctor_warns_when_installed_version_differs_from_checkout(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    entrypoint = tmp_path / "tool" / "bin" / "spice"
    python = tmp_path / "tool" / "bin" / "python"
    installed = tmp_path / "tool" / "spice"
    repo.mkdir()
    _write_spice_product_shape(repo)
    (repo / "pyproject.toml").write_text(
        '[project]\nname = "spice-harness"\nversion = "0.26.0"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(
        doctor,
        "_installed_spice_runtime",
        lambda: _installed_runtime(entrypoint, python, installed, editable=True),
    )

    drift_check = doctor._installed_spice_source_check(repo)
    (repo / "pyproject.toml").write_text(
        '[project]\nname = "spice-harness"\nversion = "0.25.0"\n',
        encoding="utf-8",
    )
    settled_check = doctor._installed_spice_source_check(repo)

    assert drift_check.status == "warn"
    assert settled_check.status == "ok"
    assert drift_check.detail != settled_check.detail
    assert "version 0.25.0 (editable); checkout pyproject declares 0.26.0" in (
        drift_check.detail
    )


def test_doctor_warns_when_installed_tool_runtime_is_unavailable(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    _write_spice_product_shape(repo)
    monkeypatch.setattr(doctor, "_installed_spice_runtime", lambda: None)

    check = doctor._installed_spice_source_check(repo)

    assert check.status == "warn"
    assert "installed spice package source is unavailable" == check.detail


def test_doctor_worktree_venv_check_reports_dev_imports_and_uv_sync_recovery(
    tmp_path,
):
    plain = tmp_path / "plain"
    plain.mkdir()
    repo = tmp_path / "repo"
    repo.mkdir()
    _write_spice_product_shape(repo)
    python = repo / ".venv" / "bin" / "python"

    plain_check = doctor._worktree_venv_check(plain)
    missing_check = doctor._worktree_venv_check(repo)
    python.parent.mkdir(parents=True)
    python.write_text("#!/bin/sh\necho '9.1.0 3.8.0'\n", encoding="utf-8")
    python.chmod(0o755)
    imports_check = doctor._worktree_venv_check(repo)
    python.write_text(
        "#!/bin/sh\necho 'No module named xdist' >&2\nexit 1\n", encoding="utf-8"
    )
    broken_check = doctor._worktree_venv_check(repo)

    assert plain_check.status == "ok"
    assert "not a spice checkout" in plain_check.detail
    assert missing_check.status == "fail"
    assert f"{python} missing; create the worktree venv with `uv sync`" == (
        missing_check.detail
    )
    assert imports_check.status == "ok"
    assert f"{python} imports pytest and xdist (9.1.0 3.8.0)" == imports_check.detail
    assert broken_check.status == "fail"
    assert "No module named xdist" in broken_check.detail
    assert broken_check.detail != imports_check.detail


def test_doctor_wrapper_seam_check_requires_dev_pytest_argv(tmp_path):
    routed = _wrapper_repo(tmp_path / "routed", '["spice", "dev", "pytest"]')
    bypassed = _wrapper_repo(tmp_path / "bypassed", '["python", "-m", "pytest"]')

    routed_check = doctor._wrapper_seam_check(routed)
    bypassed_check = doctor._wrapper_seam_check(bypassed)

    assert routed_check.status == "ok"
    assert "pytest -> spice dev pytest" in routed_check.detail
    assert bypassed_check.status == "fail"
    assert "bypasses the dev self-exec seam" in bypassed_check.detail
    assert bypassed_check.detail != routed_check.detail


def _wrapper_repo(repo: Path, pytest_argv: str) -> Path:
    repo.mkdir()
    _run(repo, "git", "init", "-b", "main")
    (repo / "pyproject.toml").write_text(
        "[tool.spice.agent]\n"
        'wrappers = ["spice-dev"]\n'
        "[tool.spice.wrappers.spice-dev.pytest]\n"
        f"argv = {pytest_argv}\n",
        encoding="utf-8",
    )
    return repo


def _installed_runtime(
    entrypoint: Path, python: Path, installed: Path, *, editable: bool
) -> doctor.InstalledSpiceRuntime:
    return doctor.InstalledSpiceRuntime(
        entrypoint=entrypoint,
        python=python,
        source=installed,
        version="0.25.0",
        editable=editable,
    )


def test_doctor_reports_file_loc_standing_debt_as_info_with_scopes_and_excludes(
    tmp_path,
):
    repo = _repo(tmp_path)
    (repo / "pyproject.toml").write_text(
        """
        [tool.spice.policy]
        package_roots = ["pkg"]
        exclude = ["generated/"]

        [tool.spice.policy.limits]
        file_loc = 20
        file_bytes = 100000

        [tool.spice.policy.flex]
        ratio = 1.0

        [[tool.spice.policy.rules]]
        scopes = { paths = ["legacy/**"] }

        [tool.spice.policy.rules.file_loc]
        multiplier = 10.0
        """,
        encoding="utf-8",
    )
    (repo / "src").mkdir()
    (repo / "legacy").mkdir()
    (repo / "generated").mkdir()
    (repo / "src" / "large.py").write_text("x = 1\n" * 21, encoding="utf-8")
    (repo / "legacy" / "large.py").write_text("x = 1\n" * 30, encoding="utf-8")
    (repo / "generated" / "large.py").write_text("x = 1\n" * 30, encoding="utf-8")
    _run(repo, "git", "add", ".")
    _run(repo, "git", "commit", "-m", "add standing file-loc debt")

    check = doctor._file_loc_check(repo, tracked_paths(repo), staged_paths(repo))

    assert check.status == "info"
    assert "commit-blocking ok" in check.detail
    assert "standing 1 informational violation(s)" in check.detail


def test_doctor_reports_env_policy_standing_debt_as_info(tmp_path):
    repo = _repo(tmp_path)
    (repo / "pkg" / "env_access.py").write_text(
        'import os\nVALUE = os.getenv("HOME")\n',  # env-policy: allow
        encoding="utf-8",
    )
    _run(repo, "git", "add", ".")
    _run(repo, "git", "commit", "-m", "add standing env-policy debt")

    check = doctor._env_policy_check(repo, tracked_paths(repo), staged_paths(repo))

    assert check.status == "info"
    assert "commit-blocking ok" in check.detail
    assert "standing 1 informational undeclared environment literal(s)" in check.detail


def test_doctor_complexity_uses_staged_scan_with_scoped_bounds(
    tmp_path,
    monkeypatch,
):
    repo = _repo(tmp_path)
    (repo / "pyproject.toml").write_text(
        """
        [tool.spice.policy]
        package_roots = ["pkg"]

        [tool.spice.policy.limits]
        routine_ccn = 5
        routine_length = 8

        [tool.spice.policy.flex]
        ratio = 1.0

        [[tool.spice.policy.rules]]
        scopes = { paths = ["legacy/**"] }

        [tool.spice.policy.rules.routine_ccn]
        multiplier = 2.0
        """,
        encoding="utf-8",
    )
    calls: list[tuple[tuple[Path, ...], int, bool]] = []
    finding = doctor.complexity.ComplexityFinding(
        record=doctor.complexity.ComplexityRecord(
            path="src/app.py",
            function_name="main",
            ccn=7,
            length=6,
            nloc=6,
        ),
        over_ccn=True,
        over_length=False,
        ccn_limit=5,
        length_limit=8,
        ccn_flex_breach=True,
        length_flex_breach=False,
    )

    def scan(
        paths: list[Path],
        *,
        bounds_for_path,
        persist: bool,
        **_kwargs,
    ) -> list[doctor.complexity.ComplexityFinding]:
        legacy_ccn = bounds_for_path(Path("legacy/app.py")).max_ccn
        calls.append((tuple(paths), legacy_ccn, persist))
        return [finding] if Path("src/app.py") in paths else []

    monkeypatch.setattr(
        doctor.complexity,
        "scan_staged_complexity_violations",
        scan,
    )

    check = doctor._complexity_check(repo, [Path("src/app.py")], [])

    assert check.status == "info"
    assert "standing 1 informational violation(s)" in check.detail
    assert calls == [((), 10, False), ((Path("src/app.py"),), 10, False)]


def _patch_non_hook_checks(monkeypatch) -> None:
    monkeypatch.setattr(doctor, "_binary_checks", lambda _repo_root: [])
    monkeypatch.setattr(
        doctor,
        "_skill_check",
        lambda _repo_root: doctor.DoctorCheck(
            "skill", "ok", "ok", "spice agent activation"
        ),
    )
    monkeypatch.setattr(
        doctor,
        "_policy_check",
        lambda _repo_root: doctor.DoctorCheck(
            "policy.package-roots", "ok", "pkg", "spice study shape"
        ),
    )
    for name, command in (
        ("shape", "spice study shape"),
        ("file-loc", "spice study file-loc"),
        ("complexity", "spice study complexity"),
        ("magic-numbers", "spice study magic-numbers"),
        ("env-policy", "spice study env-policy"),
        ("env-name-ledger", "spice study env-name-ledger"),
    ):
        monkeypatch.setattr(
            doctor,
            f"_{name.replace('-', '_')}_check",
            lambda *_args, name=name, command=command: doctor.DoctorCheck(
                name, "ok", "ok", command
            ),
        )


def test_doctor_judge_optional_by_default_and_required_when_opted_in(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    monkeypatch.setattr(doctor, "find_tool", lambda _binary: None)

    default_check = _binary_check(repo, "tool.judge")
    edit.set_scope_section(
        repo,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_ENABLED_KEY: True},
    )
    opted_in_check = _binary_check(repo, "tool.judge")

    assert [
        (
            default_check.status,
            default_check.required,
            "judge-free" in default_check.detail,
        ),
        (
            opted_in_check.status,
            opted_in_check.required,
            "opted in" in opted_in_check.detail,
        ),
    ] == [("skip", False, True), ("fail", True, True)]


@pytest.mark.parametrize(
    ("version", "expected_status"),
    (("2.6.2", "fail"), ("3.0.0", "ok"), ("4.1.0", "ok")),
)
def test_doctor_enforces_taskwarrior_three_version(
    monkeypatch, version, expected_status
):
    calls = []

    def run_tool(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=f"{version}\n",
            stderr="",
        )

    monkeypatch.setattr(doctor, "run_tool_command", run_tool)

    check = doctor._taskwarrior_check("/tools/task")

    assert check.status == expected_status
    assert "Taskwarrior 3" in check.detail
    assert "task control plane" in check.detail
    if expected_status == "fail":
        assert f"Taskwarrior {version} is below required" in check.detail
    assert calls == [
        (
            ["/tools/task", "--version"],
            {
                "policy": "probe",
                "operation": "probe Taskwarrior version",
                "capture_output": True,
                "text": True,
                "check": False,
            },
        )
    ]


def _binary_check(repo: Path, name: str) -> doctor.DoctorCheck:
    return next(check for check in doctor._binary_checks(repo) if check.name == name)


def _state_root_lines(rendered: str) -> list[str]:
    return [line.strip() for line in rendered.splitlines() if "state_root=" in line]


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "pkg").mkdir()
    (repo / "pkg" / "module.py").write_text("VALUE = 1\n", encoding="utf-8")
    (repo / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["pkg"]\n',
        encoding="utf-8",
    )
    _run(repo, "git", "init", "-b", "main")
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")
    _run(repo, "git", "add", ".")
    _run(repo, "git", "commit", "-m", "initial")
    return repo


def _write_spice_product_shape(repo: Path) -> None:
    for relative in (
        Path("spice") / "__main__.py",
        Path("spice") / "cli" / "entry.py",
        Path("spice") / "agent" / "wrap.py",
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")


def _check(report: doctor.DoctorReport, name: str) -> doctor.DoctorCheck:
    return next(check for check in report.checks if check.name == name)


def _run(repo: Path, *args: str) -> None:
    subprocess.run(args, cwd=repo, check=True, capture_output=True, text=True)


def test_doctor_treats_npm_as_optional_without_serve_web_sources(tmp_path, monkeypatch):
    _run(tmp_path, "git", "init", "-b", "main")
    real_find_tool = doctor.find_tool
    monkeypatch.setattr(
        doctor,
        "find_tool",
        lambda name: None if name == "npm" else real_find_tool(name),
    )

    checks = doctor._binary_checks(tmp_path)
    npm = next(check for check in checks if check.name == "tool.npm")

    assert npm.status == "skip"
    assert npm.required is False
    assert "optional -- you're fine without it" in npm.detail
    assert "no serve web checkJs sources" in npm.detail


def test_doctor_render_groups_optional_companions_and_reports_ready_posture(tmp_path):
    report = doctor.DoctorReport(
        repo_root=_repo(tmp_path),
        checks=[
            doctor.DoctorCheck("tool.git", "ok", "git -> /usr/bin/git", "which git"),
            doctor.DoctorCheck(
                "tool.tts",
                "skip",
                "optional -- you're fine without it; say missing",
                "spice dev doctor",
                required=False,
            ),
        ],
        fixes=[],
    )

    lines = report.render().splitlines()
    header_index = lines.index("  optional companions (safe to skip):")
    git_index = next(i for i, line in enumerate(lines) if "tool.git" in line)
    tts_index = next(i for i, line in enumerate(lines) if "tool.tts" in line)

    assert git_index < header_index < tts_index
    assert lines[-1] == (
        "  ready: required checks satisfied; "
        "1 optional companion(s) absent and safe to skip"
    )


def test_doctor_render_reports_attention_posture_when_required_check_fails(tmp_path):
    report = doctor.DoctorReport(
        repo_root=_repo(tmp_path),
        checks=[
            doctor.DoctorCheck(
                "git.clean", "fail", "2 dirty path(s)", "git status --short"
            ),
            doctor.DoctorCheck(
                "tool.rtk",
                "skip",
                "optional -- you're fine without it; state=missing",
                "rtk --version",
                required=False,
            ),
        ],
        fixes=[],
    )

    lines = report.render().splitlines()

    assert lines[-1] == (
        "  attention: 1 required check(s) failed; fix before relying on this worktree"
    )


def test_doctor_uses_configured_external_speech_backend(tmp_path, monkeypatch):
    _run(tmp_path, "git", "init", "-b", "main")
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: "tts-engine --wav",
        },
    )
    monkeypatch.setattr(doctor, "find_tool", lambda name: f"/tools/{name}")

    checks = doctor._binary_checks(tmp_path)
    tts = next(check for check in checks if check.name == "tool.tts")

    assert tts.status == "ok"
    assert "tts-engine -> /tools/tts-engine" in tts.detail
    assert "optional external speech backend" in tts.detail
