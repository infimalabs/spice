"""Doctor checks: first-run gaps, fixable generated state, and dirty trees."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice import config
from spice.hooks import doctor
from spice.hooks.install import hooks_dir, install_hooks_for_repo
from spice.studies.walk import staged_paths, tracked_paths


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
    config.update_section(
        tmp_path,
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
