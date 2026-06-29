from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.policy import ENV_POLICY_ALLOW_MARKER
from spice.studies.envpolicy import (
    render_env_name_ledger_board,
    render_env_policy_board,
    scan_env_name_ledger,
    scan_env_policy,
)


def test_env_policy_defaults_still_apply(tmp_path):
    names = [
        "SPICE_" + "TASK_BACKEND",
        "CODEX_" + "THREAD_ID",
        "CLAUDE_" + "CODE_SESSION_ID",
    ]
    path = tmp_path / "sample.py"
    path.write_text(
        "\n".join(
            f'VALUE = os.environ["{name}"]' for name in names
        ),  # env-policy: allow
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("sample.py")], root=tmp_path)

    assert [finding.name for finding in findings] == names


def test_env_policy_repo_patterns_merge_with_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        'env_name_patterns = ["MYPROJ_[A-Z0-9_]+", "ENGINE_[A-Z0-9_]+", "DEPLOY_TARGET"]\n',
        encoding="utf-8",
    )
    names = [
        "SPICE_" + "TASK_BACKEND",
        "CLAUDE_" + "CODE_SESSION_ID",
        "MYPROJ_" + "AUTH_TOKEN",
        "ENGINE_" + "THISISABATCHMODE",
        "DEPLOY_TARGET",
    ]
    path = tmp_path / "sample.cs"
    path.write_text(
        "\n".join(f'var value = "{name}";' for name in names), encoding="utf-8"
    )

    findings = scan_env_policy([Path("sample.cs")], root=tmp_path)

    assert [finding.name for finding in findings] == names


def test_env_policy_allow_marker_guidance_applies_to_repo_patterns(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nenv_name_patterns = ["MYPROJ_[A-Z0-9_]+"]\n',
        encoding="utf-8",
    )
    env_name = "MYPROJ_" + "AUTH_TOKEN"
    path = tmp_path / "sample.py"
    path.write_text(f'VALUE = "{env_name}"\n', encoding="utf-8")

    board = render_env_policy_board(scan_env_policy([Path("sample.py")], root=tmp_path))

    assert f"add `# {ENV_POLICY_ALLOW_MARKER}`" in board
    assert f"sample.py:1: {env_name}" in board


def test_env_policy_previous_line_marker_waives_next_statement(tmp_path):
    env_name = "SPICE_" + "TASK_BACKEND"
    path = tmp_path / "sample.py"
    path.write_text(
        f'# env-policy: allow\nVALUE = "{env_name}"\n',
        encoding="utf-8",
    )

    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_policy_inline_marker_does_not_waive_next_statement(tmp_path):
    waived_name = "SPICE_" + "TASK_BACKEND"
    unwaived_name = "CODEX_" + "THREAD_ID"
    path = tmp_path / "sample.py"
    path.write_text(
        f'WAIVED = "{waived_name}"  # env-policy: allow\n'
        f'UNWAIVED = "{unwaived_name}"\n',
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("sample.py")], root=tmp_path)

    assert [(finding.line, finding.name) for finding in findings] == [
        (2, unwaived_name)
    ]


def test_env_policy_wrapped_statement_marker_waives_wrapped_literal(tmp_path):
    path = tmp_path / "sample.py"
    path.write_text(
        "monkeypatch.setenv(\n"
        '    "CODEX_THREAD_ID",\n'
        '    "thread-ambient-value-that-makes-the-line-wrap-beyond-the-formatter-limit",\n'
        ")  # env-policy: allow\n",
        encoding="utf-8",
    )

    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_access_gate_on_by_default_flags_env_access(tmp_path):
    path = tmp_path / "sample.py"
    # env-policy: allow
    path.write_text('value = os.getenv("HOME")\n', encoding="utf-8")

    # With no config the access gate is on: a non-watchlisted env read is
    # flagged unless waived.
    findings = scan_env_policy([Path("sample.py")], root=tmp_path)
    assert [(f.line, f.name) for f in findings] == [(1, "os env access")]


def test_env_access_gate_opt_out_disables_access_findings(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\nenv_access_gate = false\n", encoding="utf-8"
    )
    path = tmp_path / "sample.py"
    # env-policy: allow
    path.write_text('value = os.getenv("HOME")\n', encoding="utf-8")

    # Opting out weakens the gate: the non-watchlisted read is no longer flagged.
    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_access_gate_flags_unwaived_and_dynamic_env_access(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\nenv_access_gate = true\n", encoding="utf-8"
    )
    path = tmp_path / "sample.py"
    path.write_text(
        'literal = os.getenv("HOME")\ndynamic = os.environ[chosen_key]\n',  # env-policy: allow
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("sample.py")], root=tmp_path)

    # Both the non-watchlisted literal name and the dynamic name are caught by
    # the access gate, not by the name watchlist.
    assert [(f.line, f.name) for f in findings] == [
        (1, "os env access"),
        (2, "os env access"),
    ]


