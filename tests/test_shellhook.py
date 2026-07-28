"""Agent wrapper routing and shell steering contracts."""

import io
import os
import shlex
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

from spice.agent import driver as agent_driver
from spice.agent import lifecycle, shellhook, wrap
from spice.agent.driver import CLAUDE_DRIVER, DRIVER
from tests.test_shellhookhelpers import (
    SHELL_TRACE_ENV,
    expected_python_module_wrapper_lines,
    expected_wrapper_lines,
    init_git_repo,
    write_agent_wrapper_config,
    write_fake_rewriting_rtk,
    write_rtk_config,
    write_spice_product_shape,
)

SHELL_HOOK_FAILURE_EXIT_CODE = 127
UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND = "spice agent " + "shell-hook"
UNSUPPORTED_AGENT_STEER_COMMAND = "spice agent " + "steer"
SCOPED_REWRITE_PROCESS_PID = 4242


def test_rtk_rewrite_protocol_accepts_current_result_pairs():
    responses = iter(
        [
            subprocess.CompletedProcess([], 0, stdout="rtk git log\n", stderr=""),
            subprocess.CompletedProcess([], 3, stdout="rtk git status\n", stderr=""),
            subprocess.CompletedProcess([], 1, stdout="", stderr=""),
        ]
    )

    def run(*_args, **_kwargs):
        return next(responses)

    assert wrap.rtk_rewrite_command_text("git", "log", run=run) == "rtk git log"
    assert wrap.rtk_rewrite_command_text("git", "status", run=run) == "rtk git status"
    assert wrap.rtk_rewrite_command_text("true", run=run) is None


@pytest.mark.parametrize(
    ("completed", "failure_class"),
    [
        (
            subprocess.CompletedProcess([], 3, stdout="", stderr=""),
            "invalid-result-pair",
        ),
        (
            subprocess.CompletedProcess([], 1, stdout="unexpected\n", stderr=""),
            "invalid-result-pair",
        ),
        (
            subprocess.CompletedProcess([], 9, stdout="unexpected\n", stderr=""),
            "unexpected-exit",
        ),
    ],
)
def test_rtk_rewrite_protocol_degrades_invalid_results_to_native(
    completed, failure_class
):
    stderr = io.StringIO()

    rewritten = wrap.rtk_rewrite_command_text(
        "git", "status", stderr=stderr, run=lambda *_args, **_kwargs: completed
    )

    assert {
        "state": "native" if rewritten is None else "rewritten",
        "warning": stderr.getvalue(),
    } == {
        "state": "native",
        "warning": (
            "spice agent run: RTK rewrite degraded to native "
            f"executable='rtk' failure={failure_class}\n"
        ),
    }


def test_wrapper_git_route_inherits_ambient_supervisor_environment(tmp_path):
    env = wrap.build_agent_run_environment(["git", "status"], repo_root=tmp_path)
    source = "ambient" if env is None else "explicit"

    assert source == "ambient"


