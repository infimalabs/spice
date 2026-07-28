"""Tracked policy overlay resolution."""

from pathlib import Path

import pytest

from spice import policy
from spice.errors import SpiceError
from spice.policyconfig import jittered_flex_limit, resolve_policy

PROJECT_ROOT = Path(__file__).resolve().parents[1]
CUSTOM_FILE_LOC_LIMIT = 10
CUSTOM_FILE_BYTE_LIMIT = 100
CUSTOM_COMMIT_MESSAGE_WRAP = 72
CUSTOM_REPO_TRUTH_DOC_CHARS = 6000
CUSTOM_HOTSPOT_LIMIT = 7
CUSTOM_FILE_LOC_FLEX = 15
CUSTOM_FILE_BYTE_FLEX = 150
CUSTOM_MAGIC_THRESHOLD = 12
CUSTOM_MARKDOWN_DEPTH_BASE_CHARS = 7000
CUSTOM_MARKDOWN_DEPTH_MAX_BOUNDED_CHARS = 15000
RATIO_FALLBACK_FILE_LOC_FLEX = 20
RATIO_FALLBACK_FILE_BYTE_FLEX = 200
RATIO_FALLBACK_CCN_FLEX = 10
RATIO_FALLBACK_LENGTH_FLEX = 20
JITTER_BASE_LIMIT = 100
JITTER_STATIC_FLEX = 200
DEFAULT_REPO_TRUTH_DOC_CHARS = 10_000
DEFAULT_MARKDOWN_DEPTH_BASE_CHARS = 10_000
DEFAULT_MARKDOWN_DEPTH_MAX_BOUNDED_CHARS = 30_000

POLICY_OVERRIDE_TOML = """
[policy.limits]
file_loc = 10
file_bytes = 100
routine_ccn = 5
routine_length = 8
commit_message_wrap = 72
repo_truth_doc_chars = 6000

[policy.flex]
ratio = 2.0
jitter_percent = 9
file_loc = 15
file_bytes = 150
routine_ccn = 7
routine_length = 9

[policy.complexity]
hotspot_limit = 7

[policy.magic]
examine_threshold = 12
baseline_ref = "origin/main"

[policy.debt]
reachability_test_only = 2
assertion_free_tests = 3

[policy.repo_truth]
docs = ["AGENTS.md", "TESTING.md"]

[policy.markdown_depth]
base_chars = 7000
max_bounded_chars = 15000

[policy.package]
boundary_underscore_pattern = '^x+$'

[policy.env]
allow_marker = "configured env waiver"
default_name_patterns = ["CUSTOM_[A-Z_]+"]
self_path_suffix = "tools/env_gate.py"

[policy.languages]
complexity = [".py"]
magic = [".py", ".js"]
env = [".sh"]
c_grammar = [".c"]

[policy.lockfiles]
suffixes = [".lockx"]
names = ["npm-lock.json"]

[policy.file_shape]
source_suffixes = [".tmpl"]
generated_patterns = ["generated/**"]

[policy.env_access]
baseline = "tools/spice/env-policy-baseline.json"

[policy.env_access.family_suffixes]
python = [".py", ".pyi"]

[policy.env_access.default_patterns]
python = ['Env\\.read']

[policy.env_access.finding_names]
python = "configured Python env access"

[policy.commit_message]
allowed_trailers = ["Task", "Reviewed-By"]
"""


