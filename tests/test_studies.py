"""Constitution mechanics: flex ratio, sticky state, magic-number verdicts."""

from pathlib import Path

import pytest

from spice.cli.parser import build_parser
from spice.errors import SpiceError
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
from spice.studies import cli as studies_cli
from spice.studies.envpolicy import render_env_policy_board, scan_env_policy
from spice.studies.fileloc import scan_loc_violations, scan_staged_loc_violations
from spice.studies.magicnums import scan_text_magic_numbers
from spice.studies.shape import configured_package_roots
from spice.studies.typecheck import (
    PYRIGHT_ARGS,
    python_typecheck_argv,
    python_typecheck_targets,
    run_python_typecheck,
)

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


def test_generated_lockfiles_do_not_trip_file_shape_pressure(tmp_path):
    lock_path = Path("uv.lock")
    generic_lock_path = Path("tool.lock")
    nested_lock_path = Path("client") / "package-lock.json"
    source_path = Path("large_source.py")
    (tmp_path / "client").mkdir()
    (tmp_path / lock_path).write_text("package = []\n" * 20, encoding="utf-8")
    (tmp_path / generic_lock_path).write_text("state = []\n" * 20, encoding="utf-8")
    (tmp_path / nested_lock_path).write_text(
        '{"lockfileVersion": 3}\n' * 20,
        encoding="utf-8",
    )
    (tmp_path / source_path).write_text("print('large')\n" * 20, encoding="utf-8")

    findings = scan_loc_violations(
        [lock_path, generic_lock_path, nested_lock_path, source_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
    )

    assert [finding.path for finding in findings] == [source_path.as_posix()]


def test_study_explicit_directory_reports_file_path_requirement(tmp_path, monkeypatch):
    directory = tmp_path / "spice" / "serve"
    directory.mkdir(parents=True)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "file-loc", "spice/serve"])

    with pytest.raises(SpiceError, match="file paths.*spice/serve"):
        args.func(args)


def test_generated_lockfiles_are_pruned_from_file_shape_sticky_state(
    tmp_path, monkeypatch
):
    lock_path = Path("uv.lock")
    sticky_source_path = Path("sticky_source.py")
    (tmp_path / lock_path).write_text("package = []\n" * 20, encoding="utf-8")
    (tmp_path / sticky_source_path).write_text(
        "print('large')\n" * 20,
        encoding="utf-8",
    )
    saved: dict[str, set[Path]] = {}

    monkeypatch.setattr(
        "spice.studies.fileloc.staged_renames",
        lambda _root: {},
    )
    monkeypatch.setattr(
        "spice.studies.fileloc._load_sticky",
        lambda _root, git_path: {lock_path, sticky_source_path},
    )
    monkeypatch.setattr(
        "spice.studies.fileloc._save_sticky",
        lambda paths, _root, git_path: saved.setdefault(git_path, set(paths)),
    )

    findings = scan_staged_loc_violations(
        [lock_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
    )

    assert findings == []
    assert len(saved) == 2
    assert all(lock_path not in paths for paths in saved.values())
    assert all(sticky_source_path in paths for paths in saved.values())


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
    names = [
        "SPICE_" + "TASK_BACKEND",
        "CODEX_" + "THREAD_ID",
        "CLAUDE_" + "CODE_SESSION_ID",
    ]
    path = tmp_path / "sample.py"
    path.write_text(
        "\n".join(f'VALUE = os.environ["{name}"]' for name in names),
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


def _make_package(root: Path, name: str) -> None:
    pkg = root / name
    pkg.mkdir(parents=True, exist_ok=True)
    (pkg / "mod.py").write_text("x = 1\n", encoding="utf-8")


def test_package_roots_explicit_policy_overrides_derivation(tmp_path):
    _make_package(tmp_path, "chosen")
    _make_package(tmp_path, "ignored")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\npackage_roots = ["chosen"]\n'
        '[tool.setuptools.packages.find]\ninclude = ["ignored*"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "chosen"]


def test_package_roots_derived_from_setuptools_find_excludes_non_python(tmp_path):
    _make_package(tmp_path, "app")
    # A build artifact matching the include glob but carrying no Python.
    (tmp_path / "app.egg-info").mkdir()
    (tmp_path / "app.egg-info" / "PKG-INFO").write_text("meta\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools.packages.find]\nwhere = ["."]\ninclude = ["app*"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_derived_from_explicit_packages_list_keeps_top_level(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools]\npackages = ["app", "app.sub", "app.sub.deep"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_derived_from_poetry_packages(tmp_path):
    _make_package(tmp_path / "src", "pkg")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "pkg"\n'
        '[[tool.poetry.packages]]\ninclude = "pkg"\nfrom = "src"\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "pkg"]


def test_package_roots_derived_from_poetry_name(tmp_path):
    _make_package(tmp_path, "widget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "widget"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "widget"]


def test_package_roots_derived_from_hatch_wheel_packages(tmp_path):
    _make_package(tmp_path / "src", "gadget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.hatch.build.targets.wheel]\npackages = ["src/gadget"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "gadget"]


def test_package_roots_derived_from_hatch_build_include(tmp_path):
    _make_package(tmp_path / "packages", "gadget")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.hatch.build]\ninclude = ["packages/gadget"]\n',
        encoding="utf-8",
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "packages" / "gadget"]


def test_package_roots_derived_from_flit_module(tmp_path):
    _make_package(tmp_path, "thing")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.flit.module]\nname = "thing"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "thing"]


def test_package_roots_derived_from_pdm_includes(tmp_path):
    _make_package(tmp_path, "core")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.pdm.build]\nincludes = ["core"]\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "core"]


def test_package_roots_derived_from_src_layout(tmp_path):
    _make_package(tmp_path / "src", "lib")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "anything"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "src" / "lib"]


def test_package_roots_derived_from_project_name(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "app"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == [tmp_path / "app"]


def test_package_roots_empty_when_truly_underivable(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "different"\n', encoding="utf-8"
    )

    assert configured_package_roots(tmp_path) == []


def test_package_roots_malformed_poetry_packages_fails_loudly(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.poetry]\nname = "x"\npackages = "notalist"\n', encoding="utf-8"
    )

    with pytest.raises(SpiceError):
        configured_package_roots(tmp_path)


def test_python_typecheck_targets_follow_package_roots(tmp_path):
    _make_package(tmp_path, "app")
    (tmp_path / "pyproject.toml").write_text(
        '[tool.setuptools.packages.find]\nwhere = ["."]\ninclude = ["app*"]\n',
        encoding="utf-8",
    )

    assert python_typecheck_targets(tmp_path) == ("app",)


def test_python_typecheck_targets_empty_without_a_package(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "x"\n', encoding="utf-8"
    )

    assert python_typecheck_targets(tmp_path) == ()


def test_python_typecheck_argv_appends_fixed_flags_and_targets():
    argv = python_typecheck_argv(("app",))

    assert argv[-len(PYRIGHT_ARGS) - 1 :] == (*PYRIGHT_ARGS, "app")
    assert "pyright" in " ".join(argv)


def test_run_python_typecheck_noops_without_targets(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "x"\n', encoding="utf-8"
    )

    assert run_python_typecheck(tmp_path) is None
