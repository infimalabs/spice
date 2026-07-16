"""Tracked policy rules with universal applicability selectors."""

from pathlib import Path
import subprocess

import pytest

from spice.agent.paths import write_agent_thread_pointer
from spice.errors import SpiceError
from spice.hooks import precommit
from spice.policyconfig import jittered_flex_limit, resolve_policy
from spice.studies.repodocs import (
    render_repo_truth_doc_lines,
    repo_truth_doc_findings,
)

BASE_FILE_LOC = 10
BASE_FILE_BYTES = 100
BASE_ROUTINE_CCN = 5
BASE_ROUTINE_LENGTH = 8
BASE_REPO_DOC_CHARS = 1000
BASE_MAGIC_THRESHOLD = 10
WIDE_FILE_LOC = 20
WIDE_FILE_LOC_FLEX = 40
WIDE_FILE_BYTES = 200
WIDE_FILE_BYTES_FLEX = 400
WIDE_ROUTINE_CCN = 10
WIDE_ROUTINE_CCN_FLEX = 20
WIDE_ROUTINE_LENGTH = 16
WIDE_ROUTINE_LENGTH_FLEX = 32
WIDE_REPO_DOC_CHARS = 2000
SCOPED_MAGIC_THRESHOLD = 100
CLAMPED_FILE_LOC = 25
CLAMPED_FILE_LOC_FLEX = 37
SCOPED_NEARBY_FILE_LOC = 20
SCOPED_SPECIFIC_FILE_LOC = 30
DOUBLE_STAR_FILE_LOC = 20
SCOPED_LOC_BREACH_LINES = 6
SCOPED_LOC_BASE_LIMIT = 4
UNLIMITED_FILE_LINES = 20
SCOPED_CCN_BREACH = 6
SCOPED_CCN_BASE_LIMIT = 4
MARKDOWN_ROOT_BUDGET = 10000
MARKDOWN_NESTED_BUDGET = 20000
MARKDOWN_DEEP_BUDGET = 30000
MARKDOWN_ROOT_FLEX = 15000
CUSTOM_SCOPE_DOC_BUDGET = 7000
JITTER_BASE_LIMIT = 100
JITTER_STATIC_FLEX = 200
JITTER_DOC_BASE_LIMIT = 100
JITTER_DOC_STATIC_FLEX = 200


def repo_truth_doc_violations(repo: Path) -> list[str]:
    return render_repo_truth_doc_lines(repo_truth_doc_findings(repo))


def test_policy_rules_apply_flat_settings_to_all_numeric_bounds(tmp_path):
    _write_pyproject(
        tmp_path,
        f"""
        [tool.spice.policy.limits]
        file_loc = {BASE_FILE_LOC}
        file_bytes = {BASE_FILE_BYTES}
        routine_ccn = {BASE_ROUTINE_CCN}
        routine_length = {BASE_ROUTINE_LENGTH}
        repo_truth_doc_chars = {BASE_REPO_DOC_CHARS}

        [tool.spice.policy.flex]
        ratio = 1.5

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["wide/**"] }}
        multiplier = 2.0
        flex = 2.0
        """,
    )

    resolved = resolve_policy(tmp_path)
    file_shape = resolved.file_shape_for_path(Path("wide/page.md"))
    routine_ccn = resolved.bound_for_path(
        "routine_ccn", resolved.limits.routine_ccn, Path("wide/app.py")
    )
    routine_length = resolved.bound_for_path(
        "routine_length", resolved.limits.routine_length, Path("wide/app.py")
    )

    assert file_shape.line_limit == WIDE_FILE_LOC
    assert file_shape.line_flex_limit == WIDE_FILE_LOC_FLEX
    assert file_shape.byte_limit == WIDE_FILE_BYTES
    assert file_shape.byte_flex_limit == WIDE_FILE_BYTES_FLEX
    assert routine_ccn.limit == WIDE_ROUTINE_CCN
    assert routine_ccn.flex_limit == WIDE_ROUTINE_CCN_FLEX
    assert routine_length.limit == WIDE_ROUTINE_LENGTH
    assert routine_length.flex_limit == WIDE_ROUTINE_LENGTH_FLEX
    assert (
        resolved.bound_for_path(
            "repo_truth_doc_chars",
            resolved.limits.repo_truth_doc_chars,
            Path("wide/AGENTS.md"),
        ).limit
        == WIDE_REPO_DOC_CHARS
    )


