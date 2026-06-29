"""Pre-commit gate pieces: repo-truth doc caps and their configuration."""

from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.hooks import precommit
from spice.hooks.precommit import (
    repo_truth_doc_violations,
    repo_truth_docs,
)
from spice.policy import REPO_TRUTH_DOC_LIMIT, REPO_TRUTH_DOCS

PROJECT_ROOT = Path(__file__).resolve().parents[1]

# Meta-ratchet: the exact, ordered set of built-in pre-commit guard keys. This
# is the authoritative gate registry. Removing, renaming, or dropping any guard
# breaks the assertion below and names the diff; adding a guard requires
# updating this list in the same commit that registers it. A gate may never be
# quietly deleted to make a task pass.
EXPECTED_BUILTIN_PRE_COMMIT_KEYS = [
    "repo-shape",
    "staging",
    "repo-docs",
    "formatters",
    "local-paths",
    "serve-web-typecheck",
    "python-typecheck",
    "env-policy",
    "env-name-ledger",
    "file-shape",
    "complexity",
    "magic-numbers",
    "reachability",
    "symbol-reachability",
    "assertion-free-tests",
    "private-internals",
]


def test_builtin_pre_commit_guard_registry_is_exactly_expected(tmp_path):
    actual = [step.key for step in precommit._builtin_pre_commit_steps(tmp_path, [])]
    missing = [key for key in EXPECTED_BUILTIN_PRE_COMMIT_KEYS if key not in actual]
    unexpected = [key for key in actual if key not in EXPECTED_BUILTIN_PRE_COMMIT_KEYS]
    assert actual == EXPECTED_BUILTIN_PRE_COMMIT_KEYS, (
        f"pre-commit guard registry drifted; missing guard(s): {missing or 'none'}; "
        f"unexpected guard(s): {unexpected or 'none'}. A gate may not be removed, "
        "renamed, or added without updating EXPECTED_BUILTIN_PRE_COMMIT_KEYS in the "
        "same commit."
    )


def test_private_internal_coupling_allowlist_is_exact_for_this_repo():
    """Against the real tree: every coupling the detector finds must be named in
    the built-in or tracked allowlist (no un-justified coupling), and every
    tracked allowlist entry must correspond to a coupling that still exists (no
    stale exception).
    The allowlist is a set of specific justified entries, never a frozen count.
    """
    from spice.policy import LEGITIMATE_INTERNAL_COUPLINGS
    from spice.studies import testquality

    findings = testquality.scan_private_internal_coupling(
        testquality.test_paths(PROJECT_ROOT), root=PROJECT_ROOT
    )
    present = {testquality.private_internal_coupling_key(f) for f in findings}
    configured = testquality.configured_internal_couplings(PROJECT_ROOT)
    allowed = LEGITIMATE_INTERNAL_COUPLINGS | configured
    unallowlisted = sorted(present - allowed)
    stale = sorted(configured - present)
    assert not unallowlisted, (
        "coupling(s) not in built-in or tracked internal_couplings (add a public "
        f"seam or a justified allowlist entry): {unallowlisted}"
    )
    assert not stale, (
        "stale tracked internal_couplings entr(ies) no longer present; delete "
        f"them so the allowlist stays a set of real exceptions: {stale}"
    )


def _write_coupling_repo(root):
    (root / "spice").mkdir()
    (root / "tests").mkdir()
    (root / "spice" / "foo.py").write_text("_secret = 1\n", encoding="utf-8")
    (root / "tests" / "test_foo.py").write_text(
        "from spice.foo import _secret\n\n"
        "def test_secret():\n    assert _secret == 1\n",
        encoding="utf-8",
    )


def _write_two_coupling_repo(root):
    (root / "spice").mkdir()
    (root / "tests").mkdir()
    (root / "spice" / "foo.py").write_text(
        "_secret = 1\n_other = 2\n", encoding="utf-8"
    )
    (root / "tests" / "test_foo.py").write_text(
        "from spice.foo import _secret, _other\n\n"
        "def test_secret():\n    assert (_secret, _other) == (1, 2)\n",
        encoding="utf-8",
    )


