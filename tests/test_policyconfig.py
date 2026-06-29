"""Tracked policy overlay resolution."""

from pathlib import Path

import pytest

from spice import policy
from spice.errors import SpiceError
from spice.policyconfig import resolve_policy

PROJECT_ROOT = Path(__file__).resolve().parents[1]
CUSTOM_FILE_LOC_LIMIT = 10
CUSTOM_FILE_BYTE_LIMIT = 100
CUSTOM_COMMIT_MESSAGE_WRAP = 72
CUSTOM_REPO_TRUTH_DOC_CHARS = 6000
CUSTOM_HOTSPOT_LIMIT = 7
CUSTOM_FILE_LOC_FLEX = 15
CUSTOM_FILE_BYTE_FLEX = 150
CUSTOM_MAGIC_THRESHOLD = 12
RATIO_FALLBACK_FILE_LOC_FLEX = 20
RATIO_FALLBACK_FILE_BYTE_FLEX = 200
RATIO_FALLBACK_CCN_FLEX = 10
RATIO_FALLBACK_LENGTH_FLEX = 20


def test_policy_resolver_defaults_match_policy_constants(tmp_path):
    resolved = resolve_policy(tmp_path)

    assert resolved.limits.file_loc == policy.FILE_LOC_LIMIT
    assert resolved.limits.file_bytes == policy.FILE_BYTE_LIMIT
    assert resolved.limits.routine_ccn == policy.COMPLEXITY_MAX_CCN
    assert resolved.limits.routine_length == policy.COMPLEXITY_MAX_LENGTH
    assert resolved.limits.commit_message_wrap == policy.COMMIT_MESSAGE_WRAP_LIMIT
    assert resolved.limits.repo_truth_doc_chars == policy.REPO_TRUTH_DOC_LIMIT
    assert resolved.flex.file_loc == policy.flex_limit(policy.FILE_LOC_LIMIT)
    assert resolved.flex.file_bytes == policy.flex_limit(policy.FILE_BYTE_LIMIT)
    assert resolved.flex.routine_ccn == policy.flex_limit(policy.COMPLEXITY_MAX_CCN)
    assert resolved.flex.routine_length == policy.flex_limit(
        policy.COMPLEXITY_MAX_LENGTH
    )
    assert resolved.complexity.hotspot_limit == policy.COMPLEXITY_HOTSPOT_LIMIT
    assert resolved.magic.examine_threshold == policy.MAGIC_EXAMINE_VALUE_THRESHOLD
    assert resolved.magic.baseline_ref == policy.MAGIC_BASELINE_REF
    assert resolved.debt.reachability_test_only == policy.REACHABILITY_TEST_ONLY_LIMIT
    assert resolved.debt.assertion_free_tests == policy.ASSERTION_FREE_TEST_LIMIT
    assert resolved.languages.complexity == policy.COMPLEXITY_SUFFIXES
    assert resolved.languages.magic == policy.MAGIC_SUFFIXES
    assert resolved.languages.env == policy.ENV_SUFFIXES
    assert resolved.languages.c_grammar == policy.C_GRAMMAR_SUFFIXES
    assert resolved.lockfiles.suffixes == policy.FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES
    assert resolved.lockfiles.names == policy.FILE_SHAPE_GENERATED_LOCKFILE_NAMES
    assert resolved.env_access.family_suffixes == policy.ENV_ACCESS_FAMILY_SUFFIXES
    assert resolved.env_access.default_patterns == policy.ENV_ACCESS_DEFAULT_PATTERNS
    assert resolved.env_access.baseline is None
    assert resolved.commit_message.wrap_limit == policy.COMMIT_MESSAGE_WRAP_LIMIT
    assert (
        resolved.commit_message.allowed_trailers
        == policy.COMMIT_MESSAGE_ALLOWED_TRAILER_KEYS
    )


