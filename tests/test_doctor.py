"""Doctor checks: first-run gaps, fixable generated state, and dirty trees."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from spice import config
from spice.agent.rtkhealth import RtkHealth
from spice.hooks import doctor
from spice.hooks.install import hooks_dir, install_hooks_for_repo
from spice.paths import shared_state_root, state_dir, worktree_state_root
from spice.studies.walk import staged_paths, tracked_paths
import pytest


def test_doctor_renders_supported_state_roots_for_linked_worktrees(tmp_path):
    repo = _repo(tmp_path)
    linked = tmp_path / "linked"
    _run(repo, "git", "worktree", "add", "-q", "-b", "linked", str(linked))

    primary = doctor.DoctorReport(repo_root=repo, checks=[], fixes=[]).render()
    peer = doctor.DoctorReport(repo_root=linked, checks=[], fixes=[]).render()

    assert _state_root_lines(primary) == [
        f"worktree_config_state_root={state_dir(repo)}",
        f"shared_state_root={shared_state_root(repo)}",
        f"worktree_state_root={worktree_state_root(repo)}",
    ]
    assert _state_root_lines(peer) == [
        f"worktree_config_state_root={state_dir(linked)}",
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
        (RtkHealth("missing-rtk", "missing", "launch failed"), "warn"),
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
        "detail_has_executable": f"executable={health.executable!r}" in check.detail,
        "detail_has_mode": f"mode={health.mode}" in check.detail,
        "command": check.command,
    } == {
        "status": expected_status,
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

    assert {
        "rtk": _check(report, "tool.rtk").status,
        "remaining_check": names[-1],
        "check_count": len(names),
    } == {
        "rtk": "ok" if health.active else "warn",
        "remaining_check": "env-name-ledger",
        "check_count": 23,
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
    repo.mkdir()
    module.parent.mkdir(parents=True)
    portions = doctor._spice_namespace_portions_from(
        [
            package,
            package,
            "__editable__.spice_harness-0.16.0.finder.__path_hook__",
        ],
        [module],
    )
    monkeypatch.setattr(doctor, "_spice_namespace_portions", lambda: portions)

    check = doctor._spice_namespace_portions_check(repo)

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
        lambda: doctor.InstalledSpiceRuntime(entrypoint, python, installed),
    )

    check = doctor._installed_spice_source_check(repo)

    assert check.status == "ok"
    assert (
        f"installed spice tool -> {entrypoint}; "
        f"interpreter -> {python}; package -> {installed}"
    ) == check.detail


def test_doctor_warns_when_installed_tool_runtime_is_unavailable(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    _write_spice_product_shape(repo)
    monkeypatch.setattr(doctor, "_installed_spice_runtime", lambda: None)

    check = doctor._installed_spice_source_check(repo)

    assert check.status == "warn"
    assert "installed spice package source is unavailable" == check.detail


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

        [tool.spice.policy.scopes."legacy/**".file_loc]
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

        [tool.spice.policy.scopes."legacy/**".routine_ccn]
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
    config.set_scope_section(
        repo,
        config.WORKTREE_SOURCE,
        config.JUDGE_KEY,
        {config.JUDGE_ENABLED_KEY: True},
    )
    opted_in_check = _binary_check(repo, "tool.judge")

    assert [
        (default_check.status, "judge-free" in default_check.detail),
        (opted_in_check.status, "opted in" in opted_in_check.detail),
    ] == [("warn", True), ("fail", True)]


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
    real_find_tool = doctor.find_tool
    monkeypatch.setattr(
        doctor,
        "find_tool",
        lambda name: None if name == "npm" else real_find_tool(name),
    )

    checks = doctor._binary_checks(tmp_path)
    npm = next(check for check in checks if check.name == "tool.npm")

    assert npm.status == "warn"
    assert "no serve web checkJs sources" in npm.detail


def test_doctor_uses_configured_external_speech_backend(tmp_path, monkeypatch):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {
            config.SAY_BACKEND_KEY: "external",
            config.SAY_COMMAND_KEY: "tts-engine --wav",
        },
    )
    monkeypatch.setattr(doctor, "find_tool", lambda name: f"/tools/{name}")

    checks = doctor._binary_checks(tmp_path)
    tts = next(check for check in checks if check.name == "tool.tts")

    assert tts.status == "ok"
    assert "tts-engine -> /tools/tts-engine" in tts.detail
    assert "optional external speech backend" in tts.detail