def test_policy_resolver_defaults_match_policy_constants(tmp_path):
    resolved = resolve_policy(tmp_path)

    assert policy.REPO_TRUTH_DOC_LIMIT == DEFAULT_REPO_TRUTH_DOC_CHARS
    assert policy.MARKDOWN_DEPTH_BASE_CHAR_BUDGET == DEFAULT_MARKDOWN_DEPTH_BASE_CHARS
    assert (
        policy.MARKDOWN_DEPTH_MAX_BOUNDED_CHAR_BUDGET
        == DEFAULT_MARKDOWN_DEPTH_MAX_BOUNDED_CHARS
    )
    assert resolved.limits.file_loc == policy.FILE_LOC_LIMIT
    assert resolved.limits.file_bytes == policy.FILE_BYTE_LIMIT
    assert resolved.limits.routine_ccn == policy.COMPLEXITY_MAX_CCN
    assert resolved.limits.routine_length == policy.COMPLEXITY_MAX_LENGTH
    assert resolved.limits.commit_message_wrap == policy.COMMIT_MESSAGE_WRAP_LIMIT
    assert resolved.limits.repo_truth_doc_chars == policy.REPO_TRUTH_DOC_LIMIT
    assert resolved.flex.jitter_percent == 5
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
    assert resolved.markdown_depth.base_chars == (
        policy.MARKDOWN_DEPTH_BASE_CHAR_BUDGET
    )
    assert resolved.markdown_depth.max_bounded_chars == (
        policy.MARKDOWN_DEPTH_MAX_BOUNDED_CHAR_BUDGET
    )
    assert resolved.repo_truth_docs == policy.REPO_TRUTH_DOCS
    assert resolved.boundary_underscore_pattern == policy.BOUNDARY_UNDERSCORE_PATTERN
    assert resolved.environment.allow_marker == policy.ENV_POLICY_ALLOW_MARKER
    assert (
        resolved.environment.default_name_patterns
        == policy.ENV_POLICY_DEFAULT_NAME_PATTERNS
    )
    assert resolved.environment.self_path_suffix == policy.ENV_POLICY_SELF_PATH_SUFFIX
    assert resolved.languages.complexity == policy.COMPLEXITY_SUFFIXES
    assert resolved.languages.magic == policy.MAGIC_SUFFIXES
    assert resolved.languages.env == policy.ENV_SUFFIXES
    assert resolved.languages.c_grammar == policy.C_GRAMMAR_SUFFIXES
    assert resolved.lockfiles.suffixes == policy.FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES
    assert resolved.lockfiles.names == policy.FILE_SHAPE_GENERATED_LOCKFILE_NAMES
    assert (
        resolved.file_shape_paths.source_suffixes == policy.FILE_SHAPE_SOURCE_SUFFIXES
    )
    assert (
        resolved.file_shape_paths.generated_patterns
        == policy.FILE_SHAPE_GENERATED_SOURCE_PATTERNS
    )
    assert resolved.env_access.family_suffixes == policy.ENV_ACCESS_FAMILY_SUFFIXES
    assert resolved.env_access.default_patterns == policy.ENV_ACCESS_DEFAULT_PATTERNS
    assert resolved.env_access.finding_names == policy.ENV_ACCESS_FINDING_NAMES
    assert resolved.env_access.baseline is None
    assert resolved.commit_message.wrap_limit == policy.COMMIT_MESSAGE_WRAP_LIMIT
    assert (
        resolved.commit_message.allowed_trailers
        == policy.COMMIT_MESSAGE_ALLOWED_TRAILER_KEYS
    )
    assert resolved.taste.words == policy.TASTE_WORD_SUGGESTIONS


def test_jittered_flex_limit_is_stable_bounded_and_actor_scoped():
    path = Path("./src\\app.py")
    normalized_path = Path("src/app.py")

    first = jittered_flex_limit(JITTER_BASE_LIMIT, JITTER_STATIC_FLEX, path, "actor-a")
    second = jittered_flex_limit(JITTER_BASE_LIMIT, JITTER_STATIC_FLEX, path, "actor-a")
    normalized = jittered_flex_limit(
        JITTER_BASE_LIMIT, JITTER_STATIC_FLEX, normalized_path, "actor-a"
    )
    actor_values = {
        jittered_flex_limit(
            JITTER_BASE_LIMIT,
            JITTER_STATIC_FLEX,
            normalized_path,
            f"actor-{index}",
        )
        for index in range(24)
    }

    assert first == second
    assert first == normalized
    assert JITTER_STATIC_FLEX - 5 <= first <= JITTER_STATIC_FLEX + 5
    assert len(actor_values) > 1
    assert (
        jittered_flex_limit(
            JITTER_BASE_LIMIT,
            JITTER_BASE_LIMIT,
            normalized_path,
            "actor-a",
        )
        == JITTER_BASE_LIMIT
    )