def test_wrapper_spice_routes_inherit_ambient_env(tmp_path, monkeypatch):
    monkeypatch.setattr(
        wrap,
        "agent_run_child_worktree_environment",
        lambda *_args, **_kwargs: pytest.fail("spice route env should not be built"),
    )
    spice_env = wrap.build_agent_run_environment(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_env = wrap.build_agent_run_environment(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_env is None
    assert uv_spice_env is None


def test_wrapper_does_not_reroute_spice_commands_under_single_install(
    tmp_path,
):
    # Single-install model: spice is the installed tool, so the wrapper never
    # rewrites a spice invocation to a per-worktree `python -m spice`. Even a
    # worktree that contains spice's own source (product shape) must pass spice
    # commands through unchanged to the installed runtime on PATH.
    write_spice_product_shape(tmp_path)

    spice_command = wrap.build_agent_run_command(
        ["spice", "task", "status"], repo_root=tmp_path
    )
    uv_spice_command = wrap.build_agent_run_command(
        ["uv", "run", "spice", "task", "status"], repo_root=tmp_path
    )

    assert spice_command == ["spice", "task", "status"]
    assert uv_spice_command == ["uv", "run", "spice", "task", "status"]


def test_wrapper_rewrites_stage_one_shell_command_before_stage_two(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk git status --short"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(
        ["zsh", "-c", "git status --short"], rewrite_rtk=True
    )

    assert command == ["zsh", "-c", "rtk git status --short"]
    assert calls == [("git status --short",)]


def test_wrapper_rewrites_codex_snapshot_trailing_shell_exec(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        if args == ("git status --short",):
            return "rtk git status --short"
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    snapshot = (
        '__CODEX_SNAPSHOT_OVERRIDE_SET_0="${CODEX_THREAD_ID+x}"\n'
        "if . '.codex/shell_snapshots/thread.sh' >/dev/null 2>&1; "
        "then :; fi\n"
        "exec '/bin/zsh' -c 'git status --short'"
    )

    command = wrap.build_agent_run_command(
        ["/bin/zsh", "-c", snapshot], rewrite_rtk=True
    )

    assert command == [
        "/bin/zsh",
        "-c",
        (
            '__CODEX_SNAPSHOT_OVERRIDE_SET_0="${CODEX_THREAD_ID+x}"\n'
            "if . '.codex/shell_snapshots/thread.sh' >/dev/null 2>&1; "
            "then :; fi\n"
            "exec /bin/zsh -c 'rtk git status --short'"
        ),
    ]
    assert calls == [(snapshot,), ("git status --short",)]


def test_wrapper_rewrites_direct_agent_command_with_rtk_source_of_truth(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return "rtk grep needle"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(["rg", "needle"], rewrite_rtk=True)

    assert command == ["rtk", "grep", "needle"]
    assert calls == [("rg", "needle")]


@pytest.mark.parametrize("identity_kind", ["basename", "absolute"])
def test_configured_rtk_routes_non_shadowing_rewrite_and_yields_selected_grep(
    tmp_path, monkeypatch, identity_kind
):
    executable = (
        "alternate-rtk"
        if identity_kind == "basename"
        else str(tmp_path / "Spice Tools" / "rtk companion")
    )
    write_rtk_config(tmp_path, executable)
    calls: list[tuple[tuple[str, ...], str]] = []

    def fake_rewrite(*args: str, **kwargs) -> str:
        calls.append((args, kwargs["rtk_executable"]))
        if args == ("rg", "needle"):
            return "rtk grep needle"
        return "rtk git status"

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    direct = wrap.build_agent_run_command(
        ["rg", "needle"], repo_root=tmp_path, rewrite_rtk=True
    )
    shell = wrap.build_agent_run_command(
        ["zsh", "-c", "git status"], repo_root=tmp_path, rewrite_rtk=True
    )

    assert direct == ["rg", "needle"]
    assert shell == ["zsh", "-c", f"{shlex.quote(executable)} git status"]
    assert calls == [
        (("rg", "needle"), executable),
        (("git status",), executable),
    ]


def test_canonical_and_resolved_rtk_direct_inputs_preserve_their_identity(
    tmp_path, monkeypatch
):
    executable = str(tmp_path / "Spice Tools" / "rtk companion")
    write_rtk_config(tmp_path, executable)
    monkeypatch.setattr(
        wrap,
        "rtk_rewrite_command_text",
        lambda *_args, **_kwargs: "rtk unexpected-rewrite",
    )
    inputs = [
        ["rtk", "grep", "needle"],
        [executable, "grep", "needle"],
    ]

    outputs = [
        wrap.build_agent_run_command(
            command,
            repo_root=tmp_path,
            rewrite_rtk=True,
        )
        for command in inputs
    ]

    assert outputs == inputs


def test_run_agent_command_yields_rtk_pytest_rewrite_to_repository_wrapper(
    tmp_path, monkeypatch
):
    rtk = write_fake_rewriting_rtk(tmp_path)
    write_rtk_config(tmp_path, str(rtk))
    write_agent_wrapper_config(
        tmp_path,
        order=["common", "spice-dev"],
        groups={"spice-dev": {"pytest": {"argv": ["python", "-m", "pytest"]}}},
    )
    ambient_env = shellhook.apply_shell_steering_environment(
        tmp_path,
        base_env=dict(os.environ),  # env-policy: allow
    )
    for name, value in ambient_env.items():
        monkeypatch.setenv(name, value)
    executed: list[list[str]] = []
    environments: list[dict[str, str] | None] = []

    class FakeProcess:
        pid = 0

        def wait(self) -> int:
            return 0

    def record_process(command, **kwargs):
        executed.append(command)
        environments.append(kwargs.get("env"))
        return FakeProcess()

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["zsh", "-c", "pytest -q"],
        popen_factory=record_process,
        stderr=io.StringIO(),
    )

    wrappers = (environments[0] or {})[shellhook.SHELL_HOOK_WRAPPERS_ENV]
    assert {
        "exit_code": exit_code,
        "executed": executed,
        "wrappers": wrappers,
        "pytest_wrapper_rendered": 'pytest() {\n  python -m pytest "$@"\n}' in wrappers,
    } == {
        "exit_code": 0,
        "executed": [["zsh", "-c", "pytest -q"]],
        "wrappers": ambient_env[shellhook.SHELL_HOOK_WRAPPERS_ENV],
        "pytest_wrapper_rendered": True,
    }


def test_shell_rewrite_yield_covers_module_pytest_and_selected_plain_grep(tmp_path):
    rtk = write_fake_rewriting_rtk(tmp_path)
    write_rtk_config(tmp_path, str(rtk))
    write_agent_wrapper_config(
        tmp_path,
        order=["common", "spice-dev"],
        groups={"spice-dev": {"pytest": {"argv": ["python", "-m", "pytest"]}}},
    )

    module = wrap.build_agent_run_command(
        ["zsh", "-c", "python -m pytest -q"], repo_root=tmp_path, rewrite_rtk=True
    )
    control = wrap.build_agent_run_command(
        ["zsh", "-c", "rg -n needle"], repo_root=tmp_path, rewrite_rtk=True
    )

    assert module == ["zsh", "-c", "python -m pytest -q"]
    assert control == ["zsh", "-c", "rg -n needle"]


def test_agent_run_routes_python_module_wrapper_through_uv(tmp_path, monkeypatch):
    if shutil.which("zsh") is None:
        pytest.skip("zsh is required for the end-to-end child shell")
    rtk = write_fake_rewriting_rtk(tmp_path)
    write_rtk_config(tmp_path, str(rtk))
    write_agent_wrapper_config(
        tmp_path,
        order=["common", "spice-dev"],
        groups={"spice-dev": {"pytest": {"argv": ["python", "-m", "pytest"]}}},
    )
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "fixture"\n',
        encoding="utf-8",
    )
    (tmp_path / "test_probe.py").write_text(
        "import sys\n\n\ndef test_probe():\n"
        "    print(f'probe-executable={sys.executable}')\n",
        encoding="utf-8",
    )
    home = tmp_path / "home"
    home.mkdir()
    operator_bin = tmp_path / "operator-bin"
    operator_bin.mkdir()
    uv_trace = tmp_path / "uv-trace.txt"
    uv = operator_bin / "uv"
    uv.write_text(
        "#!/bin/sh\n"
        f"printf '%s\\n' \"$*\" >> {shlex.quote(str(uv_trace))}\n"
        "shift 2\n"
        f'exec {shlex.quote(sys.executable)} "$@"\n',
        encoding="utf-8",
    )
    uv.chmod(0o755)
    ambient_python = operator_bin / "python"
    ambient_python.write_text(
        f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$@"\n',
        encoding="utf-8",
    )
    ambient_python.chmod(0o755)
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv(
        "PATH",
        str(operator_bin)
        + os.pathsep
        + os.environ.get("PATH", ""),  # env-policy: allow
    )
    for name in (
        shellhook.ZDOTDIR_ENV,
        shellhook.BASH_ENV_ENV,
        shellhook.HISTFILE_ENV,
        shellhook.ZSH_COMPDUMP_ENV,
        shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV,
        shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV,
        shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV,
    ):
        monkeypatch.delenv(name, raising=False)
    steering = shellhook.apply_shell_steering_environment(
        tmp_path,
        base_env=dict(os.environ),  # env-policy: allow
    )
    for name, value in steering.items():
        monkeypatch.setenv(name, value)
    monkeypatch.chdir(tmp_path)
    stdout_path = tmp_path / "child-stdout.txt"
    executed: list[list[str]] = []

    def spawning_recorder(command, **kwargs):
        executed.append(list(command))
        with stdout_path.open("w", encoding="utf-8") as sink:
            return subprocess.Popen(
                command, stdout=sink, stderr=subprocess.STDOUT, **kwargs
            )

    exit_code = wrap.run_agent_command(
        tmp_path,
        ["zsh", "-c", "pytest -s test_probe.py"],
        popen_factory=spawning_recorder,
        stderr=io.StringIO(),
    )
    control = wrap.build_agent_run_command(
        ["zsh", "-c", "rg -n needle"], repo_root=tmp_path, rewrite_rtk=True
    )

    output = stdout_path.read_text(encoding="utf-8")
    assert exit_code == 0
    assert "1 passed" in output
    assert f"probe-executable={sys.executable}" in output
    assert uv_trace.read_text(encoding="utf-8").splitlines() == [
        "run python -m pytest -s test_probe.py"
    ]
    assert executed == [["zsh", "-c", "pytest -s test_probe.py"]]
    assert control == ["zsh", "-c", "rg -n needle"]
    assert executed[0][2] != control[2]


def test_rtk_rewrite_yield_selectors_claim_repository_non_rtk_words(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["common", "spice-dev"],
        groups={
            "spice-dev": {
                "pytest": {"argv": ["python", "-m", "pytest"]},
                "pre-commit": {"argv": ["spice", "dev", "pre-commit"]},
                "summary": {"argv": ["rtk", "summary"]},
            }
        },
    )

    assert shellhook.rtk_rewrite_yield_selectors(tmp_path) == frozenset(
        {"grep", "pytest", "pre-commit"}
    )


@pytest.mark.parametrize("driver_name", ["codex", "claude"])
def test_rtk_rewrite_yield_selectors_include_selected_packaged_non_rtk_wrapper(
    tmp_path, monkeypatch, driver_name
):
    write_rtk_config(tmp_path, "alternate-rtk")
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, driver_name)

    assert shellhook.rtk_rewrite_yield_selectors(tmp_path) == frozenset({"grep"})


def test_rtk_rewrite_yield_selectors_leave_unselected_and_rtk_headed_words(
    tmp_path,
):
    write_agent_wrapper_config(
        tmp_path,
        order=["tools"],
        groups={"tools": {"summary": {"argv": ["rtk", "summary"]}}},
    )

    assert shellhook.rtk_rewrite_yield_selectors(tmp_path) == frozenset()


@pytest.mark.parametrize("driver_name", ["codex", "claude"])
def test_agent_run_preserves_native_rg_extended_regexp_results(
    tmp_path, monkeypatch, driver_name
):
    shell = shutil.which("zsh")
    if shell is None:
        pytest.skip("zsh is required for the agent-run shell path")
    fixture = tmp_path / "search-fixture.txt"
    fixture.write_text(
        "alpha-gamm\nbeta-gamma\nalphabeta-gamma\ngamma\nalpha-gammax\n",
        encoding="utf-8",
    )
    pattern = r"^(alpha|beta)+-gamma?$"
    raw = f"rg -n {shlex.quote(pattern)} {shlex.quote(str(fixture))}"
    rewritten = f"rtk grep -n {shlex.quote(pattern)} {shlex.quote(str(fixture))}"
    repo = Path(__file__).resolve().parents[1]
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, driver_name)
    monkeypatch.setattr(
        wrap,
        "rtk_rewrite_command_text",
        lambda *_args, **_kwargs: rewritten,
    )

    agent_command = wrap.build_agent_run_command(
        [shell, "-c", raw],
        repo_root=repo,
        rewrite_rtk=True,
    )
    native = subprocess.run(
        [shell, "-c", f"command {raw}"],
        check=False,
        capture_output=True,
        text=True,
    )
    through_agent = subprocess.run(
        agent_command,
        check=False,
        capture_output=True,
        text=True,
    )

    assert agent_command == [shell, "-c", raw]
    assert (through_agent.returncode, through_agent.stdout.splitlines()) == (
        native.returncode,
        native.stdout.splitlines(),
    )
    assert through_agent.returncode == 0
    assert len(through_agent.stdout.splitlines()) == 3