def test_env_access_gate_respects_waiver(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\nenv_access_gate = true\n", encoding="utf-8"
    )
    path = tmp_path / "sample.py"
    path.write_text(
        'value = os.getenv("HOME")  # env-policy: allow\n', encoding="utf-8"
    )

    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_access_gate_rejects_non_boolean_flag(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nenv_access_gate = "yes"\n', encoding="utf-8"
    )
    path = tmp_path / "sample.py"
    path.write_text(
        'value = os.getenv("HOME")\n', encoding="utf-8"
    )  # env-policy: allow

    with pytest.raises(SpiceError, match="env_access_gate must be a boolean"):
        scan_env_policy([Path("sample.py")], root=tmp_path)


def test_env_access_gate_flags_python_putenv_and_unsetenv(tmp_path):
    path = tmp_path / "sample.py"
    path.write_text(
        'os.putenv("FAKEENV_X", "1")\nos.unsetenv("FAKEENV_X")\n',  # env-policy: allow
        encoding="utf-8",
    )

    # The Python default idiom now covers the mutating forms, not just reads.
    findings = scan_env_policy([Path("sample.py")], root=tmp_path)
    assert [(f.line, f.name) for f in findings] == [
        (1, "os env access"),
        (2, "os env access"),
    ]


def test_env_access_default_patterns_configures_a_family(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.env_access.default_patterns]\n"
        'csharp = ["ProjectEnv\\\\.Read"]\n',
        encoding="utf-8",
    )
    path = tmp_path / "Sample.cs"
    path.write_text(
        'var v = ProjectEnv.Read("HOME");\n',  # env-policy: allow
        encoding="utf-8",
    )

    # A repo registers its own C# idiom; the access gate audits .cs access sites.
    findings = scan_env_policy([Path("Sample.cs")], root=tmp_path)
    assert [(f.line, f.name) for f in findings] == [(1, "environment env access")]


def test_env_access_config_adds_custom_family(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.languages]\n"
        'env = [".foo"]\n'
        "\n"
        "[tool.spice.policy.env_access.family_suffixes]\n"
        'custom = [".foo"]\n'
        "\n"
        "[tool.spice.policy.env_access.default_patterns]\n"
        'custom = ["CUSTOMENV\\\\.read"]\n',
        encoding="utf-8",
    )
    path = tmp_path / "sample.foo"
    path.write_text('let value = CUSTOMENV.read("HOME")\n', encoding="utf-8")

    findings = scan_env_policy([Path("sample.foo")], root=tmp_path)

    assert [(finding.line, finding.name) for finding in findings] == [
        (1, "custom env access")
    ]


def test_env_access_gate_flags_builtin_csharp_env_accesses(tmp_path):
    path = tmp_path / "Sample.cs"
    path.write_text(
        'var home = Environment.GetEnvironmentVariable("HOME");\n'
        'System.Environment.SetEnvironmentVariable("GAME_MODE", mode);\n',
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("Sample.cs")], root=tmp_path)

    assert [(f.line, f.name) for f in findings] == [
        (1, "environment env access"),
        (2, "environment env access"),
    ]


def test_env_access_gate_builtin_csharp_access_respects_waiver(tmp_path):
    path = tmp_path / "Sample.cs"
    path.write_text(
        'var home = System.Environment.GetEnvironmentVariable("HOME"); '
        "// env-policy: allow\n",
        encoding="utf-8",
    )

    assert scan_env_policy([Path("Sample.cs")], root=tmp_path) == []


def test_env_access_gate_ignores_non_env_csharp_system_calls(tmp_path):
    path = tmp_path / "Sample.cs"
    path.write_text(
        'System.Console.WriteLine("HOME");\nEnvironment.Exit(0);\n',
        encoding="utf-8",
    )

    assert scan_env_policy([Path("Sample.cs")], root=tmp_path) == []


