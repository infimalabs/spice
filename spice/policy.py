"""The constitution: every opinion the harness enforces, in one place.

These constants are the product. The hooks, studies, docs, and tests all read
this module; changing a value here changes the enforced opinion everywhere at
once. Direct study commands may accept flags for focused investigation, but
the commit gates intentionally run the defaults here.

"""

from __future__ import annotations

from fractions import Fraction

from spice import defaults

# --- file shape pressure -----------------------------------------------------
# A file may grow to the flex limit, but one that ever breached it stays held
# to the base limit (sticky, rename-following) until it shrinks back under.
FILE_LOC_LIMIT = defaults.integer("policy", "limits", "file_loc")
FILE_BYTE_LIMIT = defaults.integer("policy", "limits", "file_bytes")
FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES = defaults.strings(
    "policy", "lockfiles", "suffixes"
)
FILE_SHAPE_GENERATED_LOCKFILE_NAMES = defaults.strings("policy", "lockfiles", "names")

# --- routine complexity ------------------------------------------------------
COMPLEXITY_MAX_CCN = defaults.integer("policy", "limits", "routine_ccn")
COMPLEXITY_MAX_LENGTH = defaults.integer("policy", "limits", "routine_length")
COMPLEXITY_HOTSPOT_LIMIT = defaults.integer("policy", "complexity", "hotspot_limit")

# --- flex --------------------------------------------------------------------
# flex limit = base * FLEX_NUMERATOR // FLEX_DENOMINATOR (1000 -> 1500).
_FLEX_RATIO = Fraction(defaults.number("policy", "flex", "ratio"))
FLEX_NUMERATOR = _FLEX_RATIO.numerator
FLEX_DENOMINATOR = _FLEX_RATIO.denominator

# --- commit messages ----------------------------------------------------------
# Subject must fit; body prose is auto-folded; URLs and allowed trailers are
# exempt. Spice bakes in no per-trailer opinion: every Git trailer -- including
# Co-Authored-By -- rides through untouched. A repo that wants a finite
# allowed-trailer set or specific blocked keys configures them under
# ``[tool.spice.policy.commit_message]``; ``None`` on both means no restriction.
COMMIT_MESSAGE_WRAP_LIMIT = defaults.integer("policy", "limits", "commit_message_wrap")
COMMIT_MESSAGE_ALLOWED_TRAILER_KEYS: tuple[str, ...] | None = None
COMMIT_MESSAGE_BLOCKED_TRAILER_KEYS: tuple[str, ...] | None = None

# --- taste ----------------------------------------------------------------------
# Low-value or poor-taste words mapped to a suggestion (empty = rephrase). A
# trailing ``*`` is a stem that matches every inflection (``migrat*`` ->
# migrate/migrated/migration); a bare key is whole-word. Repos merge their own words
# over these defaults under [tool.spice.policy.taste].
TASTE_WORD_SUGGESTIONS: dict[str, str] = {
    str(key): str(value)
    for key, value in defaults.table("policy", "taste", "words").items()
}

# --- repo-truth docs ------------------------------------------------------------
# Doctrine documents ride in every agent's context, so they are capped hard.
# A repo widens the set in tracked `[tool.spice.policy] repo_truth_docs`.
REPO_TRUTH_DOC_LIMIT = defaults.integer("policy", "limits", "repo_truth_doc_chars")
REPO_TRUTH_DOCS = defaults.strings("policy", "repo_truth", "docs")
MARKDOWN_DEPTH_DOC_EXTENSIONS = defaults.strings(
    "policy", "markdown_depth_budget", "extensions"
)
MARKDOWN_DEPTH_BASE_CHAR_BUDGET = defaults.integer(
    "policy", "markdown_depth", "base_chars"
)
MARKDOWN_DEPTH_MAX_BOUNDED_CHAR_BUDGET = defaults.integer(
    "policy", "markdown_depth", "max_bounded_chars"
)

# --- package shape -------------------------------------------------------------
# Namespace packages only: no __init__.py anywhere under a declared package
# root. Package path names match the boundary-underscore shape. Splitting a
# file requires naming the seam: generic continuation shards are rejected.
# A target repo declares its roots in tracked `pyproject.toml` under
# `[tool.spice.policy] package_roots`; repos without a declaration skip the
# Python package guards (the rest of the constitution still applies).
BOUNDARY_UNDERSCORE_PATTERN = defaults.string(
    "policy", "package", "boundary_underscore_pattern"
)

# --- test-quality gates --------------------------------------------------------
# Zero means the codebase is clean and any finding fails. Non-zero limits are
# explicit cleanup debt; lower the constant once the corresponding cleanup
# drains.
#
# Test-only findings: code reachable from tests but not from production roots.
# Held at zero: every test-only finding must be wired into production or deleted
# with its tests; `spice study reachability --create-tasks` files that decision.
REACHABILITY_TEST_ONLY_LIMIT = defaults.integer(
    "policy", "debt", "reachability_test_only"
)

