"""Agent wrapper extension entry-point contracts."""

import csv
import io
import os
import shutil
import subprocess
import tomllib
import zipfile
from pathlib import Path

import pytest

from spice.agent import shellhook
from spice.errors import SpiceError

SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow
PROJECT_ROOT = Path(__file__).resolve().parent.parent
EXTENSION_FIXTURE_ROOT = PROJECT_ROOT / "fixtures" / "spiceextensionfixture"


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
    wheel = _build_extension_fixture_wheel(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))
    _write_agent_wrapper_config(tmp_path, order=["toy-wrapper"], groups={})
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    toy_wrapper_bin = bin_dir / "toy-wrapper-bin"
    toy_wrapper_bin.write_text(
        f'#!/bin/sh\nprintf \'toy:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    toy_wrapper_bin.chmod(0o755)

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


def test_agent_wrapper_lines_rejects_entry_point_shadowing_configured_group(
    tmp_path, monkeypatch
):
    wheel = _build_extension_fixture_wheel(tmp_path)
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


def _toml_key(value: str) -> str:
    if shellhook.CONFIG_NAME_RE.fullmatch(value):
        return value
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _expected_wrapper_lines(wrapper: str, selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  {wrapper} {selector} "$@"', "}"])
    return lines


def _build_extension_fixture_wheel(tmp_path: Path) -> Path:
    pyproject = tomllib.loads(
        (EXTENSION_FIXTURE_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    )
    project = pyproject["project"]
    normalized_name = str(project["name"]).replace("-", "_")
    version = str(project["version"])
    dist_info = f"{normalized_name}-{version}.dist-info"
    wheel = tmp_path / f"{normalized_name}-{version}-py3-none-any.whl"

    records: list[str] = []
    with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for source in sorted((EXTENSION_FIXTURE_ROOT / "src").rglob("*.py")):
            _write_extension_wheel_file(
                archive,
                records,
                source.relative_to(EXTENSION_FIXTURE_ROOT / "src").as_posix(),
                source.read_bytes(),
            )
        _write_extension_wheel_file(
            archive,
            records,
            f"{dist_info}/METADATA",
            _extension_metadata_text(project).encode(),
        )
        _write_extension_wheel_file(
            archive,
            records,
            f"{dist_info}/WHEEL",
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        )
        _write_extension_wheel_file(
            archive,
            records,
            f"{dist_info}/entry_points.txt",
            _extension_entry_points_text(project["entry-points"]).encode(),
        )
        archive.writestr(
            f"{dist_info}/RECORD",
            _extension_record_text(records, f"{dist_info}/RECORD"),
        )
    return wheel


def _write_extension_wheel_file(
    archive: zipfile.ZipFile, records: list[str], name: str, data: bytes
) -> None:
    archive.writestr(name, data)
    records.append(name)


def _extension_metadata_text(project: dict[str, object]) -> str:
    return "\n".join(
        [
            "Metadata-Version: 2.3",
            f"Name: {project['name']}",
            f"Version: {project['version']}",
            f"Summary: {project['description']}",
            f"Requires-Python: {project['requires-python']}",
            "",
        ]
    )


def _extension_entry_points_text(groups: dict[str, dict[str, str]]) -> str:
    lines: list[str] = []
    for group_name, entries in groups.items():
        lines.append(f"[{group_name}]")
        lines.extend(f"{name} = {target}" for name, target in entries.items())
        lines.append("")
    return "\n".join(lines)


def _extension_record_text(records: list[str], record_name: str) -> str:
    output = io.StringIO()
    writer = csv.writer(output, lineterminator="\n")
    for name in records:
        writer.writerow([name, "", ""])
    writer.writerow([record_name, "", ""])
    return output.getvalue()


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
