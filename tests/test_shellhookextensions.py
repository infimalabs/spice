"""Agent wrapper extension entry-point contracts."""

import io
import json
import os
import shlex
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import shellhook, wrap
from spice.errors import SpiceError
from tests.test_extensionhelpers import build_fixture_wheel

SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
TOY_WRAPPER_ENTRY_POINTS = {
    shellhook.WRAPPER_ENTRY_POINT_GROUP: {
        "toy-wrapper": "spiceextensionwrapper:toy_wrapper_spec",
    }
}


def test_agent_wrapper_lines_keep_builtin_and_configured_groups_compatible(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common", "repo-tools"],
        groups={
            "common": {"wrap": ["grep"]},
            "repo-tools": {
                "pre-commit": {"argv": ["spice", "dev", "pre-commit"]},
            },
        },
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == [
        *_expected_wrapper_lines("wrap", ["grep"]),
        "",
        "pre-commit() {",
        '  spice dev pre-commit "$@"',
        "}",
    ]


def test_agent_wrapper_lines_loads_selected_entry_point_wrapper_from_fixture_wheel(
    tmp_path, monkeypatch
):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    wheel = build_fixture_wheel(
        tmp_path,
        entry_points=TOY_WRAPPER_ENTRY_POINTS,
    )
    monkeypatch.syspath_prepend(str(wheel))
    _write_agent_wrapper_config(tmp_path, order=["toy-wrapper"], groups={})
    trace = tmp_path / "trace.log"
    bin_dir = _write_toy_wrapper_bin(tmp_path)

    wrapper_lines = shellhook.render_agent_wrapper_lines(tmp_path)
    completed = subprocess.run(
        [bash, "-c", "\n".join([*wrapper_lines, "toy-wrapper alpha beta"])],
        check=False,
        env={
            "PATH": str(bin_dir)
            + os.pathsep
            + os.environ.get("PATH", ""),  # env-policy: allow
            SHELL_TRACE_ENV: str(trace),
        },
        text=True,
        capture_output=True,
    )

    assert wrapper_lines == [
        "",
        "toy-wrapper() {",
        '  toy-wrapper-bin --from-entry-point "$@"',
        "}",
    ]
    assert completed.returncode == 0, _completed_process_detail(completed, trace)
    assert "toy:--from-entry-point alpha beta" in _trace_lines(
        trace, expected_prefix="toy:"
    )


def test_agent_run_yields_rtk_rewrite_to_selected_extension_wrapper(
    tmp_path, monkeypatch
):
    bash = shutil.which("bash")
    if bash is None:
        pytest.skip("bash is not installed")
    wheel = build_fixture_wheel(
        tmp_path,
        entry_points=TOY_WRAPPER_ENTRY_POINTS,
    )
    monkeypatch.syspath_prepend(str(wheel))
    _write_agent_wrapper_config(
        tmp_path,
        order=["common", "toy-wrapper"],
        groups={},
    )
    rtk = _write_fake_rewriting_rtk(tmp_path)
    (tmp_path / "spice.toml").write_text(
        f"[rtk]\nexecutable = {json.dumps(str(rtk))}\n",
        encoding="utf-8",
    )
    trace = tmp_path / "trace.log"
    bin_dir = _write_toy_wrapper_bin(tmp_path)
    base_env = dict(os.environ)  # env-policy: allow
    base_env["PATH"] = str(bin_dir) + os.pathsep + base_env.get("PATH", "")
    base_env[SHELL_TRACE_ENV] = str(trace)
    steering = shellhook.apply_shell_steering_environment(
        tmp_path,
        base_env=base_env,
    )
    for name, value in steering.items():
        monkeypatch.setenv(name, value)
    monkeypatch.chdir(tmp_path)

    assert shellhook.rtk_rewrite_yield_selectors(tmp_path) == frozenset({"toy-wrapper"})
    exit_code = wrap.run_agent_command(
        tmp_path,
        [bash, "-c", "toy-wrapper alpha beta"],
        stderr=io.StringIO(),
    )
    control = wrap.build_agent_run_command(
        [bash, "-c", "rg -n needle"],
        repo_root=tmp_path,
        rewrite_rtk=True,
    )

    assert exit_code == 0
    assert _trace_lines(trace, expected_prefix="toy:") == [
        "toy:--from-entry-point alpha beta"
    ]
    assert control == [
        bash,
        "-c",
        f"{shlex.quote(str(rtk))} grep -n needle",
    ]


def test_agent_wrapper_lines_rejects_entry_point_shadowing_configured_group(
    tmp_path, monkeypatch
):
    wheel = build_fixture_wheel(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))
    _write_agent_wrapper_config(
        tmp_path,
        order=["spice-dev"],
        groups={
            "spice-dev": {
                "pre-commit": {"argv": ["spice", "dev", "pre-commit"]},
            },
        },
    )

    with pytest.raises(SpiceError) as exc_info:
        shellhook.render_agent_wrapper_lines(tmp_path)

    assert str(exc_info.value) == (
        f"wrappers (source=pyproject path={tmp_path / 'pyproject.toml'}): "
        "spice shell hook: wrapper group 'spice-dev' is configured by both "
        "tool.spice.wrappers.spice-dev and spice.wrappers entry point spice-dev"
    )


def _write_agent_wrapper_config(
    repo: Path, *, order: list[str] | None, groups: dict[str, dict[str, object]]
) -> None:
    lines: list[str] = []
    if order is not None:
        wrappers_value = "[" + ", ".join(f'"{name}"' for name in order) + "]"
        lines.extend(
            [
                "[tool.spice.agent]",
                f"wrappers = {wrappers_value}",
            ]
        )
    for group_name, entries in groups.items():
        lines.extend(["", f"[tool.spice.wrappers.{group_name}]"])
        for wrapper, value in entries.items():
            if isinstance(value, dict):
                command = value["argv"]
                lines.append(
                    f"{_toml_key(wrapper)} = {{ argv = ["
                    + ", ".join(f'"{word}"' for word in command)
                    + "] }"
                )
                continue
            lines.append(
                f"{_toml_key(wrapper)} = ["
                + ", ".join(f'"{selector}"' for selector in value)
                + "]"
            )
    (repo / "pyproject.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")


def _write_fake_rewriting_rtk(repo: Path) -> Path:
    script = repo / "fake-rtk"
    script.write_text(
        "#!/bin/sh\n"
        "shift 2\n"
        'case "$*" in\n'
        '"toy-wrapper"*) echo "rtk toy-wrapper alpha beta"; exit 3 ;;\n'
        '"rg -n needle") echo "rtk grep -n needle"; exit 3 ;;\n'
        "esac\n"
        "exit 1\n",
        encoding="utf-8",
    )
    script.chmod(0o755)
    return script


def _write_toy_wrapper_bin(repo: Path) -> Path:
    bin_dir = repo / "bin"
    bin_dir.mkdir()
    toy_wrapper_bin = bin_dir / "toy-wrapper-bin"
    toy_wrapper_bin.write_text(
        f'#!/bin/sh\nprintf \'toy:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    toy_wrapper_bin.chmod(0o755)
    return bin_dir


def _toml_key(value: str) -> str:
    if shellhook.CONFIG_NAME_RE.fullmatch(value):
        return value
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _expected_wrapper_lines(wrapper: str, selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  {wrapper} {selector} "$@"', "}"])
    return lines


def _trace_lines(trace: Path, *, expected_prefix: str) -> list[str]:
    lines = trace.read_text(encoding="utf-8").splitlines() if trace.exists() else []
    assert any(expected_prefix in line for line in lines)
    return lines


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