@pytest.mark.parametrize("driver_name", ["codex", "claude"])
@pytest.mark.parametrize("rg_flag", ["--hidden", "--no-ignore"])
@pytest.mark.parametrize("flag_position", ["leading", "trailing"])
def test_agent_run_preserves_rg_only_flags_in_any_position(
    tmp_path, monkeypatch, driver_name, rg_flag, flag_position
):
    shell = shutil.which("zsh")
    if shell is None:
        pytest.skip("zsh is required for the agent-run shell path")
    fixture = tmp_path / "search-fixture"
    fixture.mkdir()
    init_git_repo(fixture)
    (fixture / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
    (fixture / "flag.txt").write_text(
        "contains --hidden and --no-ignore literally\n",
        encoding="utf-8",
    )
    (fixture / "visible.txt").write_text("alpha visible\n", encoding="utf-8")
    (fixture / ".hidden.txt").write_text("alpha hidden\n", encoding="utf-8")
    (fixture / "ignored.txt").write_text("alpha ignored\n", encoding="utf-8")
    if flag_position == "leading":
        raw_words = ["rg", rg_flag, "alpha", str(fixture / "flag.txt")]
    else:
        raw_words = ["rg", "alpha", str(fixture), rg_flag]
    raw = shlex.join(raw_words)
    rewritten = shlex.join(["rtk", "grep", *raw_words[1:]])
    repo = Path(__file__).resolve().parents[1]
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, driver_name)
    monkeypatch.setattr(
        wrap,
        "rtk_rewrite_command_text",
        lambda *_args, **_kwargs: rewritten,
    )

    agent_command = wrap.build_agent_run_command(
        [shell, "-c", raw],
        repo_root=repo,
        rewrite_rtk=True,
    )
    native = subprocess.run(
        [shell, "-c", f"command {raw}"],
        check=False,
        capture_output=True,
        text=True,
    )
    through_agent = subprocess.run(
        agent_command,
        check=False,
        capture_output=True,
        text=True,
    )

    assert agent_command == [shell, "-c", raw]
    assert (
        through_agent.returncode,
        set(through_agent.stdout.splitlines()),
        through_agent.stderr.splitlines(),
    ) == (
        native.returncode,
        set(native.stdout.splitlines()),
        native.stderr.splitlines(),
    )
    if flag_position == "leading":
        # The intended alpha pattern is still the pattern: the flag itself must
        # not become grep's pattern and match the literal flag fixture.
        assert through_agent.returncode == 1
        assert through_agent.stdout == ""
    else:
        expected_names = (
            {".hidden.txt", "visible.txt"}
            if rg_flag == "--hidden"
            else {"ignored.txt", "visible.txt"}
        )
        assert {
            Path(line.split(":", 1)[0]).name
            for line in through_agent.stdout.splitlines()
        } == expected_names


