"""Constitution mechanics: flex ratio, sticky state, magic-number verdicts."""

from pathlib import Path

from spice.flexstate import (
    sticky_function_keys_after_renames,
    sticky_items_after_flex_breaches,
    sticky_paths_after_renames,
)
from spice.policy import (
    COMPLEXITY_MAX_CCN,
    COMPLEXITY_MAX_LENGTH,
    ENV_POLICY_ALLOW_MARKER,
    FILE_LOC_LIMIT,
    FLEX_DENOMINATOR,
    FLEX_NUMERATOR,
    flex_limit,
)
from spice.studies.envpolicy import render_env_policy_board, scan_env_policy
from spice.studies.fileloc import scan_loc_violations
from spice.studies.magicnums import scan_text_magic_numbers

MAGIC_HIGH_THRESHOLD = 100
MAGIC_HIGH_LITERAL = "125"


def test_flex_ratio_is_three_halves():
    for limit in (FILE_LOC_LIMIT, COMPLEXITY_MAX_LENGTH, COMPLEXITY_MAX_CCN):
        assert flex_limit(limit) == limit * FLEX_NUMERATOR // FLEX_DENOMINATOR


def test_flex_breach_joins_sticky_set():
    sticky = sticky_items_after_flex_breaches(
        [("a.py", 1600), ("b.py", 900)],
        {Path("c.py")},
        key_for_item=lambda item: Path(item[0]),
        is_breach=lambda item: item[1] > flex_limit(FILE_LOC_LIMIT),
    )
    assert sticky == {Path("a.py"), Path("c.py")}


def test_sticky_paths_follow_renames():
    sticky = sticky_paths_after_renames(
        {Path("old.py")}, {Path("old.py"): Path("new.py")}
    )
    assert sticky == {Path("old.py"), Path("new.py")}


def test_binary_assets_are_byte_gated_but_not_line_gated(tmp_path):
    rel_path = Path("screenshot.png")
    (tmp_path / rel_path).write_bytes(b"\x89PNG\r\n\x1a\n\0" + b"\n" * 2000)

    findings = scan_loc_violations(
        [rel_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
    )

    assert len(findings) == 1
    assert findings[0].line_count == 0
    assert not findings[0].over_line_limit
    assert findings[0].over_byte_limit


def test_sticky_function_keys_follow_renames():
    sticky = sticky_function_keys_after_renames(
        {("old.py", "run")}, {Path("old.py"): Path("new.py")}
    )
    assert sticky == {("old.py", "run"), ("new.py", "run")}


def test_python_comparison_pivot_is_flagged():
    findings = scan_text_magic_numbers(
        Path("sample.py"), "def f(n):\n    return n > 75\n"
    )
    assert [(finding.line, finding.literal) for finding in findings] == [(2, "75")]


def test_magic_threshold_is_explicit_scan_policy():
    findings = scan_text_magic_numbers(
        Path("sample.py"),
        f"def f(n):\n    return n > {MAGIC_HIGH_LITERAL}\n",
        examine_threshold=MAGIC_HIGH_THRESHOLD,
    )
    assert [(finding.line, finding.literal) for finding in findings] == [
        (2, MAGIC_HIGH_LITERAL)
    ]


def test_python_named_constant_and_call_args_pass():
    text = "LIMIT = 4096\n\n\ndef f(handle):\n    return handle.read(4096)\n"
    assert scan_text_magic_numbers(Path("sample.py"), text) == []


def test_python_small_comparisons_pass():
    text = "def f(items):\n    return len(items) > 2\n"
    assert scan_text_magic_numbers(Path("sample.py"), text) == []


def test_js_comparison_pivot_is_flagged():
    findings = scan_text_magic_numbers(
        Path("sample.js"), "if (delta > 75) {\n  grow();\n}\n"
    )
    assert [(finding.line, finding.literal) for finding in findings] == [(1, "75")]


def test_js_const_definitions_and_call_args_pass():
    text = "const messageLimit = 400;\nsetTimeout(tick, 600);\nx = y * 1000;\n"
    assert scan_text_magic_numbers(Path("sample.js"), text) == []


def test_js_arrow_and_shift_operators_pass():
    text = "const f = (x) => 500;\nconst y = bits >> 16;\n"
    assert scan_text_magic_numbers(Path("sample.js"), text) == []


def test_c_grammar_family_covers_other_languages():
    go_findings = scan_text_magic_numbers(
        Path("sample.go"), "if delta > 75 {\n\tgrow()\n}\n"
    )
    rust_findings = scan_text_magic_numbers(
        Path("sample.rs"), "if delta > 75 { grow(); } // limit\n"
    )
    assert [(f.line, f.literal) for f in go_findings] == [(1, "75")]
    assert [(f.line, f.literal) for f in rust_findings] == [(1, "75")]


def test_c_grammar_comments_pass():
    text = "// retries > 75 is too many\n/* delta > 99 */\nlet x = 5;\n"
    assert scan_text_magic_numbers(Path("sample.rs"), text) == []


def test_env_policy_defaults_still_apply(tmp_path):
    env_name = "SPICE_" + "TASK_BACKEND"
    path = tmp_path / "sample.py"
    path.write_text(f'import os\nVALUE = os.environ["{env_name}"]\n', encoding="utf-8")

    findings = scan_env_policy([Path("sample.py")], root=tmp_path)

    assert [(finding.line, finding.name) for finding in findings] == [(2, env_name)]


def test_env_policy_repo_patterns_merge_with_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        'env_name_patterns = ["MYPROJ_[A-Z0-9_]+", "ENGINE_[A-Z0-9_]+", "DEPLOY_TARGET"]\n',
        encoding="utf-8",
    )
    names = [
        "SPICE_" + "TASK_BACKEND",
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
