"""Agent shell wrapper line rendering and live dispatch."""

import os
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import driver as agent_driver
from spice.agent import shellhook
from spice.errors import SpiceError
from tests.test_shellhookhelpers import (
    SHELL_TRACE_ENV,
    builtin_common_wrapper_lines,
    completed_process_detail,
    expected_project_common_with_pytest_wrapper_lines,
    expected_python_module_wrapper_lines,
    expected_wrapper_lines,
    trace_lines,
    write_agent_wrapper_config,
    write_rtk_config,
)


def test_agent_wrapper_lines_adds_ordered_agent_wrapper_functions(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={"common": {"wrap": ["grep", "find", "git"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == expected_wrapper_lines(
        "wrap", ["grep", "find", "git"]
    )


@pytest.mark.parametrize("driver_name", ["codex", "claude"])
def test_agent_wrapper_lines_scopes_builtin_common_default_by_driver(
    tmp_path, monkeypatch, driver_name
):
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, driver_name)

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == builtin_common_wrapper_lines(driver_name=driver_name)


def test_agent_wrapper_lines_explicit_common_group_inherits_builtin_default(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["common"],
        groups={},
    )

    assert (
        shellhook.render_agent_wrapper_lines(tmp_path) == builtin_common_wrapper_lines()
    )


@pytest.mark.parametrize(
    ("driver_name", "route_head", "route_command", "direct_name", "direct_arg"),
    (
        ("codex", "scan", "scanner", "codex-only", "codex"),
        ("claude", "view", "viewer", "claude-only", "claude"),
    ),
)
def test_wrapper_group_direct_and_match_route_scopes_share_driver_selection(
    tmp_path,
    monkeypatch,
    driver_name,
    route_head,
    route_command,
    direct_name,
    direct_arg,
):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.agent]\n"
        'wrappers = ["tools"]\n'
        "\n"
        "[tool.spice.wrappers.tools]\n"
        'scopes = { drivers = ["CODEX", "claude"] }\n'
        "\n"
        "[tool.spice.wrappers.tools.toolbox]\n"
        'argv = ["toolbox"]\n'
        'scopes = { drivers = ["claude", "codex"] }\n'
        "match = [\n"
        '  { head = "scan", argv = ["scanner"], '
        'scopes = { drivers = ["codex"] } },\n'
        '  { head = "view", argv = ["viewer"], '
        'scopes = { drivers = ["claude"] } },\n'
        "]\n"
        "\n"
        "[tool.spice.wrappers.tools.codex-only]\n"
        'argv = ["runner", "codex"]\n'
        'scopes = { drivers = ["codex"] }\n'
        "\n"
        "[tool.spice.wrappers.tools.claude-only]\n"
        'argv = ["runner", "claude"]\n'
        'scopes = { drivers = ["claude"] }\n',
        encoding="utf-8",
    )
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, driver_name)

    lines = shellhook.render_agent_wrapper_lines(tmp_path)

    assert lines == [
        "",
        "toolbox() {",
        f'  if [ "${{1-}}" = {route_head} ]; then',
        "    shift",
        f'    command {route_command} "$@"',
        "    return",
        "  fi",
        '  command toolbox "$@"',
        "}",
        "",
        f"{direct_name}() {{",
        f'  runner {direct_arg} "$@"',
        "}",
    ]