def test_policy_resolver_merges_taste_words_over_defaults(tmp_path):
    _write_repository_config(
        tmp_path,
        """
        [policy.taste.words]
        Smell = ""
        just = "reword"
        Whitelisting = "permission-listing"
        """,
    )

    resolved = resolve_policy(tmp_path)

    expected = dict(policy.TASTE_WORD_SUGGESTIONS)
    expected.update(
        {
            "smell": "",
            "just": "reword",
            "whitelisting": "permission-listing",
        }
    )
    assert resolved.taste.words == expected


def test_policy_resolver_applies_each_bound_override(tmp_path):
    _write_repository_config(tmp_path, POLICY_OVERRIDE_TOML)

    resolved = resolve_policy(tmp_path)

    assert resolved.limits.file_loc == CUSTOM_FILE_LOC_LIMIT
    assert resolved.limits.file_bytes == CUSTOM_FILE_BYTE_LIMIT
    assert resolved.limits.routine_ccn == 5
    assert resolved.limits.routine_length == 8
    assert resolved.limits.commit_message_wrap == CUSTOM_COMMIT_MESSAGE_WRAP
    assert resolved.limits.repo_truth_doc_chars == CUSTOM_REPO_TRUTH_DOC_CHARS
    assert resolved.flex.ratio == 2.0
    assert resolved.flex.jitter_percent == 9
    assert resolved.file_shape.line_flex_limit == CUSTOM_FILE_LOC_FLEX
    assert resolved.file_shape.byte_flex_limit == CUSTOM_FILE_BYTE_FLEX
    assert resolved.complexity.ccn_flex_limit == 7
    assert resolved.complexity.length_flex_limit == 9
    assert resolved.complexity.hotspot_limit == CUSTOM_HOTSPOT_LIMIT
    assert resolved.magic.examine_threshold == CUSTOM_MAGIC_THRESHOLD
    assert resolved.magic.baseline_ref == "origin/main"
    assert resolved.debt.reachability_test_only == 2
    assert resolved.debt.assertion_free_tests == 3
    assert resolved.repo_truth_docs == ("AGENTS.md", "TESTING.md")
    assert resolved.markdown_depth.base_chars == CUSTOM_MARKDOWN_DEPTH_BASE_CHARS
    assert (
        resolved.markdown_depth.max_bounded_chars
        == CUSTOM_MARKDOWN_DEPTH_MAX_BOUNDED_CHARS
    )
    assert resolved.boundary_underscore_pattern == "^x+$"
    assert resolved.environment.allow_marker == "configured env waiver"
    assert resolved.environment.default_name_patterns == ("CUSTOM_[A-Z_]+",)
    assert resolved.environment.self_path_suffix == "tools/env_gate.py"
    assert resolved.languages.complexity == (".py",)
    assert resolved.languages.magic == (".py", ".js")
    assert resolved.languages.env == (".sh",)
    assert resolved.languages.c_grammar == (".c",)
    assert resolved.lockfiles.suffixes == (".lockx",)
    assert resolved.lockfiles.names == ("npm-lock.json",)
    assert resolved.file_shape_paths.source_suffixes == (".tmpl",)
    assert resolved.file_shape_paths.generated_patterns == ("generated/**",)
    assert resolved.env_access.family_suffixes["python"] == (".py", ".pyi")
    assert resolved.env_access.default_patterns["python"] == (
        *policy.ENV_ACCESS_DEFAULT_PATTERNS["python"],
        "Env\\.read",
    )
    assert resolved.env_access.finding_names["python"] == "configured Python env access"
    assert resolved.env_access.baseline == "tools/spice/env-policy-baseline.json"
    assert resolved.commit_message.wrap_limit == CUSTOM_COMMIT_MESSAGE_WRAP
    assert resolved.commit_message.allowed_trailers == frozenset(
        {"task", "reviewed-by"}
    )