def test_env_access_gate_flags_builtin_shell_env_accesses(tmp_path):
    path = tmp_path / "run.sh"
    path.write_text(
        'echo "$HOME"\nprintf "%s\\n" "${CONFIG_DIR}"\nexport APP_MODE=debug\n',
        encoding="utf-8",
    )

    findings = scan_env_policy([Path("run.sh")], root=tmp_path)

    assert [(f.line, f.name) for f in findings] == [
        (1, "shell env access"),
        (2, "shell env access"),
        (3, "shell env access"),
    ]


def test_env_access_gate_builtin_shell_access_respects_waiver(tmp_path):
    path = tmp_path / "run.zsh"
    path.write_text(
        'echo "$SPICE_TASK_ID" # env-policy: allow\n',
        encoding="utf-8",
    )

    assert scan_env_policy([Path("run.zsh")], root=tmp_path) == []


def test_env_access_gate_shell_matchers_are_shell_scoped(tmp_path):
    (tmp_path / "sample.js").write_text(
        'const rendered = `${HOME}`;\nconst literal = "$APP_MODE";\n',
        encoding="utf-8",
    )
    (tmp_path / "run.bash").write_text('echo "$APP_MODE"\n', encoding="utf-8")

    assert scan_env_policy([Path("sample.js")], root=tmp_path) == []
    sh_findings = scan_env_policy([Path("run.bash")], root=tmp_path)
    assert [(f.line, f.name) for f in sh_findings] == [(1, "shell env access")]


def test_env_access_gate_ignores_shell_special_parameters(tmp_path):
    path = tmp_path / "run.sh"
    path.write_text(
        'echo "$? $$ $1 $@ $* $# $- $_ ${_}"\n',
        encoding="utf-8",
    )

    assert scan_env_policy([Path("run.sh")], root=tmp_path) == []


def test_env_access_matchers_are_family_scoped(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.env_access.default_patterns]\nshell = ['\\$[A-Z_]+']\n",
        encoding="utf-8",
    )
    # `$FOO` is a shell idiom only: it must fire on .sh but never on .py.
    (tmp_path / "sample.py").write_text("value = FOO\n", encoding="utf-8")
    (tmp_path / "run.sh").write_text(
        "echo $FOO\n", encoding="utf-8"
    )  # env-policy: allow

    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []
    sh_findings = scan_env_policy([Path("run.sh")], root=tmp_path)
    assert [(f.line, f.name) for f in sh_findings] == [(1, "shell env access")]


def test_env_access_default_patterns_invalid_regex_raises(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.env_access.default_patterns]\nlua = ['os.getenv(']\n",
        encoding="utf-8",
    )  # env-policy: allow
    (tmp_path / "sample.lua").write_text(
        "local v = os.getenv('FAKEENV_X')\n", encoding="utf-8"
    )  # env-policy: allow

    with pytest.raises(SpiceError, match="default_patterns.*invalid regex"):
        scan_env_policy([Path("sample.lua")], root=tmp_path)


def test_env_access_default_patterns_unknown_family_raises(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy.env_access.default_patterns]\nrust = ["env::var"]\n',
        encoding="utf-8",
    )
    (tmp_path / "sample.py").write_text("x = 1\n", encoding="utf-8")

    with pytest.raises(SpiceError, match="unknown family"):
        scan_env_policy([Path("sample.py")], root=tmp_path)


def test_env_access_default_patterns_non_table_raises(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy.env_access]\ndefault_patterns = ["nope"]\n',
        encoding="utf-8",
    )
    (tmp_path / "sample.py").write_text("x = 1\n", encoding="utf-8")

    with pytest.raises(SpiceError, match="default_patterns must be a table"):
        scan_env_policy([Path("sample.py")], root=tmp_path)


def test_env_access_gate_flags_lua_os_getenv_by_default(tmp_path):
    path = tmp_path / "config.lua"
    path.write_text(
        "local home = os.getenv('HOME')\nlocal ok = os.getenv('FAKEENV_OK')  -- env-policy: allow\n",
        encoding="utf-8",
    )

    # The Lua stdlib idiom is audited with no config; the waived read clears.
    findings = scan_env_policy([Path("config.lua")], root=tmp_path)
    assert [(f.line, f.name) for f in findings] == [(1, "lua env access")]