# Assertion-free tests: test functions that do not appear to constrain behavior
# with an assert, pytest.raises/pytest.warns, pytest.fail, or assert* helper.
ASSERTION_FREE_TEST_LIMIT = defaults.integer("policy", "debt", "assertion_free_tests")

# JavaScript globals intentionally retained for browser validation must name the
# exact production declaration and why test ownership would sever the behavior
# under test. Symbol-name-only allowlists are too broad: the same name in a
# different file remains actionable.
JAVASCRIPT_UNUSED_DECLARATION_EXEMPTIONS: dict[tuple[str, str], str] = {
    (
        "spice/serve/static/app.mosaic-event-log.js",
        "mosaicReplayEventLog",
    ): (
        "shared browser replay oracle that exercises the production event-log "
        "branch implementation without duplicating that algorithm into tests"
    ),
}

# Product-shipped private-internals exceptions. Repo-specific exceptions belong
# in tracked `[tool.spice.policy].internal_couplings`, where they are visible to
# every clone and stale entries fail the gate.
LEGITIMATE_INTERNAL_COUPLINGS: frozenset[tuple[str, str, str]] = frozenset()

# --- magic numbers -------------------------------------------------------------
# Staged scans diff against this ref; only regressions fail.
MAGIC_BASELINE_REF = defaults.string("policy", "magic", "baseline_ref")
# Below this magnitude a literal explains itself (0/1/2, small counts, axis
# indices); at or above it a comparison pivot deserves a name.
MAGIC_EXAMINE_VALUE_THRESHOLD = defaults.integer("policy", "magic", "examine_threshold")

# --- environment literals ------------------------------------------------------
# Harness-owned env names may appear in source only on lines carrying this
# waiver. The scanner self-waives the module that defines the policy pattern.
ENV_POLICY_ALLOW_MARKER = defaults.string("policy", "env", "allow_marker")
ENV_POLICY_DEFAULT_NAME_PATTERNS = defaults.strings(
    "policy", "env", "default_name_patterns"
)
ENV_POLICY_SELF_PATH_SUFFIX = defaults.string("policy", "env", "self_path_suffix")

# Access gate language families. The gate audits env *access sites* (not just
# literal names), and the access idiom differs per language, so
# matchers are scoped by suffix family: a shell `$VAR` pattern must never run
# against `.cs`/`.js`. Built-in defaults below cover the standard idioms; a repo
# overrides or adds families through
# `[tool.spice.policy.env_access.default_patterns]` and
# `[tool.spice.policy.env_access.family_suffixes]`, never having to fork the
# study.
ENV_ACCESS_FAMILY_SUFFIXES = {
    str(key): tuple(str(item) for item in value)
    for key, value in defaults.table("policy", "env_access", "family_suffixes").items()
}
SHELL_ENV_ACCESS_NAME_PATTERN = r"(?:[A-Za-z][A-Za-z0-9_]*|_[A-Za-z0-9_]+)"
ENV_ACCESS_DEFAULT_PATTERNS = {
    str(key): tuple(str(item) for item in value)
    for key, value in defaults.table("policy", "env_access", "default_patterns").items()
}
ENV_ACCESS_FINDING_NAMES = {
    str(key): str(value)
    for key, value in defaults.table("policy", "env_access", "finding_names").items()
}

# --- language scope ------------------------------------------------------------
# spice gates repositories in any language; nothing here is Python-only.
# File shape pressure scans a broad source/text suffix set and then drops
# binary/non-text assets. These families scope the grammar-aware studies: the
# C-grammar family shares `//` + `/* */` comments and C comparison syntax, so
# the regex-backed magic-number scan holds across it (Python rides its own ast
# scan). Complexity covers every language lizard parses here. Env-literal
# inventory adds the shell family.
C_GRAMMAR_SUFFIXES = defaults.strings("policy", "languages", "c_grammar")
COMPLEXITY_SUFFIXES = defaults.strings("policy", "languages", "complexity")
MAGIC_SUFFIXES = defaults.strings("policy", "languages", "magic")
ENV_SUFFIXES = defaults.strings("policy", "languages", "env")
FILE_SHAPE_SOURCE_SUFFIXES = defaults.strings("policy", "file_shape", "source_suffixes")
FILE_SHAPE_GENERATED_SOURCE_PATTERNS = defaults.strings(
    "policy", "file_shape", "generated_patterns"
)


def flex_limit(limit: int) -> int:
    return limit * FLEX_NUMERATOR // FLEX_DENOMINATOR