def test_policy_rules_named_bound_overrides_flat_settings_and_clamps(tmp_path):
    _write_pyproject(
        tmp_path,
        f"""
        [tool.spice.policy.limits]
        file_loc = {BASE_FILE_LOC}
        file_bytes = {BASE_FILE_BYTES}

        [tool.spice.policy.flex]
        ratio = 2.0

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["docs/**"] }}
        multiplier = 2.0

        [tool.spice.policy.rules.file_loc]
        multiplier = 3.0
        max = {CLAMPED_FILE_LOC}
        flex = 1.5
        """,
    )

    resolved = resolve_policy(tmp_path)
    file_shape = resolved.file_shape_for_path(Path("docs/page.md"))

    assert file_shape.line_limit == CLAMPED_FILE_LOC
    assert file_shape.line_flex_limit == CLAMPED_FILE_LOC_FLEX
    assert file_shape.byte_limit == WIDE_FILE_BYTES
    assert file_shape.byte_flex_limit == WIDE_FILE_BYTES_FLEX


def test_policy_rules_unlimited_marks_each_bound_exempt(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [[tool.spice.policy.rules]]
        scopes = { paths = ["generated/**"] }
        unlimited = true
        """,
    )

    resolved = resolve_policy(tmp_path)
    file_shape = resolved.file_shape_for_path(Path("generated/output.py"))
    routine_ccn = resolved.bound_for_path(
        "routine_ccn", resolved.limits.routine_ccn, Path("generated/output.py")
    )
    routine_length = resolved.bound_for_path(
        "routine_length", resolved.limits.routine_length, Path("generated/output.py")
    )

    assert file_shape.unlimited
    assert routine_ccn.unlimited
    assert routine_length.unlimited


def test_policy_rules_most_specific_selector_wins_per_bound(tmp_path):
    _write_pyproject(
        tmp_path,
        f"""
        [tool.spice.policy.limits]
        file_loc = {BASE_FILE_LOC}
        file_bytes = {BASE_FILE_BYTES}

        [tool.spice.policy.flex]
        ratio = 1.0

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["src/**"] }}
        multiplier = 2.0

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["src/legacy/**"] }}

        [tool.spice.policy.rules.file_loc]
        multiplier = 3.0
        """,
    )

    resolved = resolve_policy(tmp_path)
    nearby = resolved.file_shape_for_path(Path("src/app.py"))
    specific = resolved.file_shape_for_path(Path("src/legacy/app.py"))

    assert nearby.line_limit == SCOPED_NEARBY_FILE_LOC
    assert nearby.byte_limit == WIDE_FILE_BYTES
    assert specific.line_limit == SCOPED_SPECIFIC_FILE_LOC
    assert specific.byte_limit == WIDE_FILE_BYTES


def test_policy_rule_combines_canonical_subtree_and_extension_axes(tmp_path):
    _write_pyproject(
        tmp_path,
        f"""
        [tool.spice.policy.limits]
        file_loc = {BASE_FILE_LOC}

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["Docs"], extensions = [".md"] }}

        [tool.spice.policy.rules.file_loc]
        multiplier = 2.0
        """,
    )

    resolved = resolve_policy(tmp_path)

    assert (
        resolved.file_shape_for_path(Path("Docs/page.md")).line_limit
        == DOUBLE_STAR_FILE_LOC
    )
    assert (
        resolved.file_shape_for_path(Path("Docs/guides/page.md")).line_limit
        == DOUBLE_STAR_FILE_LOC
    )
    assert (
        resolved.file_shape_for_path(Path("Docs/page.txt")).line_limit == BASE_FILE_LOC
    )


def test_policy_rules_apply_magic_threshold_by_path(tmp_path):
    _write_pyproject(
        tmp_path,
        f"""
        [tool.spice.policy.magic]
        examine_threshold = {BASE_MAGIC_THRESHOLD}

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["**/*.cs"] }}
        magic.examine_threshold = {SCOPED_MAGIC_THRESHOLD}
        """,
    )

    resolved = resolve_policy(tmp_path)

    assert (
        resolved.magic_for_path(Path("src/Widget.cs")).examine_threshold
        == SCOPED_MAGIC_THRESHOLD
    )
    assert (
        resolved.magic_examine_threshold_for_path(Path("src/app.py"))
        == BASE_MAGIC_THRESHOLD
    )
    assert resolved.magic.examine_threshold == BASE_MAGIC_THRESHOLD


def test_policy_rule_invalid_magic_threshold_names_the_rule(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [[tool.spice.policy.rules]]
        scopes = { paths = ["**/*.cs"] }

        [tool.spice.policy.rules.magic]
        examine_threshold = 0
        """,
    )

    with pytest.raises(
        SpiceError,
        match=r"policy\.rules.*\[1\] magic examine_threshold",
    ):
        resolve_policy(tmp_path)