def test_wrapper_degrades_malformed_direct_rewrite_to_original_execution(
    monkeypatch,
):
    executed: list[list[str]] = []
    monkeypatch.setattr(
        wrap,
        "rtk_rewrite_command_text",
        lambda *_args, **_kwargs: "rtk 'unterminated",
    )

    class FakeProcess:
        pid = 0

        def wait(self) -> int:
            return 0

    def record_process(command, **_kwargs):
        executed.append(command)
        return FakeProcess()

    stderr = io.StringIO()
    exit_code = wrap.run_agent_command(
        None,
        ["rg", "needle"],
        popen_factory=record_process,
        stderr=stderr,
    )

    assert {
        "exit_code": exit_code,
        "executed": executed,
        "warning": stderr.getvalue(),
    } == {
        "exit_code": 0,
        "executed": [["rg", "needle"]],
        "warning": (
            "spice agent run: RTK rewrite degraded to native "
            "executable='rtk' failure=malformed-direct-argv\n"
        ),
    }


def test_wrapper_does_not_special_case_proxy_argv(monkeypatch):
    calls: list[tuple[str, ...]] = []

    def fake_rewrite(*args: str, **_kwargs) -> str | None:
        calls.append(args)
        return None

    monkeypatch.setattr(wrap, "rtk_rewrite_command_text", fake_rewrite)

    command = wrap.build_agent_run_command(["proxy", "git", "status"], rewrite_rtk=True)

    assert command == ["proxy", "git", "status"]
    assert calls == [("proxy", "git", "status")]


