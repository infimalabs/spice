"""The constitution: every opinion the harness enforces, in one place.

These constants are the product. The hooks, studies, docs, and tests all read
this module; changing a value here changes the enforced opinion everywhere at
once. Direct study commands may accept flags for focused investigation, but
the commit gates intentionally run the defaults here.

Library seam: target-repo tools may import the public constants and
`flex_limit`; underscored names remain private.
"""

from __future__ import annotations

# --- file shape pressure -----------------------------------------------------
# A file may grow to the flex limit, but one that ever breached it stays held
# to the base limit (sticky, rename-following) until it shrinks back under.
FILE_LOC_LIMIT = 1000
FILE_BYTE_LIMIT = 80_000
FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES = (".lock",)
FILE_SHAPE_GENERATED_LOCKFILE_NAMES = (
    "bun.lockb",
    "package-lock.json",
    "pnpm-lock.yaml",
)

# --- routine complexity ------------------------------------------------------
COMPLEXITY_MAX_CCN = 20
COMPLEXITY_MAX_LENGTH = 80

# --- flex --------------------------------------------------------------------
# flex limit = base * FLEX_NUMERATOR // FLEX_DENOMINATOR (1000 -> 1500).
FLEX_NUMERATOR = 3
FLEX_DENOMINATOR = 2

# --- commit messages ----------------------------------------------------------
# Subject must fit; body prose is auto-folded; URLs and allowed trailers are
# exempt. Co-Authored-By is rejected.
COMMIT_MESSAGE_WRAP_LIMIT = 100

# --- repo-truth docs ------------------------------------------------------------
# Doctrine documents ride in every agent's context, so they are capped hard.
# A repo widens the set in tracked `[tool.spice.policy] repo_truth_docs`.
REPO_TRUTH_DOC_LIMIT = 5000
REPO_TRUTH_DOCS = ("AGENTS.md",)

# --- package shape -------------------------------------------------------------
# Namespace packages only: no __init__.py anywhere under a declared package
# root. Package path names match the boundary-underscore shape. Splitting a
# file requires naming the seam: generic continuation shards are rejected.
# A target repo declares its roots in tracked `pyproject.toml` under
# `[tool.spice.policy] package_roots`; repos without a declaration skip the
# Python package guards (the rest of the constitution still applies).
BOUNDARY_UNDERSCORE_PATTERN = r"^_*[0-9a-z]+_*$"

# --- test-quality ratchets -----------------------------------------------------
# Grandfathered baselines: CI fails on any *new* violation. Lower the constant
# once the corresponding cleanup drains. Zero means the codebase is currently
# clean; the only allowed direction is down.
#
# Test-only modules: modules reachable from tests but not from production roots.
# A non-zero baseline means that many modules are currently test-only exhaust;
# the gate refuses any new addition above the tolerance.
REACHABILITY_TEST_ONLY_LIMIT = 0

# Assertion-free tests: test functions that do not appear to constrain behavior
# with an assert, pytest.raises/pytest.warns, pytest.fail, or assert* helper.
ASSERTION_FREE_TEST_LIMIT = 0

# Private-internal coupling: the gate refuses any test that reaches into a
# production internal UNLESS that exact coupling is named below. This is not a
# grandfathered count — it is a set of specific (file, test, target) entries,
# each justified inline. A coupling that is not listed fails the gate (fix it
# with a public seam); a listed coupling that no longer exists also fails (a
# stale exception must be deleted). The only way the number grows is by adding
# a named, reasoned entry — never by bumping a tolerance.
LEGITIMATE_INTERNAL_COUPLINGS: frozenset[tuple[str, str, str]] = frozenset(
    {
        # The agent-run fast path deliberately bypasses the full CLI dispatcher
        # so it never imports the parser/inbox stack; asserting the bypass is
        # inherently about internal control flow and has no public observable.
        (
            "tests/test_agentrun.py",
            "test_agent_run_dispatch_bypasses_full_parser_and_inbox_import",
            "_dispatch",
        ),
        # Mount-routing precedence (a dotted mount chosen before builtin parse)
        # is a decision made inside the CLI dispatcher; the test exercises that
        # internal routing branch directly.
        (
            "tests/test_mounts.py",
            "test_dispatch_prefers_dotted_mount_before_builtin_parse",
            "_dispatch",
        ),
        # _existing_watch_paths is the pre-registration filter to paths that
        # exist on disk; the live bus exposes no public accessor for that
        # intermediate set, so the unit observes it directly.
        (
            "tests/test_livebus.py",
            "test_existing_watch_paths_returns_existing_input_paths",
            "_existing_watch_paths",
        ),
        # _kqueue is the OS-level kevent handle; verifying it re-registers only
        # when the watched-path set changes is inherently white-box — "the
        # kqueue rearmed" has no public surface to assert against.
        (
            "tests/test_livebus.py",
            "test_kqueue_watch_rearms_only_when_watched_paths_change",
            "_kqueue",
        ),
    }
)

