from __future__ import annotations

import json
import os
import shutil
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from spice import extensions as extension_loader
from spice.agent import shellhook
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV, driver_for, select_driver
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.hooks import doctor
from spice.studies import cli as studies_cli
from tests.test_extensionhelpers import (
    FilteredExtensionDistribution,
    build_fixture_distribution,
)

SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow


def test_declared_extension_fixture_wheel_runs_driver_study_and_wrapper(
    tmp_path, monkeypatch, capsys
):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    wheel, distribution = build_fixture_distribution(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))
    _use_extension_entries(
        monkeypatch,
        distribution,
        {
            extension_loader.SPICE_DRIVER_ENTRY_POINT_GROUP: {"toy"},
            extension_loader.SPICE_STUDY_ENTRY_POINT_GROUP: {"toy-study"},
            extension_loader.SPICE_WRAPPER_ENTRY_POINT_GROUP: {"toy-wrapper"},
        },
    )
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    _write_agent_config(tmp_path, driver="toy", wrappers=["toy-wrapper"])
    _write_toy_wrapper_bin(tmp_path)

    parser = build_parser()
    driver = select_driver("toy")
    driver_args = parser.parse_args(["config", "agent", "--driver", "toy"])
    study_args = parser.parse_args(["study", "toy-study", "src/app.py", "--json"])
    wrapper_lines = shellhook.render_agent_wrapper_lines(tmp_path)

    assert driver.name == "toy"
    assert driver.build_exec_command(
        repo_root=tmp_path,
        prompt="hello",
        thread_id="thread-1",
        model="toy-large",
    ) == ["toy-agent", "exec", "--thread", "thread-1", "--model", "toy-large", "hello"]
    assert driver_for(tmp_path).name == "toy"
    assert driver_args.driver == "toy"
    assert study_args.func(study_args) == 0
    assert json.loads(capsys.readouterr().out) == {
        "artifactKind": "spice.study.toy-study",
        "result": {"paths": ["src/app.py"], "study": "toy"},
    }
    assert wrapper_lines == [
        "",
        "toy-wrapper() {",
        '  toy-wrapper-bin --from-entry-point "$@"',
        "}",
    ]
    completed = subprocess.run(
        [bash, "-c", "\n".join([*wrapper_lines, "toy-wrapper alpha beta"])],
        check=False,
        env={
            "PATH": str(tmp_path / "bin")
            + os.pathsep
            + os.environ.get("PATH", ""),  # env-policy: allow
            SHELL_TRACE_ENV: str(tmp_path / "trace.log"),
        },
        text=True,
        capture_output=True,
    )
    assert completed.returncode == 0, _completed_process_detail(
        completed, tmp_path / "trace.log"
    )
    assert "toy:--from-entry-point alpha beta" in (tmp_path / "trace.log").read_text(
        encoding="utf-8"
    )


@pytest.mark.parametrize(
    ("group", "names", "trigger", "shadow_name", "expected"),
    [
        (
            extension_loader.SPICE_DRIVER_ENTRY_POINT_GROUP,
            {"codex", "toy"},
            "driver",
            "codex",
            "shadows built-in",
        ),
        (
            extension_loader.SPICE_STUDY_ENTRY_POINT_GROUP,
            {"file-loc", "toy-study"},
            "study",
            "file-loc",
            "shadows built-in",
        ),
        (
            extension_loader.SPICE_WRAPPER_ENTRY_POINT_GROUP,
            {"spice-dev", "toy-wrapper"},
            "wrapper",
            "spice-dev",
            "configured by both tool.spice.wrappers.spice-dev "
            "and spice.wrappers entry point spice-dev",
        ),
    ],
)
def test_declared_extension_fixture_shadow_entries_fail_loudly_by_surface(
    tmp_path,
    monkeypatch,
    group: str,
    names: set[str],
    trigger: str,
    shadow_name: str,
    expected: str,
):
    wheel, distribution = build_fixture_distribution(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))
    _use_extension_entries(monkeypatch, distribution, {group: names})
    _assert_shared_loader_shadow(group, distribution, shadow_name)
    _write_agent_config(tmp_path, wrappers=["spice-dev"])
    _write_agent_wrapper_group(tmp_path, "spice-dev")

    with pytest.raises(SpiceError) as exc_info:
        _trigger_shadow(trigger, tmp_path)

    message = str(exc_info.value)
    assert group in message
    assert shadow_name in message
    assert expected in message


