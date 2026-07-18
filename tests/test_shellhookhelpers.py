"""Shared fixtures for shell-hook routing and startup tests."""

import json
import shlex
import subprocess
import time
from pathlib import Path

from spice.agent import shellhook

SHELL_TRACE_ENV = "SPICE_TEST_TRACE"  # env-policy: allow


def write_spice_product_shape(repo: Path) -> None:
    for relative in (
        Path("spice") / "__main__.py",
        Path("spice") / "cli" / "entry.py",
        Path("spice") / "agent" / "wrap.py",
    ):
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("# test spice product shape\n", encoding="utf-8")


def init_git_repo(repo: Path) -> None:
    subprocess.run(
        ["git", "init"],
        cwd=repo,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def write_fake_rewriting_rtk(repo: Path) -> Path:
    script = repo / "fake-rtk"
    script.write_text(
        "#!/bin/sh\n"
        "shift 2\n"
        'case "$*" in\n'
        '"pytest"* | "python -m pytest"*) echo "rtk pytest"; exit 3 ;;\n'
        '"rg -n needle") echo "rtk grep -n needle"; exit 3 ;;\n'
        "esac\n"
        "exit 1\n",
        encoding="utf-8",
    )
    script.chmod(0o755)
    return script


def write_rtk_config(repo: Path, executable: str) -> None:
    (repo / "spice.toml").write_text(
        f"[rtk]\nexecutable = {json.dumps(executable)}\n",
        encoding="utf-8",
    )


def write_agent_wrapper_config(
    repo: Path,
    *,
    order: list[str] | None,
    groups: dict[str, dict[str, object] | bool],
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
    disabled_groups = [name for name, entries in groups.items() if entries is False]
    if disabled_groups:
        lines.extend(["", "[tool.spice.wrappers]"])
        lines.extend(f"{toml_key(name)} = false" for name in disabled_groups)
    for group_name, entries in groups.items():
        if entries is False:
            continue
        assert isinstance(entries, dict)
        lines.extend(["", f"[tool.spice.wrappers.{group_name}]"])
        for wrapper, value in entries.items():
            if value is False:
                lines.append(f"{toml_key(wrapper)} = false")
                continue
            if isinstance(value, dict):
                command = value["argv"]
                lines.append(
                    f"{toml_key(wrapper)} = {{ argv = ["
                    + ", ".join(f'"{word}"' for word in command)
                    + "] }"
                )
                continue
            lines.append(
                f"{toml_key(wrapper)} = ["
                + ", ".join(f'"{selector}"' for selector in value)
                + "]"
            )
    (repo / "pyproject.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")


def toml_key(value: str) -> str:
    if shellhook.CONFIG_NAME_RE.fullmatch(value):
        return value
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def expected_project_common_with_pytest_wrapper_lines() -> list[str]:
    return [
        *expected_wrapper_lines("wrap", ["run", "grep", "find", "git"]),
        *expected_python_module_wrapper_lines(["pytest"]),
    ]


def builtin_common_wrapper_lines(
    rtk_executable: str = "rtk", *, driver_name: str = "codex"
) -> list[str]:
    command_word = shellhook.shell_command_word(rtk_executable)
    lines = [
        "",
        "rtk() {",
        '  if [ "${1-}" = grep ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        --files|--type|--type=*|--no-heading|-g|--glob|--glob=*|*/)",
        "          shift",
        '          command rg "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
        '  if [ "${1-}" = find ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        -name|-iname|-type|-maxdepth) ;;",
        "        -*|'('|')'|'!')",
        "          shift",
        '          command find "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
        '  if [ "${1-}" = git ]; then',
        '    for _spice_word in "$@"; do',
        '      case "$_spice_word" in',
        "        --first-parent|--check|--name-status|--name-only)",
        "          shift",
        '          command git "$@"',
        "          return",
        "          ;;",
        "      esac",
        "    done",
        "  fi",
    ]
    if driver_name == "codex":
        lines.extend(
            [
                *shellhook.grep_search_operand_route_guard_lines(
                    f"{command_word} grep -E -r"
                ),
                '  if [ "${1-}" = grep ]; then',
                "    shift",
                f'    command {command_word} grep -E "$@"',
                "    return",
                "  fi",
            ]
        )
    lines.extend([f'  command {command_word} "$@"', "}"])
    if driver_name == "codex":
        lines.extend(
            [
                "",
                "grep() {",
                '  for _spice_word in "$@"; do',
                '    case "$_spice_word" in',
                "      -E|-F|-P|-G)",
                '        command grep "$@"',
                "        return",
                "        ;;",
                "    esac",
                "  done",
                '  command grep -E "$@"',
                "}",
            ]
        )
    return lines


def expected_wrapper_lines(wrapper: str, selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  {wrapper} {selector} "$@"', "}"])
    return lines


def expected_python_module_wrapper_lines(selectors: list[str]) -> list[str]:
    lines: list[str] = []
    for selector in selectors:
        lines.extend(["", f"{selector}() {{", f'  python -m {selector} "$@"', "}"])
    return lines


def fake_spice_executable(tmp_path: Path, *, run_agent_commands: bool = False) -> Path:
    path = tmp_path / "installed" / "bin" / "spice"
    path.parent.mkdir(parents=True, exist_ok=True)
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    agent_run_exec = (
        (
            'if [ "$1" = "agent" ] && [ "$2" = "run" ] '
            '&& [ "$3" = "--" ]; then\n'
            "  shift 3\n"
            '  if [ "$2" = "-c" ] || [ "$2" = "-lc" ]; then\n'
            f"    export ZDOTDIR={shlex.quote(str(static_hook_dir))}\n"
            f"    export BASH_ENV={shlex.quote(str(static_hook_dir / shellhook.BASH_HOOK_NAME))}\n"
            "  fi\n"
            '  exec "$@"\n'
            "fi\n"
        )
        if run_agent_commands
        else ""
    )
    path.write_text(
        (
            "#!/bin/sh\n"
            "printf 'fake:%s:%s:%s\\n' "
            f'"${{{shellhook.ZDOTDIR_ENV}-unset}}" '
            f'"${{{shellhook.BASH_ENV_ENV}-unset}}" '
            '"$*" '
            f'>> "${{{SHELL_TRACE_ENV}}}"\n'
            f"{agent_run_exec}"
        ),
        encoding="utf-8",
    )
    path.chmod(0o755)
    return path


def trace_lines(trace: Path, *, expected_prefix: str) -> list[str]:
    return eventually(
        lambda: (
            trace.read_text(encoding="utf-8").splitlines() if trace.exists() else []
        ),
        contains=expected_prefix,
    )


def completed_process_detail(
    completed: subprocess.CompletedProcess, trace: Path
) -> str:
    trace_text = trace.read_text(encoding="utf-8") if trace.exists() else "<missing>"
    return (
        f"returncode={completed.returncode}\n"
        f"stdout={completed.stdout!r}\n"
        f"stderr={completed.stderr!r}\n"
        f"trace={trace_text!r}"
    )


def eventually(factory, *, contains: str):
    deadline = time.monotonic() + 2.0
    latest = factory()
    while time.monotonic() < deadline:
        if contains_value(latest, contains):
            return latest
        time.sleep(0.05)
        latest = factory()
    return latest


def contains_value(value, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    return any(needle in item for item in value)