def test_policy_rule_invalid_payload_names_the_rule(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [[tool.spice.policy.rules]]
        scopes = { paths = ["src/**"] }
        flex = 0.5
        """,
    )

    with pytest.raises(
        SpiceError, match=r"policy\.rules.*\[1\] flex must be a number >= 1\.0"
    ):
        resolve_policy(tmp_path)


def test_markdown_depth_budget_generates_default_repo_doc_scopes(tmp_path):
    resolved = resolve_policy(tmp_path)

    root = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("README.md"),
    )
    nested = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("docs/guide.md"),
    )
    deep = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("docs/reference/guide.md"),
    )
    unbounded = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("docs/reference/generated/guide.md"),
    )

    assert root.limit == MARKDOWN_ROOT_BUDGET
    assert root.flex_limit == MARKDOWN_ROOT_FLEX
    assert nested.limit == MARKDOWN_NESTED_BUDGET
    assert deep.limit == MARKDOWN_DEEP_BUDGET
    assert unbounded.unlimited


def test_markdown_depth_budget_selector_gates_extension_regex_and_short_stems(
    tmp_path,
):
    _write_pyproject(
        tmp_path,
        """
        [tool.spice.policy.limits]
        repo_truth_doc_chars = 1000

        [tool.spice.policy.markdown_depth_budget]
        extensions = [".mdoc"]
        stem_pattern = "[A-Z_]+"
        """,
    )
    resolved = resolve_policy(tmp_path)

    scoped = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("DOCS_MAIN.mdoc"),
    )
    wrong_extension = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("DOCS_MAIN.md"),
    )
    wrong_stem = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("guide.mdoc"),
    )
    single_letter = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("A.mdoc"),
    )

    assert scoped.limit == MARKDOWN_ROOT_BUDGET
    assert wrong_extension.limit == BASE_REPO_DOC_CHARS
    assert wrong_stem.limit == BASE_REPO_DOC_CHARS
    assert single_letter.limit == BASE_REPO_DOC_CHARS


def test_markdown_depth_budget_explicit_rule_replaces_default_for_subtree(
    tmp_path,
):
    _write_pyproject(
        tmp_path,
        f"""
        [tool.spice.policy.limits]
        repo_truth_doc_chars = {BASE_REPO_DOC_CHARS}

        [[tool.spice.policy.rules]]
        scopes = {{ paths = ["docs/**"] }}

        [tool.spice.policy.rules.repo_truth_doc_chars]
        min = {CUSTOM_SCOPE_DOC_BUDGET}
        max = {CUSTOM_SCOPE_DOC_BUDGET}
        flex = 1.0
        """,
    )
    resolved = resolve_policy(tmp_path)
    scoped = resolved.bound_for_path(
        "repo_truth_doc_chars",
        resolved.limits.repo_truth_doc_chars,
        Path("docs/reference/guide.md"),
    )

    assert scoped.limit == CUSTOM_SCOPE_DOC_BUDGET
    assert scoped.flex_limit == CUSTOM_SCOPE_DOC_BUDGET


def test_file_shape_guard_applies_scoped_bounds_and_sticky(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        "file_loc = 2\n"
        "file_bytes = 100000\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 1.0\n"
        "\n"
        "[[tool.spice.policy.rules]]\n"
        'scopes = { paths = ["src/**"] }\n'
        "[tool.spice.policy.rules.file_loc]\n"
        "multiplier = 2.0\n"
        "flex = 1.25\n",
    )
    _write_repo_file(repo, "src/page.py", "line\n" * SCOPED_LOC_BREACH_LINES)
    _git(repo, "add", ".")

    with pytest.raises(
        SpiceError,
        match=f"{SCOPED_LOC_BREACH_LINES} lines > {SCOPED_LOC_BASE_LIMIT}",
    ):
        precommit._run_file_loc_guard(repo, [Path("src/page.py")])


def test_file_shape_scope_unlimited_exempts_generated_tree(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        "file_loc = 2\n"
        "file_bytes = 100\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 1.0\n"
        "\n"
        "[[tool.spice.policy.rules]]\n"
        'scopes = { paths = ["generated/**"] }\n'
        "unlimited = true\n",
    )
    output_path = Path("generated/output.py")
    _write_repo_file(
        repo, output_path.as_posix(), "print('large')\n" * UNLIMITED_FILE_LINES
    )
    _git(repo, "add", ".")

    assert resolve_policy(repo).file_shape_for_path(output_path).unlimited
    precommit._run_file_loc_guard(repo, [output_path])


def test_complexity_guard_applies_scoped_bounds_and_sticky(tmp_path, monkeypatch):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        "routine_ccn = 2\n"
        "routine_length = 20\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 1.0\n"
        "\n"
        "[[tool.spice.policy.rules]]\n"
        'scopes = { paths = ["src/legacy/**"] }\n'
        "[tool.spice.policy.rules.routine_ccn]\n"
        "multiplier = 2.0\n"
        "flex = 1.25\n",
    )
    _write_repo_file(repo, "src/legacy/app.py", "def run():\n    return 1\n")
    _git(repo, "add", ".")
    record = precommit.complexity.ComplexityRecord(
        path="src/legacy/app.py",
        function_name="run",
        ccn=SCOPED_CCN_BREACH,
        length=BASE_ROUTINE_LENGTH,
        nloc=BASE_ROUTINE_LENGTH,
    )
    monkeypatch.setattr(
        precommit.complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    with pytest.raises(
        SpiceError, match=f"ccn {SCOPED_CCN_BREACH} > {SCOPED_CCN_BASE_LIMIT}"
    ):
        precommit._run_complexity_guard(repo, [Path("src/legacy/app.py")])


def test_file_shape_guard_uses_actor_path_jittered_flex_ceiling(tmp_path):
    repo = _git_init(tmp_path / "repo")
    rel_path = Path("src/jitter.py")
    actor, jittered = _actor_with_lower_jittered_flex(
        rel_path, JITTER_BASE_LIMIT, JITTER_STATIC_FLEX
    )
    line_count = jittered + 1
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        f"file_loc = {JITTER_BASE_LIMIT}\n"
        "file_bytes = 100000\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 2.0\n",
    )
    _write_repo_file(repo, rel_path.as_posix(), "line\n" * line_count)
    write_agent_thread_pointer(repo, actor)
    _git(repo, "add", ".")

    assert line_count <= JITTER_STATIC_FLEX
    with pytest.raises(SpiceError, match=f"{line_count} lines > {JITTER_BASE_LIMIT}"):
        precommit._run_file_loc_guard(repo, [rel_path])


def test_complexity_guard_uses_actor_path_jittered_flex_ceiling(tmp_path, monkeypatch):
    repo = _git_init(tmp_path / "repo")
    rel_path = Path("src/jitter.py")
    actor, jittered = _actor_with_lower_jittered_flex(
        rel_path, JITTER_BASE_LIMIT, JITTER_STATIC_FLEX
    )
    ccn = jittered + 1
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy.limits]\n"
        f"routine_ccn = {JITTER_BASE_LIMIT}\n"
        "routine_length = 1000\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 2.0\n",
    )
    _write_repo_file(repo, rel_path.as_posix(), "def run():\n    return 1\n")
    write_agent_thread_pointer(repo, actor)
    _git(repo, "add", ".")
    record = precommit.complexity.ComplexityRecord(
        path=rel_path.as_posix(),
        function_name="run",
        ccn=ccn,
        length=1,
        nloc=1,
    )
    monkeypatch.setattr(
        precommit.complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    assert ccn <= JITTER_STATIC_FLEX
    with pytest.raises(SpiceError, match=f"ccn {ccn} > {JITTER_BASE_LIMIT}"):
        precommit._run_complexity_guard(repo, [rel_path])


def test_repo_doc_guard_uses_actor_path_jittered_flex_ceiling(tmp_path):
    repo = _git_init(tmp_path / "repo")
    rel_path = Path("AGENTS.md")
    actor, jittered = _actor_with_lower_jittered_flex(
        rel_path, JITTER_DOC_BASE_LIMIT, JITTER_DOC_STATIC_FLEX
    )
    char_count = jittered + 1
    _write_repo_file(
        repo,
        "pyproject.toml",
        "[tool.spice.policy]\n"
        'repo_truth_docs = ["AGENTS.md"]\n'
        "\n"
        "[tool.spice.policy.limits]\n"
        f"repo_truth_doc_chars = {JITTER_DOC_BASE_LIMIT}\n"
        "\n"
        "[tool.spice.policy.flex]\n"
        "ratio = 2.0\n"
        "\n"
        "[tool.spice.policy.markdown_depth_budget]\n"
        "extensions = []\n",
    )
    _write_repo_file(repo, rel_path.as_posix(), "x" * char_count)
    write_agent_thread_pointer(repo, actor)
    _git(repo, "add", ".")

    assert char_count <= JITTER_DOC_STATIC_FLEX
    assert repo_truth_doc_violations(repo) == [
        f"  {rel_path.as_posix()}: {char_count} characters "
        f"(cap {JITTER_DOC_BASE_LIMIT})"
    ]


def _actor_with_lower_jittered_flex(
    path: Path, base_limit: int, static_flex: int
) -> tuple[str, int]:
    for index in range(64):
        actor = f"actor-{index}"
        jittered = jittered_flex_limit(base_limit, static_flex, path, actor)
        if jittered < static_flex:
            return actor, jittered
    raise AssertionError("expected at least one actor to lower the flex ceiling")


def _write_pyproject(root: Path, text: str) -> None:
    (root / "pyproject.toml").write_text(text, encoding="utf-8")


def _git_init(repo: Path) -> Path:
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo


def _write_repo_file(repo: Path, rel_path: str, text: str) -> None:
    path = repo / rel_path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True)