def test_policy_resolver_applies_each_bound_override(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [tool.spice.policy.limits]
        file_loc = 10
        file_bytes = 100
        routine_ccn = 5
        routine_length = 8
        commit_message_wrap = 72
        repo_truth_doc_chars = 6000

        [tool.spice.policy.flex]
        ratio = 2.0
        file_loc = 15
        file_bytes = 150
        routine_ccn = 7
        routine_length = 9

        [tool.spice.policy.complexity]
        hotspot_limit = 7

        [tool.spice.policy.magic]
        examine_threshold = 12
        baseline_ref = "origin/main"

        [tool.spice.policy.debt]
        reachability_test_only = 2
        assertion_free_tests = 3

        [tool.spice.policy.languages]
        complexity = [".py"]
        magic = [".py", ".js"]
        env = [".sh"]
        c_grammar = [".c"]

        [tool.spice.policy.lockfiles]
        suffixes = [".lockx"]
        names = ["npm-lock.json"]

        [tool.spice.policy.env_access]
        baseline = "tools/spice/env-policy-baseline.json"

        [tool.spice.policy.env_access.family_suffixes]
        python = [".py", ".pyi"]

        [tool.spice.policy.env_access.default_patterns]
        python = ['Env\\.read']

        [tool.spice.policy.commit_message]
        allowed_trailers = ["Task", "Reviewed-By"]
        """,
    )

    resolved = resolve_policy(tmp_path)

    assert resolved.limits.file_loc == CUSTOM_FILE_LOC_LIMIT
    assert resolved.limits.file_bytes == CUSTOM_FILE_BYTE_LIMIT
    assert resolved.limits.routine_ccn == 5
    assert resolved.limits.routine_length == 8
    assert resolved.limits.commit_message_wrap == CUSTOM_COMMIT_MESSAGE_WRAP
    assert resolved.limits.repo_truth_doc_chars == CUSTOM_REPO_TRUTH_DOC_CHARS
    assert resolved.flex.ratio == 2.0
    assert resolved.file_shape.line_flex_limit == CUSTOM_FILE_LOC_FLEX
    assert resolved.file_shape.byte_flex_limit == CUSTOM_FILE_BYTE_FLEX
    assert resolved.complexity.ccn_flex_limit == 7
    assert resolved.complexity.length_flex_limit == 9
    assert resolved.complexity.hotspot_limit == CUSTOM_HOTSPOT_LIMIT
    assert resolved.magic.examine_threshold == CUSTOM_MAGIC_THRESHOLD
    assert resolved.magic.baseline_ref == "origin/main"
    assert resolved.debt.reachability_test_only == 2
    assert resolved.debt.assertion_free_tests == 3
    assert resolved.languages.complexity == (".py",)
    assert resolved.languages.magic == (".py", ".js")
    assert resolved.languages.env == (".sh",)
    assert resolved.languages.c_grammar == (".c",)
    assert resolved.lockfiles.suffixes == (".lockx",)
    assert resolved.lockfiles.names == ("npm-lock.json",)
    assert resolved.env_access.family_suffixes["python"] == (".py", ".pyi")
    assert resolved.env_access.default_patterns["python"] == (
        *policy.ENV_ACCESS_DEFAULT_PATTERNS["python"],
        "Env\\.read",
    )
    assert resolved.env_access.baseline == "tools/spice/env-policy-baseline.json"
    assert resolved.commit_message.wrap_limit == CUSTOM_COMMIT_MESSAGE_WRAP
    assert resolved.commit_message.allowed_trailers == frozenset(
        {"task", "reviewed-by"}
    )


def test_policy_resolver_uses_ratio_fallback_for_unset_flex(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [tool.spice.policy.limits]
        file_loc = 10
        file_bytes = 100
        routine_ccn = 5
        routine_length = 8

        [tool.spice.policy.flex]
        ratio = 2.0
        routine_length = 20
        """,
    )

    resolved = resolve_policy(tmp_path)

    assert resolved.file_shape.line_flex_limit == RATIO_FALLBACK_FILE_LOC_FLEX
    assert resolved.file_shape.byte_flex_limit == RATIO_FALLBACK_FILE_BYTE_FLEX
    assert resolved.complexity.ccn_flex_limit == RATIO_FALLBACK_CCN_FLEX
    assert resolved.complexity.length_flex_limit == RATIO_FALLBACK_LENGTH_FLEX


