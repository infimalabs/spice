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
COMPLEXITY_HOTSPOT_LIMIT = 20

# --- flex --------------------------------------------------------------------
# flex limit = base * FLEX_NUMERATOR // FLEX_DENOMINATOR (1000 -> 1500).
FLEX_NUMERATOR = 3
FLEX_DENOMINATOR = 2

# --- commit messages ----------------------------------------------------------
# Subject must fit; body prose is auto-folded; URLs and allowed trailers are
# exempt. ``None`` keeps the legacy policy: any Git trailer is allowed except
# Co-Authored-By. Repos may configure a finite allowed-trailer set.
COMMIT_MESSAGE_WRAP_LIMIT = 100
COMMIT_MESSAGE_ALLOWED_TRAILER_KEYS: tuple[str, ...] | None = None

# --- repo-truth docs ------------------------------------------------------------
# Doctrine documents ride in every agent's context, so they are capped hard.
# A repo widens the set in tracked `[tool.spice.policy] repo_truth_docs`.
REPO_TRUTH_DOC_LIMIT = 5000
REPO_TRUTH_DOCS = ("AGENTS.md",)
MARKDOWN_DEPTH_DOC_EXTENSIONS = (".md",)
MARKDOWN_DEPTH_BASE_CHAR_BUDGET = 5000
MARKDOWN_DEPTH_MAX_BOUNDED_CHAR_BUDGET = 15000

# --- package shape -------------------------------------------------------------
# Namespace packages only: no __init__.py anywhere under a declared package
# root. Package path names match the boundary-underscore shape. Splitting a
# file requires naming the seam: generic continuation shards are rejected.
# A target repo declares its roots in tracked `pyproject.toml` under
# `[tool.spice.policy] package_roots`; repos without a declaration skip the
# Python package guards (the rest of the constitution still applies).
BOUNDARY_UNDERSCORE_PATTERN = r"^_*[0-9a-z]+_*$"

# --- test-quality gates --------------------------------------------------------
# Zero means the codebase is clean and any finding fails. Non-zero limits are
# explicit cleanup debt; lower the constant once the corresponding cleanup
# drains.
#
# Test-only findings: code reachable from tests but not from production roots.
# Held at zero: every test-only finding must be wired into production or deleted
# with its tests; `spice study reachability --create-tasks` files that decision.
REACHABILITY_TEST_ONLY_LIMIT = 0

# Assertion-free tests: test functions that do not appear to constrain behavior
# with an assert, pytest.raises/pytest.warns, pytest.fail, or assert* helper.
ASSERTION_FREE_TEST_LIMIT = 0

# Product-shipped private-internals exceptions. Repo-specific exceptions belong
# in tracked `[tool.spice.policy].internal_couplings`, where they are visible to
# every clone and stale entries fail the gate.
LEGITIMATE_INTERNAL_COUPLINGS: frozenset[tuple[str, str, str]] = frozenset()

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

# Access gate language families. The gate audits env *access sites* (not just
# literal names), and the access idiom differs per language, so
# matchers are scoped by suffix family: a shell `$VAR` pattern must never run
# against `.cs`/`.js`. Built-in defaults below cover the standard idioms; a repo
# overrides or adds families through
# `[tool.spice.policy.env_access.default_patterns]` and
# `[tool.spice.policy.env_access.family_suffixes]`, never having to fork the
# study.
ENV_ACCESS_FAMILY_SUFFIXES = {
    "python": (".py",),
    "csharp": (".cs",),
    "lua": (".lua",),
    "shell": (".bash", ".sh", ".zsh"),
    "javascript": (".js", ".ts"),
}
SHELL_ENV_ACCESS_NAME_PATTERN = r"(?:[A-Za-z][A-Za-z0-9_]*|_[A-Za-z0-9_]+)"
ENV_ACCESS_DEFAULT_PATTERNS = {
    "python": (r"\bos\.(?:environ|getenv|putenv|unsetenv)\b",),  # env-policy: allow
    "csharp": (
        r"\b(?:System\.)?Environment\.(?:GetEnvironmentVariable|SetEnvironmentVariable)\b",
    ),
    "lua": (r"\bos\.getenv\b",),  # env-policy: allow
    "shell": (
        rf"(?<!\\)\$(?:{SHELL_ENV_ACCESS_NAME_PATTERN}|\{{{SHELL_ENV_ACCESS_NAME_PATTERN}\}})",
        rf"\bexport\s+{SHELL_ENV_ACCESS_NAME_PATTERN}=",
    ),
    # `\bprocess\.env\b` covers dot-access, bracket-access, bare reads, and
    # destructuring (`const {X} = process.env`) in one idiom.
    "javascript": (r"\bprocess\.env\b",),
}
ENV_ACCESS_FINDING_NAMES = {
    "python": "os env access",
    "csharp": "environment env access",
    "lua": "lua env access",
    "shell": "shell env access",
    "javascript": "process.env access",
}

# --- language scope ------------------------------------------------------------
# spice gates repositories in any language; nothing here is Python-only.
# File shape pressure scans a broad source/text suffix set and then drops
# binary/non-text assets. These families scope the grammar-aware studies: the
# C-grammar family shares `//` + `/* */` comments and C comparison syntax, so
# the regex-backed magic-number scan holds across it (Python rides its own ast
# scan). Complexity covers every language lizard parses here. Env-literal
# inventory adds the shell family.
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
FILE_SHAPE_SOURCE_SUFFIXES = (
    *ENV_SUFFIXES,
    ".astro",
    ".cjs",
    ".cts",
    ".css",
    ".html",
    ".json",
    ".jsx",
    ".less",
    ".md",
    ".mjs",
    ".mts",
    ".pyi",
    ".rst",
    ".sass",
    ".scss",
    ".sql",
    ".svelte",
    ".toml",
    ".tsx",
    ".txt",
    ".vue",
    ".xml",
    ".yaml",
    ".yml",
)
FILE_SHAPE_GENERATED_SOURCE_PATTERNS = (
    "**/*.generated.*",
    "**/*_generated.*",
    "**/*.g.*",
    "**/*_pb2.py",
    "**/*_pb2_grpc.py",
    "**/*.min.css",
    "**/*.min.js",
    "build/**",
    "coverage/**",
    "dist/**",
)


def flex_limit(limit: int) -> int:
    return limit * FLEX_NUMERATOR // FLEX_DENOMINATOR
