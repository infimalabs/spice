"""Agent shell wrapper line rendering and live dispatch."""

import os
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import shellhook
from spice.errors import SpiceError
from tests.test_shellhook import (
    SHELL_TRACE_ENV,
    _builtin_rtk_wrapper_lines,
    _completed_process_detail,
    _expected_project_common_with_pytest_wrapper_lines,
    _expected_python_module_wrapper_lines,
    _expected_wrapper_lines,
    _trace_lines,
    _write_agent_wrapper_config,
)


def test_agent_wrapper_lines_adds_ordered_agent_wrapper_functions(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep", "find", "git"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == _expected_wrapper_lines(
        "wrap", ["grep", "find", "git"]
    )


def test_agent_wrapper_lines_uses_builtin_common_default(tmp_path):
    assert (
        shellhook.render_agent_wrapper_lines(tmp_path) == _builtin_rtk_wrapper_lines()
    )


def test_agent_wrapper_lines_explicit_common_group_inherits_builtin_default(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={},
    )

    assert (
        shellhook.render_agent_wrapper_lines(tmp_path) == _builtin_rtk_wrapper_lines()
    )


def test_agent_wrapper_lines_project_common_group_overrides_builtin_default(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={"common": {"wrap": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == _expected_wrapper_lines(
        "wrap", ["grep"]
    )


def test_agent_wrapper_lines_project_common_can_add_pytest_wrapper(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={
            "common": {
                "wrap": ["run", "grep", "find", "git"],
                "pytest": {"argv": ["$SPICE_SHELL_HOOK_PYTHON", "-m", "pytest"]},
            }
        },
    )

    assert (
        shellhook.render_agent_wrapper_lines(tmp_path)
        == _expected_project_common_with_pytest_wrapper_lines()
    )


def test_agent_wrapper_lines_accepts_direct_argv_wrapper(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["tests"],
        groups={"tests": {"pytest": {"argv": ["python", "-m", "pytest"]}}},
    )

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == _expected_python_module_wrapper_lines(["pytest"])


def test_agent_wrapper_lines_renders_match_route_guards(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match=(
            "[\n"
            '  { head = "scan", flags = ["--fast", "--mode=*"],'
            ' argv = ["scanner"] },\n'
            '  { flags = ["-raw"], argv = ["viewer", "--raw"] },\n'
            "]"
        ),
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == [
        "",
        "toolbox() {",
        '  if [ "${1-}" = scan ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        --fast|--mode=*)",
        "          shift",
        '          command scanner "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
        '  for _spice_word in "$@"; do',
        '    case "$_spice_word" in',
        "      -raw)",
        '        command viewer --raw "$@"',
        "        return",
        "        ;;",
        "    esac",
        "  done",
        '  command toolbox "$@"',
        "}",
    ]


def test_agent_wrapper_lines_renders_absent_route_guard(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match=(
            '[{ head = "scan", absent = ["-E", "-F"],'
            ' argv = ["toolbox", "scan", "-E"] }]'
        ),
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == [
        "",
        "toolbox() {",
        '  if [ "${1-}" = scan ]; then',
        "    _spice_route=absent",
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        -E|-F)",
        "          _spice_route=",
        "          break",
        "          ;;",
        "      esac",
        "    done",
        '    if [ -n "$_spice_route" ]; then',
        "      shift",
        '      command toolbox scan -E "$@"',
        "      return",
        "    fi",
        "  fi",
        '  command toolbox "$@"',
        "}",
    ]


def test_agent_wrapper_lines_rejects_route_with_flags_and_absent(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ flags = ["-raw"], absent = ["-E"], argv = ["viewer"] }]',
    )

    with pytest.raises(SpiceError, match=r"match\[0\] takes flags or absent"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_absent_route_lacking_entries(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ absent = [], argv = ["viewer"] }]',
    )

    with pytest.raises(SpiceError, match=r"match\[0\].absent has no entries"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_self_intercepting_wrapper_lacking_match(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["tools"],
        groups={"tools": {"toolbox": {"argv": ["toolbox"]}}},
    )

    with pytest.raises(SpiceError, match="cannot intercept itself"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_unknown_direct_wrapper_keys(tmp_path):
    _write_match_wrapper_config(tmp_path, argv='["scanner"]', extra='mode = "fast"')

    with pytest.raises(SpiceError, match="has unsupported keys: mode"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_unknown_match_route_keys(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ flags = ["-raw"], argv = ["viewer"], mode = "fast" }]',
    )

    with pytest.raises(SpiceError, match=r"match\[0\] has unsupported keys: mode"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_empty_match_list(tmp_path):
    _write_match_wrapper_config(tmp_path, argv='["toolbox"]', match="[]")

    with pytest.raises(SpiceError, match="match must be a non-empty list"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_match_route_lacking_flag_entries(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ flags = [], argv = ["viewer"] }]',
    )

    with pytest.raises(SpiceError, match=r"match\[0\].flags has no entries"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_multiword_match_flag(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ flags = ["--raw extra"], argv = ["viewer"] }]',
    )

    with pytest.raises(SpiceError, match="must be a single word"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_empty_direct_wrapper_argv(tmp_path):
    _write_match_wrapper_config(tmp_path, argv="[]")

    with pytest.raises(SpiceError, match="argv has no entries"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_builtin_rtk_wrapper_dispatches_in_live_zsh(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name in ("rtk", "rg", "find"):
        tool = bin_dir / name
        tool.write_text(
            f'#!/bin/sh\nprintf \'{name}:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
            encoding="utf-8",
        )
        tool.chmod(0o755)
    script = "\n".join(
        [
            "set -u",
            *shellhook.render_agent_wrapper_lines(tmp_path),
            "rtk grep --files src",
            "rtk grep needle src",
            "rtk grep -F 'a|b' src",
            "rtk grep -E 'a|b' src",
            "rtk find src -name '*.py' -print",
            "rtk find src \\( -name '*.py' -o -name '*.md' \\)",
            "rtk",
        ]
    )

    completed = subprocess.run(
        [zsh, "-c", script],
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

    assert completed.returncode == 0, _completed_process_detail(completed, trace)
    lines = _trace_lines(trace, expected_prefix="rg:")
    assert lines == [
        "rg:--files src",
        "rtk:grep -E needle src",
        "rtk:grep -F a|b src",
        "rtk:grep -E a|b src",
        "find:src -name *.py -print",
        "find:src ( -name *.py -o -name *.md )",
        "rtk:",
    ]
    assert lines[0] != lines[1]


def test_spice_checkout_maps_bare_pre_commit_to_dev_gate():
    repo = Path(__file__).resolve().parents[1]
    lines = shellhook.render_agent_wrapper_lines(repo)

    wrapper_start = lines.index("pre-commit() {")
    assert lines[wrapper_start : wrapper_start + 3] == [
        "pre-commit() {",
        '  spice dev pre-commit "$@"',
        "}",
    ]


def test_agent_wrapper_lines_honors_empty_agent_wrapper_list(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=[],
        groups={"common": {"wrap": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == []


def test_agent_wrapper_lines_fails_loudly_for_path_wrapper_selectors(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"dash": ["/bin/sh", "sh"]}},
    )

    with pytest.raises(SpiceError, match="requires the redirector stage"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_fails_loudly_for_path_wrapper_commands(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"pytest": {"argv": ["/bin/python", "-m", "pytest"]}}},
    )

    with pytest.raises(SpiceError, match="path wrapper command"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_fails_loudly_for_duplicate_wrapper_selectors(tmp_path):
    _write_agent_wrapper_config(
        tmp_path,
        order=["base", "shells"],
        groups={"base": {"wrap": ["sh"]}, "shells": {"dash": ["sh"]}},
    )

    with pytest.raises(SpiceError, match="configured by both"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def _write_match_wrapper_config(
    repo: Path, *, argv: str, match: str | None = None, extra: str | None = None
) -> None:
    lines = [
        "[tool.spice.agent]",
        'wrappers = ["tools"]',
        "",
        "[tool.spice.wrappers.tools.toolbox]",
        f"argv = {argv}",
    ]
    if match is not None:
        lines.append(f"match = {match}")
    if extra is not None:
        lines.append(extra)
    (repo / "pyproject.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