@pytest.mark.parametrize(
    ("body", "expected"),
    [
        (
            """
            [tool.spice.policy.limits]
            file_loc = "large"
            """,
            r"\[tool\.spice\.policy\.limits\] file_loc",
        ),
        (
            """
            [tool.spice.policy.magic]
            examine_threshold = 0
            """,
            r"\[tool\.spice\.policy\.magic\] examine_threshold",
        ),
        (
            """
            [tool.spice.policy.magic]
            baseline_ref = ""
            """,
            r"\[tool\.spice\.policy\.magic\] baseline_ref",
        ),
        (
            """
            [tool.spice.policy.complexity]
            hotspot_limit = 0
            """,
            r"\[tool\.spice\.policy\.complexity\] hotspot_limit",
        ),
    ],
)
def test_policy_resolver_names_invalid_config_key(tmp_path, body, expected):
    _write_pyproject(
        tmp_path,
        body,
    )

    with pytest.raises(SpiceError, match=expected):
        resolve_policy(tmp_path)


def test_policy_resolver_names_invalid_debt_key(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [tool.spice.policy.debt]
        reachability_test_only = -1
        """,
    )

    with pytest.raises(
        SpiceError,
        match=r"\[tool\.spice\.policy\.debt\] reachability_test_only",
    ):
        resolve_policy(tmp_path)


def test_policy_resolver_rejects_forbidden_commit_trailer_config(tmp_path):
    _write_pyproject(
        tmp_path,
        """
        [tool.spice.policy.commit_message]
        allowed_trailers = ["Co-Authored-By"]
        """,
    )

    with pytest.raises(
        SpiceError,
        match=r"\[tool\.spice\.policy\.commit_message\] allowed_trailers",
    ):
        resolve_policy(tmp_path)


def test_config_reference_mentions_tracked_policy_keys():
    text = (PROJECT_ROOT / "docs" / "config" / "reference.md").read_text(
        encoding="utf-8"
    )
    expected = [
        "[tool.spice.policy]",
        "package_roots",
        "name_cluster_threshold",
        "exclude",
        "generated_paths",
        "test_paths",
        "repo_truth_docs",
        "env_name_patterns",
        "env_names",
        "env_access_gate",
        "reachability_providers",
        "python_typecheck_interpreter",
        "assertion_helpers",
        "internal_couplings",
        "pre_commit",
        "pre_commit_success",
        "pre_commit_builtins",
        "[tool.spice.policy.limits]",
        "file_loc",
        "file_bytes",
        "routine_ccn",
        "routine_length",
        "commit_message_wrap",
        "repo_truth_doc_chars",
        "[tool.spice.policy.flex]",
        "ratio",
        "[tool.spice.policy.complexity]",
        "hotspot_limit",
        "[tool.spice.policy.magic]",
        "examine_threshold",
        "baseline_ref",
        "[tool.spice.policy.debt]",
        "reachability_test_only",
        "assertion_free_tests",
        "[tool.spice.policy.commit_message]",
        "allowed_trailers",
        "[tool.spice.policy.languages]",
        "c_grammar",
        "[tool.spice.policy.lockfiles]",
        "suffixes",
        "names",
        "[tool.spice.policy.env_access]",
        "family_suffixes",
        "default_patterns",
        "[tool.spice.policy.markdown_depth_budget]",
        "extensions",
        "stem_pattern",
        '[tool.spice.policy.scopes."<matcher>"]',
        "multiplier",
        "min",
        "max",
        "unlimited",
        "magic",
        "mount",
        "run",
        "argv",
        "when",
        "formatter",
        "enabled",
        "label",
        "path",
        "test",
        "target",
    ]

    missing = [item for item in expected if item not in text]

    assert missing == []


def _write_pyproject(root: Path, text: str) -> None:
    (root / "pyproject.toml").write_text(text, encoding="utf-8")