# --- magic numbers -------------------------------------------------------------
# Staged scans diff against this ref; only regressions fail.
MAGIC_BASELINE_REF = "HEAD"
# Below this magnitude a literal explains itself (0/1/2, small counts, axis
# indices); at or above it a comparison pivot deserves a name.
MAGIC_EXAMINE_VALUE_THRESHOLD = 10

# --- environment literals ------------------------------------------------------
# Harness-owned env names may appear in source only on lines carrying this
# waiver. The scanner self-waives the module that defines the policy pattern.
ENV_POLICY_ALLOW_MARKER = "env-policy: allow"
ENV_POLICY_DEFAULT_NAME_PATTERNS = (  # env-policy: allow
    r"SPICE_[A-Z0-9_]+",
    r"CODEX_THREAD_ID",  # env-policy: allow
    r"CLAUDE_CODE_SESSION_ID",  # env-policy: allow
)
ENV_POLICY_SELF_PATH_SUFFIX = "studies/envpolicy.py"

# Presence reverse-gate language families. The reverse-gate audits env *access
# sites* (not just literal names), and the access idiom differs per language, so
# matchers are scoped by suffix family: a shell `$VAR` pattern must never run
# against `.cs`/`.js`. Built-in defaults below cover the standard idioms; a repo
# registers its own or additional idioms per family with
# `[tool.spice.policy] env_access_patterns` (e.g. a project's bespoke Lua
# runtime accessors), never having to fork the study.
ENV_ACCESS_FAMILY_SUFFIXES = {
    "python": (".py",),
    "csharp": (".cs",),
    "lua": (".lua",),
    "shell": (".bash", ".sh", ".zsh"),
}
ENV_ACCESS_DEFAULT_PATTERNS = {
    "python": (r"\bos\.(?:environ|getenv|putenv|unsetenv)\b",),  # env-policy: allow
    "csharp": (
        r"\b(?:System\.)?Environment\.(?:GetEnvironmentVariable|SetEnvironmentVariable)\b",
    ),
}
ENV_ACCESS_FINDING_NAMES = {
    "python": "os env access",
    "csharp": "environment env access",
    "lua": "lua env access",
    "shell": "shell env access",
}

# --- language scope ------------------------------------------------------------
# spice gates repositories in any language; nothing here is Python-only.
# File shape pressure is suffix-free. These families scope the grammar-aware
# studies: the C-grammar family shares `//` + `/* */` comments and C
# comparison syntax, so the regex-backed magic-number scan holds across it
# (Python rides its own ast scan). Complexity covers every language lizard
# parses here. Env-literal inventory adds the shell family.
C_GRAMMAR_SUFFIXES = (
    ".c",
    ".cc",
    ".cpp",
    ".cs",
    ".go",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".kt",
    ".m",
    ".mm",
    ".rs",
    ".scala",
    ".swift",
    ".ts",
)
COMPLEXITY_SUFFIXES = (*C_GRAMMAR_SUFFIXES, ".lua", ".php", ".py", ".rb")
MAGIC_SUFFIXES = (".py", *C_GRAMMAR_SUFFIXES)
ENV_SUFFIXES = (*COMPLEXITY_SUFFIXES, ".bash", ".sh", ".zsh")


def flex_limit(limit: int) -> int:
    return limit * FLEX_NUMERATOR // FLEX_DENOMINATOR