def test_agent_wrapper_lines_project_common_group_replaces_packaged_default(
    tmp_path,
):
    write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={"common": {"wrap": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == expected_wrapper_lines(
        "wrap", ["grep"]
    )


def test_agent_wrapper_lines_project_common_can_add_pytest_wrapper(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=None,
        groups={
            "common": {
                "wrap": ["run", "grep", "find", "git"],
                "pytest": {"argv": ["python", "-m", "pytest"]},
            }
        },
    )

    assert (
        shellhook.render_agent_wrapper_lines(tmp_path)
        == expected_project_common_with_pytest_wrapper_lines()
    )


def test_repository_common_wrapper_preserves_native_git_fidelity_routes():
    lines = shellhook.render_agent_wrapper_lines(Path.cwd())

    assert '  if [ "${1-}" = git ]; then' in lines
    assert "        --first-parent|--check|--name-status|--name-only)" in lines
    assert '          command git "$@"' in lines


def test_repository_spice_dev_wrapper_redirects_bare_task():
    lines = shellhook.render_agent_wrapper_lines(Path.cwd())

    start = lines.index("task() {")
    assert lines[start - 1 : start + 3] == [
        "",
        "task() {",
        '  spice task "$@"',
        "}",
    ]


@pytest.mark.parametrize("shell_name", ["zsh", "bash"])
def test_spice_checkout_task_wrapper_forwards_and_preserves_native_escape(
    tmp_path, shell_name
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name in ("spice", "task"):
        tool = bin_dir / name
        tool.write_text(
            f'#!/bin/sh\nprintf \'{name}:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
            encoding="utf-8",
        )
        tool.chmod(0o755)
    script = "\n".join(
        [
            "set -u",
            *shellhook.render_agent_wrapper_lines(Path.cwd()),
            "task status --limit 3",
            "command task native status",
        ]
    )

    completed = subprocess.run(
        [shell, "-c", script],
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

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    assert trace_lines(trace, expected_prefix="task:native status") == [
        "spice:task status --limit 3",
        "task:native status",
    ]


def test_agent_wrapper_lines_accepts_direct_argv_wrapper(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["tests"],
        groups={"tests": {"pytest": {"argv": ["python", "-m", "pytest"]}}},
    )

    assert shellhook.render_agent_wrapper_lines(
        tmp_path
    ) == expected_python_module_wrapper_lines(["pytest"])


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


def test_agent_wrapper_lines_renders_head_only_route_guard(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ head = "scan", argv = ["toolbox", "scan", "-E"] }]',
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == [
        "",
        "toolbox() {",
        '  if [ "${1-}" = scan ]; then',
        "    shift",
        '    command toolbox scan -E "$@"',
        "    return",
        "  fi",
        '  command toolbox "$@"',
        "}",
    ]


def test_agent_wrapper_lines_rejects_route_lacking_head_and_flags(tmp_path):
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ argv = ["viewer"] }]',
    )

    with pytest.raises(SpiceError, match=r"match\[0\] needs a head or flags"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_rejects_self_intercepting_wrapper_lacking_match(tmp_path):
    write_agent_wrapper_config(
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


@pytest.mark.parametrize("identity_kind", ["builtin", "basename", "absolute"])
def test_rtk_wrapper_dispatches_configured_identity_in_live_zsh(
    tmp_path, identity_kind
):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    executable = {
        "builtin": "rtk",
        "basename": "alternate-rtk",
        "absolute": str(tmp_path / "Spice Tools" / "rtk companion"),
    }[identity_kind]
    write_rtk_config(tmp_path, executable)
    resolved_tool = (
        bin_dir / executable if identity_kind != "absolute" else Path(executable)
    )
    resolved_tool.parent.mkdir(parents=True, exist_ok=True)
    resolved_tool.write_text(
        f'#!/bin/sh\nprintf \'resolved:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    resolved_tool.chmod(0o755)
    for name in ("rg", "find", "git"):
        tool = bin_dir / name
        tool.write_text(
            f'#!/bin/sh\nprintf \'{name}:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
            encoding="utf-8",
        )
        tool.chmod(0o755)
    wrapper_lines = shellhook.render_agent_wrapper_lines(tmp_path)
    assert wrapper_lines == builtin_common_wrapper_lines(executable)
    script = "\n".join(
        [
            "set -u",
            *wrapper_lines,
            "rtk grep --files src",
            "rtk grep -g '*.md' needle docs",
            "rtk grep --glob '*.toml' needle .",
            "rtk grep --glob='*.py' needle src",
            "rtk grep needle docs/design/",
            "rtk grep needle src",
            "rtk grep -F 'a|b' src",
            "rtk grep -G 'a\\|b' src",
            "rtk find src -name '*.py' -print",
            "rtk find src \\( -name '*.py' -o -name '*.md' \\)",
            "rtk git log --first-parent v1..HEAD",
            "rtk git show --name-status HEAD",
            "rtk git diff --check",
            "rtk git diff --name-only HEAD~1 HEAD",
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

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    lines = trace_lines(trace, expected_prefix="rg:")
    assert lines == [
        "rg:--files src",
        "rg:-g *.md needle docs",
        "rg:--glob *.toml needle .",
        "rg:--glob=*.py needle src",
        "rg:needle docs/design/",
        "resolved:grep -E -r needle src",
        "resolved:grep -E -r -F a|b src",
        "resolved:grep -E -r -G a\\|b src",
        "find:src -name *.py -print",
        "find:src ( -name *.py -o -name *.md )",
        "git:log --first-parent v1..HEAD",
        "git:show --name-status HEAD",
        "git:diff --check",
        "git:diff --name-only HEAD~1 HEAD",
        "resolved:",
    ]
    assert lines[0] != lines[1]


@pytest.mark.parametrize("shell_name", ["zsh", "bash"])
def test_rtk_grep_route_adds_recursive_mode_for_search_operands(tmp_path, shell_name):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    tool = bin_dir / "rtk"
    tool.write_text(
        f'#!/bin/sh\nprintf \'rtk:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    tool.chmod(0o755)
    script = "\n".join(
        [
            "set -u",
            *shellhook.render_agent_wrapper_lines(tmp_path),
            "rtk grep needle one-dir",
            "rtk grep needle one-dir two-dir",
            "rtk grep needle source.txt",
            "rtk grep -n needle source.txt",
            "rtk grep -A 2 needle source.txt",
            "rtk grep -C 2 needle source.txt",
            "rtk grep --context needle source.txt",
            "rtk grep -e needle source.txt",
            "rtk grep needle",
            "rtk grep -n needle",
            "rtk grep -A 2 needle",
            "rtk grep -C 2 needle",
            "rtk grep --context needle",
            "rtk grep -e needle",
        ]
    )

    completed = subprocess.run(
        [shell, "-c", script],
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

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    assert trace_lines(trace, expected_prefix="rtk:") == [
        "rtk:grep -E -r needle one-dir",
        "rtk:grep -E -r needle one-dir two-dir",
        "rtk:grep -E -r needle source.txt",
        "rtk:grep -E -r -n needle source.txt",
        "rtk:grep -E -r -A 2 needle source.txt",
        "rtk:grep -E -r -C 2 needle source.txt",
        "rtk:grep -E -r --context needle source.txt",
        "rtk:grep -E -r -e needle source.txt",
        "rtk:grep -E needle",
        "rtk:grep -E -n needle",
        "rtk:grep -E -A 2 needle",
        "rtk:grep -E -C 2 needle",
        "rtk:grep -E --context needle",
        "rtk:grep -E -e needle",
    ]


def test_pyproject_head_only_route_dispatches_in_live_zsh(tmp_path):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    _write_match_wrapper_config(
        tmp_path,
        argv='["toolbox"]',
        match='[{ head = "scan", argv = ["toolbox", "scan", "-E"] }]',
    )
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    tool = bin_dir / "toolbox"
    tool.write_text(
        f'#!/bin/sh\nprintf \'toolbox:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    tool.chmod(0o755)
    script = "\n".join(
        [
            "set -u",
            *shellhook.render_agent_wrapper_lines(tmp_path),
            "toolbox scan 'a|b' src",
            "toolbox status",
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

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    lines = trace_lines(trace, expected_prefix="toolbox:")
    assert lines == [
        "toolbox:scan -E a|b src",
        "toolbox:status",
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


@pytest.mark.parametrize(
    ("driver_name", "pattern", "expected_trace"),
    [
        (
            "codex",
            "alpha|beta",
            [
                "grep:-E alpha|beta source.txt",
                "rtk:grep -E -r alpha|beta source.txt",
            ],
        ),
        (
            "claude",
            "alpha\\|beta",
            [
                "grep:alpha\\|beta source.txt",
                "rtk:grep alpha\\|beta source.txt",
            ],
        ),
    ],
)
def test_global_grep_defaults_follow_active_driver_in_live_zsh(
    tmp_path, monkeypatch, driver_name, pattern, expected_trace
):
    zsh = shutil.which("zsh")
    if zsh is None:
        pytest.skip("zsh is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name in ("grep", "rtk"):
        tool = bin_dir / name
        tool.write_text(
            f'#!/bin/sh\nprintf \'{name}:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
            encoding="utf-8",
        )
        tool.chmod(0o755)
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, driver_name)
    script = "\n".join(
        [
            "set -u",
            *shellhook.render_agent_wrapper_lines(tmp_path),
            f"grep '{pattern}' source.txt",
            f"rtk grep '{pattern}' source.txt",
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

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    assert trace_lines(trace, expected_prefix="grep:") == expected_trace


@pytest.mark.parametrize("shell_name", ["zsh", "bash"])
def test_spice_checkout_bare_grep_defaults_to_ere_and_preserves_explicit_mode(
    tmp_path, shell_name
):
    shell = shutil.which(shell_name)
    if shell is None:
        pytest.skip(f"{shell_name} is not installed")
    trace = tmp_path / "trace.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    tool = bin_dir / "grep"
    tool.write_text(
        f'#!/bin/sh\nprintf \'grep:%s\\n\' "$*" >> "${{{SHELL_TRACE_ENV}}}"\n',
        encoding="utf-8",
    )
    tool.chmod(0o755)
    script = "\n".join(
        [
            "set -u",
            *shellhook.render_agent_wrapper_lines(Path.cwd()),
            "grep 'alpha|beta' source.txt",
            "grep -E 'alpha|beta' source.txt",
            "grep -F 'alpha|beta' source.txt",
            "grep -P 'alpha|beta' source.txt",
            "grep -G 'alpha\\|beta' source.txt",
        ]
    )

    completed = subprocess.run(
        [shell, "-c", script],
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

    assert completed.returncode == 0, completed_process_detail(completed, trace)
    assert trace_lines(trace, expected_prefix="grep:") == [
        "grep:-E alpha|beta source.txt",
        "grep:-E alpha|beta source.txt",
        "grep:-F alpha|beta source.txt",
        "grep:-P alpha|beta source.txt",
        "grep:-G alpha\\|beta source.txt",
    ]


def test_agent_wrapper_lines_honors_empty_agent_wrapper_list(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=[],
        groups={"common": {"wrap": ["grep"]}},
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == []


def test_agent_wrapper_lines_fails_loudly_for_path_wrapper_selectors(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"dash": ["/bin/sh", "sh"]}},
    )

    with pytest.raises(SpiceError, match="requires the redirector stage"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_fails_loudly_for_path_wrapper_commands(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["shells"],
        groups={"shells": {"pytest": {"argv": ["/bin/python", "-m", "pytest"]}}},
    )

    with pytest.raises(SpiceError, match="path wrapper command"):
        shellhook.render_agent_wrapper_lines(tmp_path)


def test_agent_wrapper_lines_fails_loudly_for_duplicate_wrapper_selectors(tmp_path):
    write_agent_wrapper_config(
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