def _assert_shared_loader_shadow(group: str, distribution, shadow_name: str) -> None:
    filtered = FilteredExtensionDistribution(distribution, {group: {shadow_name}})
    with pytest.raises(SpiceError) as exc_info:
        extension_loader.extension_entry_points(
            group,
            built_in_names=[shadow_name],
            distributions=[filtered],
        )

    message = str(exc_info.value)
    assert group in message
    assert shadow_name in message
    assert "shadows built-in" in message


def test_doctor_namespace_guard_fails_when_second_checkout_is_on_sys_path(
    tmp_path, monkeypatch
):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    extra_checkout = tmp_path / "second-checkout"
    (extra_checkout / "spice").mkdir(parents=True)
    monkeypatch.syspath_prepend(str(extra_checkout))

    check = doctor._spice_namespace_portions_check(tmp_path)
    report = doctor.DoctorReport(repo_root=tmp_path, checks=[check], fixes=[])

    assert report.failed
    assert check.name == "runtime.spice-namespace"
    assert check.status == "fail"
    assert "conflicting spice namespace portions" in check.detail
    assert str(extra_checkout.resolve()) in check.detail
    assert "FAIL runtime.spice-namespace" in report.render()


def _use_extension_entries(
    monkeypatch,
    distribution,
    names_by_group: dict[str, set[str]],
) -> None:
    filtered = FilteredExtensionDistribution(distribution, names_by_group)
    monkeypatch.setattr(
        extension_loader.metadata,
        "distributions",
        lambda: [filtered],
    )


def _trigger_shadow(trigger: str, repo_root: Path) -> None:
    actions: dict[str, Callable[[], object]] = {
        "driver": lambda: select_driver("toy"),
        "study": build_parser,
        "wrapper": lambda: shellhook.render_agent_wrapper_lines(repo_root),
    }
    actions[trigger]()


def _write_agent_config(
    repo_root: Path, *, driver: str | None = None, wrappers: list[str]
) -> None:
    lines = ["[tool.spice.agent]"]
    if driver is not None:
        lines.append(f'driver = "{driver}"')
    wrappers_value = "[" + ", ".join(f'"{wrapper}"' for wrapper in wrappers) + "]"
    lines.append(f"wrappers = {wrappers_value}")
    (repo_root / "pyproject.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")


def _write_agent_wrapper_group(repo_root: Path, group_name: str) -> None:
    with (repo_root / "pyproject.toml").open("a", encoding="utf-8") as handle:
        handle.write(
            "\n"
            f"[tool.spice.wrappers.{group_name}]\n"
            'pre-commit = { argv = ["spice", "dev", "pre-commit"] }\n'
        )


def _write_toy_wrapper_bin(repo_root: Path) -> None:
    bin_dir = repo_root / "bin"
    bin_dir.mkdir()
    toy_wrapper_bin = bin_dir / "toy-wrapper-bin"
    toy_wrapper_bin.write_text(
        f'#!/bin/sh\nprintf \'toy:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    toy_wrapper_bin.chmod(0o755)


def _completed_process_detail(
    completed: subprocess.CompletedProcess, trace: Path
) -> str:
    trace_text = trace.read_text(encoding="utf-8") if trace.exists() else "<missing>"
    return (
        f"returncode={completed.returncode}\n"
        f"stdout={completed.stdout!r}\n"
        f"stderr={completed.stderr!r}\n"
        f"trace={trace_text!r}"
    )