def test_policy_resolver_uses_ratio_fallback_for_unset_flex(tmp_path):
    _write_repository_config(
        tmp_path,
        """
        [policy.limits]
        file_loc = 10
        file_bytes = 100
        routine_ccn = 5
        routine_length = 8

        [policy.flex]
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
            [policy.limits]
            file_loc = "large"
            """,
            r"policy\.limits\.file_loc \(source=repository path=.*spice\.toml\)",
        ),
        (
            """
            [policy.magic]
            examine_threshold = 0
            """,
            r"policy\.magic\.examine_threshold \(source=repository path=.*spice\.toml\)",
        ),
        (
            """
            [policy.magic]
            baseline_ref = ""
            """,
            r"policy\.magic\.baseline_ref \(source=repository path=.*spice\.toml\)",
        ),
        (
            """
            [policy.complexity]
            hotspot_limit = 0
            """,
            r"policy\.complexity\.hotspot_limit \(source=repository path=.*spice\.toml\)",
        ),
    ],
)
def test_policy_resolver_names_invalid_config_key(tmp_path, body, expected):
    _write_repository_config(
        tmp_path,
        body,
    )

    with pytest.raises(SpiceError, match=expected):
        resolve_policy(tmp_path)


def test_policy_resolver_names_invalid_debt_key(tmp_path):
    _write_repository_config(
        tmp_path,
        """
        [policy.debt]
        reachability_test_only = -1
        """,
    )

    with pytest.raises(
        SpiceError,
        match=r"policy\.debt\.reachability_test_only \(source=repository path=.*spice\.toml\)",
    ):
        resolve_policy(tmp_path)


def test_policy_resolver_allows_explicit_co_authored_by_trailer(tmp_path):
    _write_repository_config(
        tmp_path,
        """
        [policy.commit_message]
        allowed_trailers = ["Task", "Co-Authored-By"]
        """,
    )

    resolved = resolve_policy(tmp_path)

    assert resolved.commit_message.allowed_trailers == frozenset(
        {"task", "co-authored-by"}
    )


def test_config_reference_mentions_tracked_policy_keys():
    text = (PROJECT_ROOT / "docs" / "config" / "reference.md").read_text(
        encoding="utf-8"
    )
    expected = [
        "[policy]",
        "package_roots",
        "name_cluster_threshold",
        "exclude",
        "generated_paths",
        "test_paths",
        "repo_truth.docs",
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
        "[policy.limits]",
        "file_loc",
        "file_bytes",
        "routine_ccn",
        "routine_length",
        "commit_message_wrap",
        "repo_truth_doc_chars",
        "[policy.flex]",
        "ratio",
        "[policy.complexity]",
        "hotspot_limit",
        "[policy.magic]",
        "examine_threshold",
        "baseline_ref",
        "[policy.debt]",
        "reachability_test_only",
        "assertion_free_tests",
        "[policy.commit_message]",
        "allowed_trailers",
        "[policy.languages]",
        "c_grammar",
        "[policy.lockfiles]",
        "suffixes",
        "names",
        "[policy.file_shape]",
        "source_suffixes",
        "generated_patterns",
        "[policy.env_access]",
        "family_suffixes",
        "default_patterns",
        "[policy.markdown_depth_budget]",
        "extensions",
        "stem_pattern",
        "[[policy.rules]]",
        "scopes = { paths",
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


def _write_repository_config(root: Path, text: str) -> None:
    (root / "spice.toml").write_text(text, encoding="utf-8")