def test_wrapper_routes_project_python_commands_through_uv(tmp_path, monkeypatch):
    write_spice_product_shape(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[project]\nname = 'probe'\n", encoding="utf-8"
    )

    python_command = wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    )
    python3_command = wrap.build_agent_run_command(
        ["python3", "-m", "pip", "--version"], repo_root=tmp_path
    )

    assert python_command == ["uv", "run", "python", "-m", "pip", "--version"]
    assert python3_command == ["uv", "run", "python", "-m", "pip", "--version"]


def test_wrapper_routes_only_bare_project_python_argv(tmp_path):
    write_spice_product_shape(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[project]\nname = 'probe'\n", encoding="utf-8"
    )

    assert wrap.build_agent_run_command(
        ["python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == ["uv", "run", "python", "-m", "pip", "--version"]
    assert wrap.build_agent_run_command(
        ["uv", "run", "python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == ["uv", "run", "python", "-m", "pip", "--version"]
    assert wrap.build_agent_run_command(
        ["proxy", "python", "-m", "pip", "--version"], repo_root=tmp_path
    ) == ["proxy", "python", "-m", "pip", "--version"]
    assert wrap.build_agent_run_command(["git", "status"], repo_root=tmp_path) == [
        "git",
        "status",
    ]


def test_wrapper_routes_project_python_through_uv_with_active_virtualenv(
    tmp_path, monkeypatch
):
    (tmp_path / "pyproject.toml").write_text(
        "[project]\nname = 'probe'\n", encoding="utf-8"
    )
    venv_python = tmp_path / "active-env" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / "active-env"))

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        "uv",
        "run",
        "python",
        "--version",
    ]


def test_wrapper_routes_project_python_through_uv_with_repo_venv(tmp_path, monkeypatch):
    (tmp_path / "pyproject.toml").write_text(
        "[project]\nname = 'probe'\n", encoding="utf-8"
    )
    venv_python = tmp_path / ".venv" / "bin" / "python"
    venv_python.parent.mkdir(parents=True)
    venv_python.write_text("#!/bin/sh\n", encoding="utf-8")
    venv_python.chmod(0o755)
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)

    assert wrap.build_agent_run_command(
        ["python", "--version"], repo_root=tmp_path
    ) == [
        "uv",
        "run",
        "python",
        "--version",
    ]