def test_env_access_default_patterns_registers_lua_colon_accessor(tmp_path):
    # The consuming project's bespoke runtime accessor is method-style and not a
    # universal idiom, so it is registered via config, scoped to Lua.
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.env_access.default_patterns]\nlua = ['\\w+:GetEnv\\(']\n",
        encoding="utf-8",
    )
    (tmp_path / "runtime.lua").write_text(
        "local v = engine:GetEnv('LEVEL')\n", encoding="utf-8"
    )
    # The very same accessor text in a non-Lua source must NOT be flagged: the
    # registered pattern is scoped to Lua's suffixes only.
    (tmp_path / "sample.py").write_text(
        "v = engine:GetEnv('LEVEL')\n", encoding="utf-8"
    )

    lua_findings = scan_env_policy([Path("runtime.lua")], root=tmp_path)
    assert [(f.line, f.name) for f in lua_findings] == [(1, "lua env access")]
    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_access_gate_flags_javascript_process_env_by_default(tmp_path):
    path = tmp_path / "config.ts"
    path.write_text(
        "const home = process.env.HOME\n"
        "const port = process.env['FAKEENV_PORT']\n"
        'const tls = process.env["FAKEENV_TLS"]  // env-policy: allow\n',
        encoding="utf-8",
    )

    # Dot- and bracket-access are both audited with no config; the waived line clears.
    findings = scan_env_policy([Path("config.ts")], root=tmp_path)
    assert [(f.line, f.name) for f in findings] == [
        (1, "process.env access"),
        (2, "process.env access"),
    ]


def test_env_access_gate_javascript_matcher_is_scoped(tmp_path):
    # `process.env` is a JS idiom only: the same text in a .py source is not flagged.
    (tmp_path / "app.js").write_text("const k = process.env.KEY\n", encoding="utf-8")
    (tmp_path / "sample.py").write_text("k = process.env.KEY\n", encoding="utf-8")

    js_findings = scan_env_policy([Path("app.js")], root=tmp_path)
    assert [(f.line, f.name) for f in js_findings] == [(1, "process.env access")]
    assert scan_env_policy([Path("sample.py")], root=tmp_path) == []


def test_env_name_ledger_flags_unaccounted_literal_env_names(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nenv_names = ["FAKEENV_PORT"]\n',
        encoding="utf-8",
    )
    (tmp_path / "sample.py").write_text(
        'home = os.getenv("HOME")\nport = os.environ["FAKEENV_PORT"]\n',  # env-policy: allow
        encoding="utf-8",
    )

    findings = scan_env_name_ledger([Path("sample.py")], root=tmp_path)

    assert [(finding.kind, finding.name) for finding in findings] == [
        ("unaccounted", "HOME")
    ]
    board = render_env_name_ledger_board(findings)
    assert "unaccounted: HOME" in board
    assert "used at sample.py:1" in board


def test_env_name_ledger_flags_stale_declared_env_names(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nenv_names = ["HOME", "OLD_ENV"]\n',
        encoding="utf-8",
    )
    (tmp_path / "sample.py").write_text(
        'home = os.getenv("HOME")\n',  # env-policy: allow
        encoding="utf-8",
    )

    findings = scan_env_name_ledger([Path("sample.py")], root=tmp_path)

    assert [(finding.kind, finding.name) for finding in findings] == [
        ("stale", "OLD_ENV")
    ]


def test_env_name_ledger_passes_clean_manifest_across_languages(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nenv_names = ["APP_MODE", "HOME", "FAKEENV_PORT", "FAKEENV_TLS"]\n',
        encoding="utf-8",
    )
    (tmp_path / "sample.py").write_text(
        'home = os.getenv("HOME")\n',  # env-policy: allow
        encoding="utf-8",
    )
    (tmp_path / "config.ts").write_text(
        "const port = process.env.FAKEENV_PORT\n"
        'const tls = process.env["FAKEENV_TLS"]\n',
        encoding="utf-8",
    )
    (tmp_path / "run.sh").write_text(
        "export APP_MODE=debug\n",
        encoding="utf-8",
    )

    findings = scan_env_name_ledger(
        [Path("sample.py"), Path("config.ts"), Path("run.sh")],
        root=tmp_path,
    )

    assert render_env_name_ledger_board(findings) == "env-name-ledger: ok"


def test_env_name_ledger_holds_test_files_to_the_same_standard(tmp_path):
    # Tests are not exempt: an env name referenced only in a test file is still
    # required in the manifest, exactly like production source.
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\nenv_names = []\n", encoding="utf-8"
    )
    test_dir = tmp_path / "tests"
    test_dir.mkdir()
    (test_dir / "test_env_usage.py").write_text(
        'value = os.getenv("FAKEENV_X")\n',  # env-policy: allow
        encoding="utf-8",
    )

    findings = scan_env_name_ledger([Path("tests/test_env_usage.py")], root=tmp_path)

    assert [(f.kind, f.name) for f in findings] == [("unaccounted", "FAKEENV_X")]