def test_private_internal_guard_allows_configured_internal_coupling(tmp_path):
    _write_coupling_repo(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "internal_couplings = [\n"
        '  { path = "tests/test_foo.py", test = "<module>", '
        'target = "spice.foo._secret" },\n'
        "]\n",
        encoding="utf-8",
    )

    assert precommit.quality_gate_failure(tmp_path, "coupling") is None


def test_private_internal_guard_still_fails_unlisted_coupling(tmp_path):
    _write_two_coupling_repo(tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "internal_couplings = [\n"
        '  { path = "tests/test_foo.py", test = "<module>", '
        'target = "spice.foo._secret" },\n'
        "]\n",
        encoding="utf-8",
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_private_internal_coupling_guard(tmp_path)

    message = str(exc_info.value)
    assert "private-internals: 1 coupling(s)" in message
    assert "spice.foo._other" in message


def test_private_internal_guard_reports_stale_configured_coupling(tmp_path):
    (tmp_path / "spice").mkdir()
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "test_foo.py").write_text(
        "def test_public():\n    assert 1 == 1\n", encoding="utf-8"
    )
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        "internal_couplings = [\n"
        '  { path = "tests/test_foo.py", test = "<module>", '
        'target = "spice.foo._secret" },\n'
        "]\n",
        encoding="utf-8",
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_private_internal_coupling_guard(tmp_path)

    message = str(exc_info.value)
    assert "configured internal_couplings entr(ies) stale" in message
    assert "tests/test_foo.py:<module>: spice.foo._secret" in message


def test_quality_gate_failure_reports_dirty_gate_and_none_when_clean(tmp_path):
    clean = tmp_path / "clean"
    clean.mkdir()
    (clean / "spice").mkdir()
    (clean / "tests").mkdir()
    assert precommit.quality_gate_failure(clean, "coupling") is None

    dirty = tmp_path / "dirty"
    dirty.mkdir()
    _write_coupling_repo(dirty)
    message = precommit.quality_gate_failure(dirty, "coupling")
    assert message is not None
    assert "spice.foo._secret" in message


def test_quality_gate_failure_rejects_unknown_gate(tmp_path):
    with pytest.raises(SpiceError, match="unknown quality gate"):
        precommit.quality_gate_failure(tmp_path, "bogus")


def test_quality_gate_failures_for_tags_only_runs_gate_tags(tmp_path):
    _write_coupling_repo(tmp_path)
    assert precommit.quality_gate_failures_for_tags(tmp_path, ["unrelated"]) == []
    failures = precommit.quality_gate_failures_for_tags(
        tmp_path, ["gate:coupling", "other"]
    )
    assert len(failures) == 1
    assert failures[0].startswith("[gate:coupling]")


def test_default_repo_truth_docs_apply_without_configuration(tmp_path):
    assert repo_truth_docs(tmp_path) == list(REPO_TRUTH_DOCS)


def test_declared_repo_truth_docs_override_the_default(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy]\nrepo_truth_docs = ["AGENTS.md", "TESTING.md"]\n',
        encoding="utf-8",
    )
    assert repo_truth_docs(tmp_path) == ["AGENTS.md", "TESTING.md"]


def test_doc_within_cap_reports_no_violations(tmp_path):
    (tmp_path / "AGENTS.md").write_text("short doctrine\n", encoding="utf-8")
    assert repo_truth_doc_violations(tmp_path) == []


def test_doc_over_cap_is_reported_as_a_violation(tmp_path):
    (tmp_path / "AGENTS.md").write_text(
        "x" * (REPO_TRUTH_DOC_LIMIT + 1), encoding="utf-8"
    )
    violations = repo_truth_doc_violations(tmp_path)
    assert len(violations) == 1
    assert "AGENTS.md" in violations[0]
    assert f"cap {REPO_TRUTH_DOC_LIMIT}" in violations[0]


def test_doc_cap_reads_configured_limit(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.limits]\nrepo_truth_doc_chars = 12\n",
        encoding="utf-8",
    )
    (tmp_path / "AGENTS.md").write_text("thirteen chars", encoding="utf-8")

    violations = repo_truth_doc_violations(tmp_path)

    assert len(violations) == 1
    assert "AGENTS.md" in violations[0]
    assert "cap 12" in violations[0]