def test_wrapper_preserves_native_python_without_project_marker(tmp_path, monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)

    command = wrap.build_agent_run_command(["python", "--version"], repo_root=tmp_path)

    assert command == ["python", "--version"]


def test_wrapper_plain_commands_do_not_inject_worktree_spice_pythonpath(
    tmp_path, monkeypatch
):
    write_spice_product_shape(tmp_path)
    monkeypatch.delenv("PYTHONPATH", raising=False)

    env = wrap.build_agent_run_environment(["pytest"], repo_root=tmp_path)

    assert env is None


def test_static_shell_hook_paths_count_as_generated():
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()

    assert shellhook.is_generated_shell_hook_path(str(static_hook_dir))
    assert shellhook.is_generated_shell_hook_path(
        str(static_hook_dir / shellhook.BASH_HOOK_NAME)
    )


def test_wrapper_non_shell_commands_inherit_ambient_shell_hook_environment(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("ZDOTDIR", "hook")
    monkeypatch.setenv("BASH_ENV", "hook")
    monkeypatch.setenv(SHELL_TRACE_ENV, "preserved")

    env = wrap.build_agent_run_environment(["true"], repo_root=tmp_path)

    assert env is None


def test_wrapper_exports_agent_scoped_rtk_db_for_ambient_agent_commands(
    tmp_path, monkeypatch
):
    init_git_repo(tmp_path)
    monkeypatch.setenv(DRIVER.thread_id_env, "thread-a")
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)

    env = wrap.build_agent_run_environment(["true"], repo_root=tmp_path)

    assert env is not None
    assert Path(env[wrap.RTK_DB_PATH_ENV]) == (
        tmp_path / ".git" / ".spice" / "agents" / "thread-a" / "rtk" / "history.db"
    )
    assert Path(env[wrap.RTK_DB_PATH_ENV]).parent.is_dir()


@pytest.mark.parametrize("identity_kind", ["builtin", "basename", "absolute"])
def test_rtk_selectors_and_children_share_distinct_thread_scoped_history(
    tmp_path, monkeypatch, identity_kind
):
    init_git_repo(tmp_path)
    executable = {
        "builtin": "rtk",
        "basename": "alternate-rtk",
        "absolute": str(tmp_path / "Spice Tools" / "rtk companion"),
    }[identity_kind]
    write_rtk_config(tmp_path, executable)
    monkeypatch.setenv(wrap.RTK_DB_PATH_ENV, "/ambient/global-history.db")
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.setattr(wrap, "emit_initial_side_channel_payload", lambda *_a, **_k: ())
    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", lambda *_a, **_k: None)
    monkeypatch.setattr(wrap, "join_agent_side_channel_watch", lambda *_a, **_k: None)
    selector_environments: list[dict[str, str]] = []
    child_environments: list[dict[str, str]] = []
    selector_commands: list[list[str]] = []
    child_commands: list[list[str]] = []
    native_run = subprocess.run

    def rewrite_run(args, **kwargs):
        if args[:2] != [executable, "rewrite"]:
            return native_run(args, **kwargs)
        selector_commands.append(args)
        selector_environments.append(kwargs["env"])
        return subprocess.CompletedProcess(
            args,
            wrap.RTK_REWRITE_MATCH_EXIT_CODE,
            stdout="rtk git status",
            stderr="",
        )

    class Process:
        pid = SCOPED_REWRITE_PROCESS_PID

        def wait(self):
            return 0

    def popen(command, *, env):
        child_commands.append(command)
        child_environments.append(env)
        return Process()

    monkeypatch.setattr(wrap.subprocess, "run", rewrite_run)
    for thread_id, command in (
        ("thread-a", ["git", "status"]),
        ("thread-b", ["zsh", "-c", "git status"]),
    ):
        monkeypatch.setenv(DRIVER.thread_id_env, thread_id)
        assert (
            wrap.run_agent_command(
                tmp_path,
                command,
                popen_factory=popen,
                stderr=io.StringIO(),
            )
            == 0
        )

    expected_paths = [
        tmp_path / ".git" / ".spice" / "agents" / thread / "rtk" / "history.db"
        for thread in ("thread-a", "thread-b")
    ]
    assert [Path(env[wrap.RTK_DB_PATH_ENV]) for env in selector_environments] == (
        expected_paths
    )
    assert [Path(env[wrap.RTK_DB_PATH_ENV]) for env in child_environments] == (
        expected_paths
    )
    assert selector_commands == [
        [executable, "rewrite", "--", "git", "status"],
        [executable, "rewrite", "--", "git status"],
    ]
    assert child_commands == [
        [executable, "git", "status"],
        ["zsh", "-c", f"{shlex.quote(executable)} git status"],
    ]


def test_layered_wrapper_false_disables_inherited_entry_and_group(tmp_path):
    write_agent_wrapper_config(
        tmp_path,
        order=["common", "disabled", "active"],
        groups={
            "common": {
                "rtk": False,
                "wrap": ["grep"],
            },
            "disabled": False,
            "active": {"pytest": {"argv": ["python", "-m", "pytest"]}},
        },
    )

    assert shellhook.render_agent_wrapper_lines(tmp_path) == [
        *expected_wrapper_lines("wrap", ["grep"]),
        *expected_python_module_wrapper_lines(["pytest"]),
    ]


def test_configured_agent_environment_installs_driver_shell_steering_hooks(
    tmp_path, monkeypatch
):
    from spice.config.edit import set_scope_section
    from spice.config.layers import WORKTREE_SOURCE

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    set_scope_section(tmp_path, WORKTREE_SOURCE, "agent", {"driver": "claude"})
    real_zdotdir = tmp_path / "real-zdotdir"
    real_zdotdir.mkdir()
    real_bash_env = tmp_path / "real-bash-env"
    real_bash_env.write_text("# real bash env\n", encoding="utf-8")
    monkeypatch.delenv(agent_driver.SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.delenv(DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(CLAUDE_DRIVER.thread_id_env, raising=False)
    monkeypatch.delenv(shellhook.HISTFILE_ENV, raising=False)
    monkeypatch.delenv(shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV, raising=False)
    monkeypatch.setenv(shellhook.ZDOTDIR_ENV, str(real_zdotdir))
    monkeypatch.setenv(shellhook.BASH_ENV_ENV, str(real_bash_env))

    env = lifecycle.agent_environment(tmp_path)

    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    assert env[shellhook.ZDOTDIR_ENV] == str(hook_dir)
    assert env[shellhook.BASH_ENV_ENV] == str(hook_dir / shellhook.BASH_HOOK_NAME)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == str(real_zdotdir)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == str(real_bash_env)
    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        real_zdotdir / ".zsh_history"
    )
    zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in zshenv
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in bashenv
    assert "spice agent run --" in zshenv
    assert "spice agent run --" in bashenv
    assert "--preserve-shell-hook-env" not in zshenv
    assert "--preserve-shell-hook-env" not in bashenv
    assert UNSUPPORTED_AGENT_STEER_COMMAND not in zshenv
    assert "--watch --parent-pid" not in zshenv


def test_shell_steering_runtime_environment_ignores_generated_hook_as_original():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()

    env = shellhook.shell_steering_runtime_environment(
        base_env={
            shellhook.ZDOTDIR_ENV: str(hook_dir),
            shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
            shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: str(hook_dir),
            shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: str(
                hook_dir / shellhook.BASH_HOOK_NAME
            ),
        },
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == ""
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == ""


def test_shell_steering_runtime_environment_keeps_real_original_before_hook():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()

    env = shellhook.shell_steering_runtime_environment(
        base_env={
            shellhook.ZDOTDIR_ENV: str(hook_dir),
            shellhook.BASH_ENV_ENV: str(hook_dir / shellhook.BASH_HOOK_NAME),
            shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV: "/real-zdotdir",
            shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV: "/real-bash-env",
        },
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV] == "/real-zdotdir"
    assert env[shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV] == "/real-bash-env"


def test_shell_steering_runtime_environment_maps_zsh_history_to_home(tmp_path):
    env = shellhook.shell_steering_runtime_environment(
        base_env={"HOME": str(tmp_path)},
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        tmp_path / ".zsh_history"
    )


def test_shell_steering_runtime_environment_maps_zsh_history_to_original_zdotdir(
    tmp_path,
):
    real_zdotdir = tmp_path / "real-zdotdir"
    env = shellhook.shell_steering_runtime_environment(
        base_env={shellhook.ZDOTDIR_ENV: str(real_zdotdir)},
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        real_zdotdir / ".zsh_history"
    )


def test_shell_steering_runtime_environment_preserves_explicit_zsh_history(tmp_path):
    history = tmp_path / "custom-history"
    env = shellhook.shell_steering_runtime_environment(
        base_env={shellhook.HISTFILE_ENV: str(history)},
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(history)


def test_shell_steering_runtime_environment_ignores_generated_hook_zsh_history(
    tmp_path,
):
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    env = shellhook.shell_steering_runtime_environment(
        base_env={
            "HOME": str(tmp_path),
            shellhook.HISTFILE_ENV: str(hook_dir / ".zsh_history"),
        },
    )

    assert env[shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV] == str(
        tmp_path / ".zsh_history"
    )


def test_shell_steering_files_are_stable_across_original_env_changes():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    first_zshenv = (hook_dir / ".zshenv").read_text(encoding="utf-8")
    first_bashenv = (hook_dir / shellhook.BASH_HOOK_NAME).read_text(encoding="utf-8")

    assert (hook_dir / ".zshenv").read_text(encoding="utf-8") == first_zshenv
    assert (hook_dir / shellhook.BASH_HOOK_NAME).read_text(
        encoding="utf-8"
    ) == first_bashenv
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in first_zshenv
    assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in first_bashenv
    assert "spice agent run --" in first_zshenv
    assert "spice agent run --" in first_bashenv
    assert "--preserve-shell-hook-env" not in first_zshenv
    assert "--preserve-shell-hook-env" not in first_bashenv
    assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in first_bashenv
    assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in first_bashenv


def test_packaged_shell_hooks_are_static_env_driven_and_packaged():
    hook_dir = shellhook.packaged_shell_steering_hook_dir()
    static_hook_dir = shellhook.packaged_shell_steering_static_hook_dir()
    dynamic_surfaces = {
        ".zshenv",
        ".zprofile",
        ".zlogin",
        shellhook.BASH_HOOK_NAME,
    }
    expected_reexec_lines = {
        ".zshenv": [
            'exec spice agent run -- "$_spice_shell_bin" -lc "$ZSH_EXECUTION_STRING"',
            'exec spice agent run -- "$_spice_shell_bin" -c "$ZSH_EXECUTION_STRING"',
        ],
        ".zprofile": [
            'exec spice agent run -- "$_spice_shell_bin" -lc "$ZSH_EXECUTION_STRING"',
            'exec spice agent run -- "$_spice_shell_bin" -c "$ZSH_EXECUTION_STRING"',
        ],
        ".zlogin": [
            'exec spice agent run -- "$_spice_shell_bin" -lc "$ZSH_EXECUTION_STRING"',
            'exec spice agent run -- "$_spice_shell_bin" -c "$ZSH_EXECUTION_STRING"',
        ],
        shellhook.BASH_HOOK_NAME: [
            'exec spice agent run -- "$_spice_shell_bin" -lc "$BASH_EXECUTION_STRING"',
            'exec spice agent run -- "$_spice_shell_bin" -c "$BASH_EXECUTION_STRING"',
        ],
    }

    for filename in (*shellhook.ZSH_HOOK_NAMES, shellhook.BASH_HOOK_NAME):
        text = (hook_dir / filename).read_text(encoding="utf-8")
        assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in text
        assert shellhook.SHELL_HOOK_WRAPPERS_ENV in text
        assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in text
        assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in text
        if filename in dynamic_surfaces:
            reexec_lines = [
                line.strip()
                for line in text.splitlines()
                if line.strip().startswith("exec spice agent run --")
            ]
            assert reexec_lines == expected_reexec_lines[filename]
        assert "staticshellhooks" in text
        assert "--preserve-shell-hook-env" not in text
        if filename == shellhook.BASH_HOOK_NAME:
            assert shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV not in text
        else:
            assert shellhook.SHELL_HOOK_ORIGINAL_HISTFILE_ENV in text

        static_text = (static_hook_dir / filename).read_text(encoding="utf-8")
        assert UNSUPPORTED_AGENT_SHELL_HOOK_COMMAND not in static_text
        assert "spice agent run --" not in static_text
        assert shellhook.SHELL_HOOK_WRAPPERS_ENV in static_text
        assert shellhook.SHELL_HOOK_ORIGINAL_ZDOTDIR_ENV in static_text
        assert shellhook.SHELL_HOOK_ORIGINAL_BASH_ENV_ENV in static_text

    package_data = tomllib.loads(Path("pyproject.toml").read_text(encoding="utf-8"))[
        "tool"
    ]["setuptools"]["package-data"]["spice.agent"]
    assert "shellhooks/.zshrc" in package_data
    assert "staticshellhooks/.zshrc" in package_data


def test_runtime_environment_preserves_operator_path_with_worktree_venv(tmp_path):
    (tmp_path / ".venv" / "bin").mkdir(parents=True)
    base_env = {"PATH": "/operator/bin:/usr/bin"}
    runtime_env = shellhook.shell_steering_runtime_environment(
        base_env=base_env, repo_root=tmp_path
    )
    env = {**base_env, **runtime_env}
    assert env["PATH"] == "/operator/bin:/usr/bin"


def test_runtime_environment_leaves_path_untouched_without_a_venv(tmp_path):
    env = shellhook.shell_steering_runtime_environment(
        base_env={"PATH": "/usr/bin"}, repo_root=tmp_path
    )
    assert "PATH" not in env
