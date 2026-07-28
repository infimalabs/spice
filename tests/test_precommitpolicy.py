"""Pre-commit policy, debt, and repo-truth gate configuration tests."""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

from spice import defaults
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
from spice.agent.paths import write_agent_thread_pointer
from spice.errors import SpiceError
from spice.flexstate import (
    FLEX_SLICE_CLAIM_TTL_SECONDS,
    FlexSliceClaim,
    git_state_path,
    save_flex_slice_claims,
)
from spice.hooks import precommit
from spice.studies import gates, taste
from spice.studies.complexity import (
    COMPLEXITY_CCN_STICKY_GIT_PATH,
    COMPLEXITY_LENGTH_STICKY_GIT_PATH,
)
from spice.studies.fileloc import (
    FILE_BYTE_STICKY_STATE_GIT_PATH,
    FILE_LOC_STICKY_STATE_GIT_PATH,
    LocFinding,
)
from spice.studies.repodocs import (
    REPO_DOC_CHAR_STICKY_STATE_GIT_PATH,
    render_repo_truth_doc_lines,
    repo_truth_doc_findings,
    repo_truth_docs,
)
from spice.policy import REPO_TRUTH_DOC_LIMIT, REPO_TRUTH_DOCS
from spice.policyconfig import resolve_policy
from tests.test_configtrusthelpers import approve_repository_config

PROJECT_ROOT = Path(__file__).resolve().parents[1]

BUILTIN_PRE_COMMIT_LABELS = [
    "merge integrity",
    "plan phase",
    "repo shape",
    "staging",
    "repo docs",
    "formatters",
    "local paths",
    "serve web typecheck",
    "javascript unused",
    "env policy",
    "env name ledger",
    "file shape",
    "complexity",
    "magic numbers",
    "markdown links",
    "reachability",
    "symbol reachability",
    "python unused",
    "assertion-free tests",
    "private internals",
]

EXPECTED_BUILTIN_PRE_COMMIT_KEYS = [
    "merge-integrity",
    "plan-phase",
    "repo-shape",
    "staging",
    "repo-docs",
    "formatters",
    "local-paths",
    "taste",
    "serve-web-typecheck",
    "javascript-unused",
    "python-typecheck",
    "env-policy",
    "env-name-ledger",
    "file-shape",
    "complexity",
    "magic-numbers",
    "markdown-links",
    "reachability",
    "symbol-reachability",
    "python-unused",
    "assertion-free-tests",
    "private-internals",
]

STICKY_LEDGER_GIT_PATHS = (
    FILE_LOC_STICKY_STATE_GIT_PATH,
    FILE_BYTE_STICKY_STATE_GIT_PATH,
    COMPLEXITY_CCN_STICKY_GIT_PATH,
    COMPLEXITY_LENGTH_STICKY_GIT_PATH,
    REPO_DOC_CHAR_STICKY_STATE_GIT_PATH,
)
# A file-shape gate sized so one staged path sits over the flex ceiling (a
# breach, which latches) and another sits inside the flex band (retained only
# while a latch already names it).
LATCH_PROBE_BASE_LOC = 2
LATCH_PROBE_FLEX_RATIO = 2.0
LATCH_PROBE_BYTE_LOC = 1000000
LATCH_PROBE_BREACH_LINES = 5
LATCH_PROBE_BAND_LINES = 3
# A flex slice is only ever claimed on behalf of a named worktree, so the probe
# repo needs the agent thread pointer a real one carries.
LATCH_PROBE_THREAD_ID = "0123456789abcdef0123456789abcdef"


def repo_truth_doc_violations(repo: Path) -> list[str]:
    return render_repo_truth_doc_lines(repo_truth_doc_findings(repo))


def test_builtin_pre_commit_guard_registry_is_exactly_expected(tmp_path):
    actual = [step.key for step in precommit._builtin_pre_commit_steps(tmp_path, [])]
    packaged = list(defaults.table("policy", "pre_commit_builtins"))
    missing = [key for key in EXPECTED_BUILTIN_PRE_COMMIT_KEYS if key not in actual]
    unexpected = [key for key in actual if key not in EXPECTED_BUILTIN_PRE_COMMIT_KEYS]
    assert actual == EXPECTED_BUILTIN_PRE_COMMIT_KEYS, (
        f"pre-commit guard registry drifted; missing guard(s): {missing or 'none'}; "
        f"unexpected guard(s): {unexpected or 'none'}. A gate may not be removed, "
        "renamed, or added without updating EXPECTED_BUILTIN_PRE_COMMIT_KEYS in the "
        "same commit."
    )
    assert packaged == actual, (
        "every built-in pre-commit step must have a packaged registry entry so "
        "the shared false-disable resolver can remove it"
    )


def test_config_reference_documents_pre_commit_keys_and_taste_contract():
    text = (PROJECT_ROOT / "docs" / "config" / "reference.md").read_text(
        encoding="utf-8"
    )
    builtins_row = next(
        line
        for line in text.splitlines()
        if line.startswith("| `pre_commit_builtins` ")
    )
    documented = [
        key for key in EXPECTED_BUILTIN_PRE_COMMIT_KEYS if f"`{key}`" in builtins_row
    ]

    assert documented == EXPECTED_BUILTIN_PRE_COMMIT_KEYS
    assert "### `[policy.taste.words]`" in text
    assert "gate-only pre-commit built-in" in text
    assert "`policy.TASTE_WORD_SUGGESTIONS`" in text
    assert "whole word" in text
    assert "trailing `*`" in text
    assert "starts from the built-in map" in text
    assert "assigns repository entries in TOML order" in text
    assert "`allowlist`" in text
    assert "`blocklist`" in text


def test_taste_pre_commit_guard_reports_exact_inclusive_inflection(tmp_path):
    doc = tmp_path / "notes.md"
    doc.write_text("Replace BLACKLISTING in this guide.\n", encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_taste_guard(tmp_path, [Path("notes.md")])

    assert str(exc_info.value) == "\n".join(
        [
            "taste: 1 low-value or poor-taste word(s); rephrase for better taste",
            "  FAIL  notes.md:1  'blacklisting' -> consider 'blocklisting'",
        ]
    )


def test_tracked_taste_policy_accepts_precise_timeout_threshold_prose(tmp_path):
    doc = tmp_path / "threshold.md"
    doc.write_text(
        "The peer sends bytes just under the timeout threshold.\n",
        encoding="utf-8",
    )
    configured_words = dict(resolve_policy(PROJECT_ROOT).taste.words)
    findings = taste.scan_taste([doc], root=tmp_path, words=configured_words)

    assert taste.render_taste_board(findings) == "taste: ok"


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
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        "internal_couplings = [\n"
        '  { path = "tests/test_foo.py", test = "<module>", '
        'target = "spice.foo._secret" },\n'
        "]\n",
        encoding="utf-8",
    )

    assert precommit.quality_gate_failure(tmp_path, "coupling") is None


def test_private_internal_guard_still_fails_unlisted_coupling(tmp_path):
    _write_two_coupling_repo(tmp_path)
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
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
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
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
    (tmp_path / "spice.toml").write_text(
        '[policy]\nrepo_truth_docs = ["AGENTS.md", "TESTING.md"]\n',
        encoding="utf-8",
    )
    assert repo_truth_docs(tmp_path) == ["AGENTS.md", "TESTING.md"]


def test_doc_within_cap_reports_no_violations(tmp_path):
    (tmp_path / "AGENTS.md").write_text("short doctrine\n", encoding="utf-8")
    assert repo_truth_doc_violations(tmp_path) == []


def test_doc_over_cap_is_reported_as_a_violation(tmp_path):
    (tmp_path / "AGENTS.md").write_text(
        "x" * (REPO_TRUTH_DOC_LIMIT * 2), encoding="utf-8"
    )
    violations = repo_truth_doc_violations(tmp_path)
    assert len(violations) == 1
    assert "AGENTS.md" in violations[0]
    assert f"cap {REPO_TRUTH_DOC_LIMIT}" in violations[0]


def test_doc_cap_reads_configured_limit_when_markdown_default_is_replaced(tmp_path):
    (tmp_path / "spice.toml").write_text(
        "[policy.limits]\n"
        "repo_truth_doc_chars = 12\n"
        "\n"
        "[policy.markdown_depth_budget]\n"
        "extensions = []\n",
        encoding="utf-8",
    )
    (tmp_path / "AGENTS.md").write_text("x" * 19, encoding="utf-8")

    violations = repo_truth_doc_violations(tmp_path)

    assert len(violations) == 1
    assert "AGENTS.md" in violations[0]
    assert "cap 12" in violations[0]


def test_repo_doc_guard_scans_tracked_markdown_with_depth_budget_and_sticky(
    tmp_path,
):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(repo, "README.md", "x" * 16_000)
    _write_repo_file(repo, "docs/guide.md", "x" * 19_000)
    _git(repo, "add", ".")

    with pytest.raises(SpiceError) as first_exc:
        precommit._run_repo_truth_doc_guard(repo)

    first_message = str(first_exc.value)
    assert "README.md" in first_message
    assert "cap 10000" in first_message
    state_path = git_state_path(REPO_DOC_CHAR_STICKY_STATE_GIT_PATH, root=repo)
    assert state_path.exists()

    _write_repo_file(repo, "README.md", "x" * 11_000)
    _git(repo, "add", "README.md")

    with pytest.raises(SpiceError) as sticky_exc:
        precommit._run_repo_truth_doc_guard(repo)

    sticky_message = str(sticky_exc.value)
    assert "README.md" in sticky_message
    assert "cap 10000" in sticky_message


def test_repo_doc_guard_unbounds_markdown_past_depth_threshold(tmp_path):
    repo = _git_init(tmp_path / "repo")
    deep_path = Path("docs/reference/generated/guide.md")
    _write_repo_file(repo, deep_path.as_posix(), "x" * 30000)
    _git(repo, "add", ".")

    bound = resolve_policy(repo).bound_for_path(
        "repo_truth_doc_chars",
        REPO_TRUTH_DOC_LIMIT,
        deep_path,
    )
    assert bound.unlimited
    precommit._run_repo_truth_doc_guard(repo)


def test_repo_doc_guard_ignores_assets_and_binary_markdown_candidates(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(repo, "README.md", "x" * 16_000)
    _write_repo_file(repo, "docs/image.png", "x" * 30000)
    binary_doc = repo / "docs" / "blob.md"
    binary_doc.parent.mkdir(parents=True, exist_ok=True)
    binary_doc.write_bytes(b"\0" + (b"x" * 30000))
    _git(repo, "add", ".")

    violations = repo_truth_doc_violations(repo)
    assert len(violations) == 1
    assert "README.md" in violations[0]


def _loc_finding(path: str, claim: FlexSliceClaim | None) -> LocFinding:
    return LocFinding(
        path=path,
        line_count=8,
        byte_count=80,
        over_line_limit=True,
        over_byte_limit=False,
        line_limit=5,
        byte_limit=1000,
        line_flex_breach=True,
        byte_flex_breach=False,
        flex_slice_claim=claim,
    )


def test_file_loc_guard_informs_peer_held_and_blocks_only_owned(capsys):
    peer = _loc_finding(
        "peer.py",
        FlexSliceClaim(
            path=Path("peer.py"),
            actor="actor-a",
            created_at=100.0,
            expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
        ),
    )
    owned = _loc_finding("mine.py", None)

    # Peer-held only: informational redirect, commit proceeds (no raise).
    precommit._raise_or_inform_flex_findings(
        [peer], render=lambda subset: f"board:{[finding.path for finding in subset]}"
    )
    assert "board:['peer.py']" in capsys.readouterr().out

    # A blocking (locally owned) finding fails the gate; the peer redirect still
    # informs, but only the blocking finding raises.
    with pytest.raises(SpiceError) as excinfo:
        precommit._raise_or_inform_flex_findings(
            [peer, owned],
            render=lambda subset: f"board:{[finding.path for finding in subset]}",
        )
    assert "board:['mine.py']" in str(excinfo.value)
    assert "board:['peer.py']" in capsys.readouterr().out


def test_file_shape_guard_leaves_tracked_markdown_to_repo_doc_budget(tmp_path):
    repo = _git_init(tmp_path / "repo")
    doc_path = Path("docs") / "guide.md"
    _write_repo_file(repo, doc_path.as_posix(), "x" * 130_000)
    _git(repo, "add", ".")

    precommit._run_file_loc_guard(repo, [doc_path])
    violations = repo_truth_doc_violations(repo)

    assert len(violations) == 1
    assert doc_path.as_posix() in violations[0]


def test_doc_cap_reads_scoped_limit_and_unlimited_exemption(tmp_path):
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        'repo_truth_docs = ["AGENTS.md", "docs/STRICT.md", "wide/WIDE.md", "skip/SKIP.md"]\n'
        "\n"
        "[policy.limits]\n"
        "repo_truth_doc_chars = 20\n"
        "\n"
        "[[policy.rules]]\n"
        'scopes = { paths = ["docs/**"] }\n'
        "[policy.rules.repo_truth_doc_chars]\n"
        "max = 5\n"
        "\n"
        "[[policy.rules]]\n"
        'scopes = { paths = ["wide/**"] }\n'
        "[policy.rules.repo_truth_doc_chars]\n"
        "multiplier = 2.0\n"
        "\n"
        "[[policy.rules]]\n"
        'scopes = { paths = ["skip/**"] }\n'
        "unlimited = true\n",
        encoding="utf-8",
    )
    (tmp_path / "AGENTS.md").write_text("x" * 15, encoding="utf-8")
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "STRICT.md").write_text("x" * 8, encoding="utf-8")
    (tmp_path / "wide").mkdir()
    (tmp_path / "wide" / "WIDE.md").write_text("x" * 30, encoding="utf-8")
    (tmp_path / "skip").mkdir()
    (tmp_path / "skip" / "SKIP.md").write_text("x" * 100, encoding="utf-8")

    violations = repo_truth_doc_violations(tmp_path)

    assert violations == ["  docs/STRICT.md: 8 characters (cap 5)"]


def test_policy_pre_commit_extensions_run_after_builtin_steps(tmp_path, monkeypatch):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "spice.toml").write_text(
        "[commands]\n"
        f"fmt-cs = {_argv_toml(sys.executable, str(recorder), 'fmt-cs')}\n"
        "\n"
        "[policy]\n"
        "pre_commit = [\n"
        '  "fmt-cs",\n'
        '  { label = "assets", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'assets')} }},\n"
        "]\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    assert events.read_text(encoding="utf-8").splitlines() == [
        *BUILTIN_PRE_COMMIT_LABELS,
        "fmt-cs",
        "assets",
    ]


def test_policy_pre_commit_builtin_steps_can_be_disabled_and_replaced(
    tmp_path, monkeypatch
):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "spice.toml").write_text(
        "[policy.pre_commit_builtins]\n"
        "formatters = false\n"
        '"magic-numbers" = { label = "custom magic", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'custom magic')} }}\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    assert events.read_text(encoding="utf-8").splitlines() == [
        "merge integrity",
        "plan phase",
        "repo shape",
        "staging",
        "repo docs",
        "local paths",
        "serve web typecheck",
        "javascript unused",
        "env policy",
        "env name ledger",
        "file shape",
        "complexity",
        "custom magic",
        "markdown links",
        "reachability",
        "symbol reachability",
        "python unused",
        "assertion-free tests",
        "private internals",
    ]


def test_pre_commit_prints_every_disabled_builtin_on_every_run(
    tmp_path, monkeypatch, capsys
):
    (tmp_path / "spice.toml").write_text(
        "[policy.pre_commit_builtins]\n"
        "formatters = false\n"
        '"magic-numbers" = { enabled = false }\n',
        encoding="utf-8",
    )
    _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)
    expected = [
        "pre-commit: disabled builtin formatters",
        "pre-commit: disabled builtin magic-numbers",
    ]

    for _invocation in range(2):
        assert precommit.handle_pre_commit(tmp_path) == 0
        assert capsys.readouterr().out.splitlines() == expected


def test_markdown_links_pre_commit_guard_reports_shared_board(tmp_path, monkeypatch):
    finding = precommit.links.MarkdownLinkCaseFinding(
        source_path=Path("docs/index.md"),
        line=4,
        raw_target="GUIDE.md",
        resolved_path=Path("docs/GUIDE.md"),
        expected_path=Path("docs/guide.md"),
    )
    monkeypatch.setattr(
        precommit.links,
        "markdown_link_case_findings",
        lambda repo_root: [finding],
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_markdown_links_guard(tmp_path)

    assert str(exc_info.value) == "\n".join(
        [
            "markdown-links: 1 case-mismatched tracked markdown link target(s)",
            "  FAIL  docs/index.md:4 GUIDE.md -> docs/guide.md",
        ]
    )


def test_policy_pre_commit_failure_reports_the_step_label(tmp_path, monkeypatch):
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        'pre_commit = [{ label = "assets", '
        f"run = {_argv_toml(sys.executable, '-c', _failure_program())} }}]\n",
        encoding="utf-8",
    )
    _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    with pytest.raises(SpiceError) as exc_info:
        precommit.handle_pre_commit(tmp_path)

    message = str(exc_info.value)
    assert "[assets]" in message
    assert "exited 7" in message
    assert "asset failed" in message


def test_policy_pre_commit_success_extensions_run_after_gate_passes(
    tmp_path, monkeypatch
):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        "pre_commit = [\n"
        '  { label = "assets", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'assets')}, "
        'scopes = { phases = ["pre-commit"] } },\n'
        "]\n"
        "pre_commit_success = [\n"
        '  { label = "success", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'success')}, "
        'scopes = { phases = ["pre-commit-success"] } },\n'
        "]\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    assert precommit.handle_pre_commit(tmp_path) == 0
    assert events.read_text(encoding="utf-8").splitlines() == [
        *BUILTIN_PRE_COMMIT_LABELS,
        "assets",
        "success",
    ]


def test_assertion_free_test_guard_fails_above_default_zero_debt(tmp_path):
    test_dir = tmp_path / "tests"
    test_dir.mkdir()
    (test_dir / "test_empty.py").write_text(
        "def test_empty():\n    value = 1\n", encoding="utf-8"
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_assertion_free_test_guard(tmp_path)

    message = str(exc_info.value)
    assert "assertion-free-tests: 1 test(s)" in message
    assert "test_empty.py:1 test_empty" in message
    assert "[policy.debt] assertion_free_tests=0" in message
    assert "0 means clean" in message


def test_assertion_free_test_guard_allows_configured_debt_baseline(tmp_path):
    test_dir = tmp_path / "tests"
    test_dir.mkdir()
    (test_dir / "test_empty.py").write_text(
        "def test_empty():\n    value = 1\n", encoding="utf-8"
    )
    (tmp_path / "spice.toml").write_text(
        "[policy.debt]\nassertion_free_tests = 1\n",
        encoding="utf-8",
    )

    precommit._run_assertion_free_test_guard(tmp_path)
    findings = precommit.testquality.scan_assertion_free_tests(
        precommit.testquality.test_paths(tmp_path), root=tmp_path
    )
    assert len(findings) == 1


def test_symbol_reachability_guard_fails_on_any_finding(tmp_path):
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "import spice.live\n", encoding="utf-8"
    )
    (tmp_path / "spice" / "live.py").write_text(
        "def planted_dead_function_abc():\n    return 1\n", encoding="utf-8"
    )
    (tmp_path / "tests" / "test_symbol.py").write_text(
        "from spice.live import planted_dead_function_abc\n", encoding="utf-8"
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_symbol_reachability_guard(tmp_path)

    message = str(exc_info.value)
    assert "symbol-reachability: 1 test-only symbol(s)" in message
    assert "spice/live.py:planted_dead_function_abc" in message
    assert "zero test-only symbols are allowed" in message


def test_reachability_guard_fails_on_configured_module_provider_finding(tmp_path):
    provider = tmp_path / "js_provider.py"
    payload = json.dumps(
        [
            {
                "kind": "module",
                "subject": "web.dead_widget",
                "path": "web/src/dead_widget.js",
                "imported_by": ["web/test/dead_widget.test.js"],
            }
        ]
    )
    provider.write_text(f"print({payload!r})\n", encoding="utf-8")
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        "reachability_providers = [\n"
        '  { name = "javascript", '
        f"run = {json.dumps([sys.executable, str(provider)])}, "
        'scopes = { paths = ["web/**/*.js"] } },\n'
        "]\n",
        encoding="utf-8",
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_reachability_guard(tmp_path, [Path("web/src/dead_widget.js")])

    message = str(exc_info.value)
    assert "reachability: 1 test-only finding(s)" in message
    assert "provider: javascript" in message
    assert "subject: web.dead_widget" in message
    assert "[policy.debt] reachability_test_only=0" in message
    assert "0 means clean" in message


def test_reachability_guard_reports_configured_debt_when_exceeded(tmp_path):
    provider = tmp_path / "js_provider.py"
    payload = json.dumps(
        [
            {
                "kind": "module",
                "subject": "web.dead_widget",
                "path": "web/src/dead_widget.js",
                "imported_by": ["web/test/dead_widget.test.js"],
            },
            {
                "kind": "module",
                "subject": "web.dead_panel",
                "path": "web/src/dead_panel.js",
                "imported_by": ["web/test/dead_panel.test.js"],
            },
        ]
    )
    provider.write_text(f"print({payload!r})\n", encoding="utf-8")
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        "reachability_providers = [\n"
        '  { name = "javascript", '
        f"run = {json.dumps([sys.executable, str(provider)])}, "
        'scopes = { paths = ["web/**/*.js"] } },\n'
        "]\n"
        "\n"
        "[policy.debt]\n"
        "reachability_test_only = 1\n",
        encoding="utf-8",
    )

    with pytest.raises(SpiceError) as exc_info:
        precommit._run_reachability_guard(tmp_path, [Path("web/src/dead_widget.js")])

    message = str(exc_info.value)
    assert "reachability: 2 test-only finding(s)" in message
    assert "[policy.debt] reachability_test_only=1" in message
    assert "explicit drainable cleanup debt" in message


def test_symbol_reachability_guard_fails_on_configured_symbol_provider_finding(
    tmp_path,
):
    provider = tmp_path / "js_provider.py"
    payload = json.dumps(
        [
            {
                "kind": "function",
                "subject": "render.unusedRender",
                "path": "web/src/render.js",
                "imported_by": ["web/test/render.test.js"],
            }
        ]
    )
    provider.write_text(f"print({payload!r})\n", encoding="utf-8")
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        "reachability_providers = [\n"
        '  { name = "javascript", '
        f"run = {json.dumps([sys.executable, str(provider)])}, "
        'scopes = { paths = ["web/**/*.js"] } },\n'
        "]\n",
        encoding="utf-8",
    )

    # A symbol-kind provider finding routes to the finer symbol-reachability gate.
    with pytest.raises(SpiceError) as exc_info:
        precommit._run_symbol_reachability_guard(tmp_path, [Path("web/src/render.js")])

    message = str(exc_info.value)
    assert "symbol-reachability: 1 test-only symbol(s)" in message
    assert "provider: javascript" in message
    assert "web/src/render.js:unusedRender" in message
    assert "zero test-only symbols are allowed" in message


def test_symbol_reachability_guard_allows_clean_repo(tmp_path):
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "from spice.live import production_function\nproduction_function()\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "live.py").write_text(
        "def production_function():\n    return 1\n", encoding="utf-8"
    )

    precommit._run_symbol_reachability_guard(tmp_path)
    assert precommit.reachability.scan_symbol_reachability(tmp_path) == []


def test_policy_pre_commit_success_extensions_wait_for_clean_gate(
    tmp_path, monkeypatch
):
    recorder = _write_recorder(tmp_path)
    (tmp_path / "spice.toml").write_text(
        "[policy]\n"
        "pre_commit = [\n"
        '  { label = "assets", '
        f"run = {_argv_toml(sys.executable, '-c', _failure_program())} }},\n"
        "]\n"
        "pre_commit_success = [\n"
        '  { label = "success", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'success')} }},\n"
        "]\n",
        encoding="utf-8",
    )
    events = _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch)

    with pytest.raises(SpiceError) as exc_info:
        precommit.handle_pre_commit(tmp_path)

    assert "asset failed" in str(exc_info.value)
    assert events.read_text(encoding="utf-8").splitlines() == BUILTIN_PRE_COMMIT_LABELS


def test_policy_pre_commit_extensions_receive_filtered_staged_paths(
    tmp_path, monkeypatch
):
    repo = _git_init(tmp_path / "repo")
    recorder = _write_staged_paths_recorder(tmp_path)
    _write_repo_file(
        repo,
        "spice.toml",
        "[policy]\n"
        "pre_commit = [\n"
        '  { label = "cs", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'cs')}, "
        'scopes = { paths = ["*.cs"], phases = ["pre-commit"] } },\n'
        '  { label = "lua", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'lua')}, "
        'scopes = { paths = ["*.lua"], phases = ["pre-commit"] } },\n'
        '  { label = "always", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'always')} }},\n"
        "]\n",
    )
    _write_repo_file(repo, "docs/readme.md", "docs\n")
    _write_repo_file(repo, "src/main.cs", "class Program {}\n")
    _git(repo, "add", ".")
    _patch_pre_commit_builtin_noops(monkeypatch)

    assert precommit.handle_pre_commit(repo) == 0

    rows = (tmp_path / "staged-paths.txt").read_text(encoding="utf-8").splitlines()
    assert rows == [
        "cs:src/main.cs",
        "always:docs/readme.md|spice.toml|src/main.cs",
    ]


def test_policy_pre_commit_combined_scopes_select_layered_agent_context(
    tmp_path, monkeypatch
):
    repo = _git_init(tmp_path / "repo")
    recorder = _write_staged_paths_recorder(tmp_path)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    _write_repo_file(
        repo,
        "spice.toml",
        "[agent]\n"
        'model = "GPT-COMBINED"\n'
        'driver = "codex"\n'
        "\n"
        "[policy]\n"
        "pre_commit = [\n"
        '  { label = "combined", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'combined')}, "
        'scopes = { paths = ["docs/**", "src/**"], '
        'drivers = ["claude", "CODEX"], '
        'models = ["other", "gpt-combined"] } },\n'
        '  { label = "all-paths", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'all-paths')}, "
        'scopes = { drivers = ["codex"], models = ["gpt-combined"] } },\n'
        '  { label = "all-drivers", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'all-drivers')}, "
        'scopes = { paths = ["src/**"], models = ["gpt-combined"] } },\n'
        '  { label = "all-models", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'all-models')}, "
        'scopes = { paths = ["src/**"], drivers = ["codex"] } },\n'
        '  { label = "all-contexts", '
        f"run = {_argv_toml(sys.executable, str(recorder), 'all-contexts')} }},\n"
        "]\n",
    )
    _write_repo_file(repo, "src/main.py", "answer = 42\n")
    _git(repo, "add", ".")
    _patch_pre_commit_builtin_noops(monkeypatch)

    assert precommit.handle_pre_commit(repo) == 0

    rows = (tmp_path / "staged-paths.txt").read_text(encoding="utf-8").splitlines()
    assert rows == [
        "combined:src/main.py",
        "all-paths:spice.toml|src/main.py",
        "all-drivers:src/main.py",
        "all-models:src/main.py",
        "all-contexts:spice.toml|src/main.py",
    ]


def test_rejected_pre_commit_run_leaves_every_sticky_ledger_as_it_found_it(
    tmp_path, monkeypatch
):
    repo = _latch_probe_repo(tmp_path)
    _seed_line_sticky_ledger(repo, {Path("band.py")})
    _patch_builtins_except_file_shape(monkeypatch)
    before = _sticky_ledger_text(repo)

    with pytest.raises(SpiceError, match="big.py"):
        precommit.handle_pre_commit(repo)

    # The commit was refused, so the author's next attempt must meet the same
    # ceiling this one did. A latch recorded here would hold the rejected work
    # to base instead, making the remedy this very failure printed unreachable.
    assert _sticky_ledger_text(repo) == before


def test_accepted_pre_commit_run_latches_its_breach_and_prunes_stale_paths(
    tmp_path, monkeypatch
):
    repo = _latch_probe_repo(tmp_path)
    _seed_line_sticky_ledger(repo, {Path("gone.py")})
    _hold_peer_flex_slice(repo, Path("big.py"))
    _patch_builtins_except_file_shape(monkeypatch)

    assert precommit.handle_pre_commit(repo) == 0

    # A peer holds big.py's flex slice, so its breach reports as a redirect and
    # the commit lands. This run was accepted, so its latch is exactly what the
    # next commit must live with: the landed breach held, the vanished path gone.
    assert _line_sticky_paths(repo) == ["big.py"]


def test_file_shrunk_after_a_rejected_attempt_passes_the_next_run(
    tmp_path, monkeypatch
):
    repo = _latch_probe_repo(tmp_path)
    _patch_builtins_except_file_shape(monkeypatch)
    with pytest.raises(SpiceError, match="big.py"):
        precommit.handle_pre_commit(repo)

    # The author does exactly what the refusal asked and lands the file inside
    # the flex band it was measured against.
    _write_repo_file(repo, "big.py", "line\n" * LATCH_PROBE_BAND_LINES)
    _git(repo, "add", ".")

    # Nothing was committed, so nothing may hold this file to base: a size the
    # gate would have accepted from any other author has to be accepted here.
    assert precommit.handle_pre_commit(repo) == 0


def _patch_pre_commit_builtin_recorders(tmp_path, monkeypatch):
    events = tmp_path / "events.txt"

    def record(label: str) -> None:
        with events.open("a", encoding="utf-8") as handle:
            handle.write(label + "\n")

    monkeypatch.setattr(precommit, "staged_paths", lambda repo_root: [])
    monkeypatch.setattr(
        precommit,
        "_run_merge_integrity_guard",
        lambda repo_root: record("merge integrity"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_plan_phase_mutation_guard",
        lambda repo_root: record("plan phase"),
    )
    monkeypatch.setattr(
        precommit, "_run_shape_guards", lambda repo_root: record("repo shape")
    )
    monkeypatch.setattr(
        precommit, "_run_staging_guard", lambda repo_root: record("staging")
    )
    monkeypatch.setattr(
        precommit, "_run_repo_truth_doc_guard", lambda repo_root: record("repo docs")
    )
    monkeypatch.setattr(
        precommit,
        "_run_python_format_guard",
        lambda repo_root, paths: record("formatters"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_serve_web_typecheck_guard",
        lambda repo_root: record("serve web typecheck"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_javascript_unused_guard",
        lambda repo_root: record("javascript unused"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_local_path_guard",
        lambda repo_root, paths: record("local paths"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_env_policy_guard",
        lambda repo_root, paths: record("env policy"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_env_name_ledger_guard",
        lambda repo_root: record("env name ledger"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_file_loc_guard",
        lambda repo_root, paths: record("file shape"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_complexity_guard",
        lambda repo_root, paths: record("complexity"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_magic_numbers_guard",
        lambda repo_root, paths: record("magic numbers"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_markdown_links_guard",
        lambda repo_root: record("markdown links"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_reachability_guard",
        lambda repo_root, paths=None: record("reachability"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_symbol_reachability_guard",
        lambda repo_root, paths=None: record("symbol reachability"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_python_unused_guard",
        lambda repo_root: record("python unused"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_assertion_free_test_guard",
        lambda repo_root: record("assertion-free tests"),
    )
    monkeypatch.setattr(
        precommit,
        "_run_private_internal_coupling_guard",
        lambda repo_root: record("private internals"),
    )
    return events


def _patch_pre_commit_builtin_noops_except_staging(monkeypatch) -> None:
    monkeypatch.setattr(precommit, "_run_merge_integrity_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_plan_phase_mutation_guard", lambda repo_root: None
    )
    monkeypatch.setattr(precommit, "_run_shape_guards", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_repo_truth_doc_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_python_format_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_serve_web_typecheck_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_javascript_unused_guard", lambda repo_root: None
    )
    monkeypatch.setattr(
        precommit, "_run_local_path_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_env_policy_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_env_name_ledger_guard", lambda repo_root: None)
    monkeypatch.setattr(precommit, "_run_file_loc_guard", lambda repo_root, paths: None)
    monkeypatch.setattr(
        precommit, "_run_complexity_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(
        precommit, "_run_magic_numbers_guard", lambda repo_root, paths: None
    )
    monkeypatch.setattr(precommit, "_run_markdown_links_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(
        precommit, "_run_symbol_reachability_guard", lambda repo_root, paths=None: None
    )
    monkeypatch.setattr(precommit, "_run_python_unused_guard", lambda repo_root: None)
    monkeypatch.setattr(
        precommit, "_run_assertion_free_test_guard", lambda repo_root: None
    )


def _patch_pre_commit_builtin_noops(monkeypatch) -> None:
    _patch_pre_commit_builtin_noops_except_staging(monkeypatch)
    monkeypatch.setattr(precommit, "_run_staging_guard", lambda repo_root: None)


def _patch_builtins_except_file_shape(monkeypatch) -> None:
    """No-op every builtin gate except the one whose latch is under test."""
    file_loc_guard = precommit._run_file_loc_guard
    _patch_pre_commit_builtin_noops(monkeypatch)
    monkeypatch.setattr(precommit, "_run_file_loc_guard", file_loc_guard)


def _latch_probe_repo(tmp_path: Path) -> Path:
    """A staged repo with one path over the flex ceiling and one inside it."""
    repo = _git_init(tmp_path / "repo")
    _write_repo_file(
        repo,
        "spice.toml",
        "[policy.limits]\n"
        f"file_loc = {LATCH_PROBE_BASE_LOC}\n"
        f"file_bytes = {LATCH_PROBE_BYTE_LOC}\n"
        "\n"
        "[policy.flex]\n"
        f"ratio = {LATCH_PROBE_FLEX_RATIO}\n"
        "\n"
        "[[policy.rules]]\n"
        'scopes = { paths = ["spice.toml"] }\n'
        "unlimited = true\n",
    )
    _write_repo_file(repo, "big.py", "line\n" * LATCH_PROBE_BREACH_LINES)
    _write_repo_file(repo, "band.py", "line\n" * LATCH_PROBE_BAND_LINES)
    _git(repo, "add", ".")
    write_agent_thread_pointer(repo, LATCH_PROBE_THREAD_ID)
    return repo


def _line_sticky_ledger() -> gates.StickyLedger[Path]:
    return gates.path_sticky_ledger(FILE_LOC_STICKY_STATE_GIT_PATH)


def _seed_line_sticky_ledger(repo: Path, paths: set[Path]) -> None:
    gates.persist_sticky_ledger(_line_sticky_ledger(), paths, root=repo)


def _line_sticky_paths(repo: Path) -> list[str]:
    latched = gates.load_sticky_ledger(_line_sticky_ledger(), root=repo, renames={})
    return sorted(path.as_posix() for path in latched)


def _sticky_ledger_text(repo: Path) -> dict[str, str]:
    """Every sticky ledger's on-disk text, keyed by its git-state path."""
    snapshot: dict[str, str] = {}
    for git_path in STICKY_LEDGER_GIT_PATHS:
        state_path = git_state_path(git_path, root=repo)
        if state_path.exists():
            snapshot[git_path] = state_path.read_text(encoding="utf-8")
    return snapshot


def _hold_peer_flex_slice(repo: Path, path: Path) -> None:
    """A live claim by another actor, which reports a breach as a redirect."""
    now = time.time()
    save_flex_slice_claims(
        (
            FlexSliceClaim(
                path=path,
                actor="peer-worktree",
                created_at=now,
                expires_at=now + FLEX_SLICE_CLAIM_TTL_SECONDS,
            ),
        ),
        root=repo,
    )


def _write_recorder(tmp_path):
    recorder = tmp_path / "record_step.py"
    recorder.write_text(
        "from pathlib import Path\n"
        "import sys\n"
        "with Path('events.txt').open('a', encoding='utf-8') as handle:\n"
        "    handle.write(sys.argv[1] + '\\n')\n",
        encoding="utf-8",
    )
    return recorder


def _argv_toml(*argv: str) -> str:
    return "[" + ", ".join(_toml_string(item) for item in argv) + "]"


def _toml_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _failure_program() -> str:
    return "import sys; print('asset failed'); sys.exit(7)"


def _write_staged_paths_recorder(tmp_path):
    recorder = tmp_path / "record_staged_paths.py"
    staged_paths_env = "SPICE_" + "STAGED_PATHS"
    recorder.write_text(
        "import os\n"
        "from pathlib import Path\n"
        "import sys\n"
        f"paths = os.environ[{staged_paths_env!r}].splitlines()\n"  # env-policy: allow
        "with Path(sys.argv[0]).with_name('staged-paths.txt').open("
        "'a', encoding='utf-8') as handle:\n"
        "    handle.write(sys.argv[1] + ':' + '|'.join(paths) + '\\n')\n",
        encoding="utf-8",
    )
    return recorder


def _git_init(repo: Path) -> Path:
    _run(["git", "init", "-b", "main", str(repo)])
    _git(repo, "config", "user.email", "spice@example.test")
    _git(repo, "config", "user.name", "Spice Tests")
    return repo


def _write_repo_file(repo: Path, name: str, text: str) -> None:
    path = repo / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    if name == "spice.toml":
        approve_repository_config(repo)


def _git(
    repo: Path, *args: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    return _run(["git", "-C", str(repo), *args], check=check)


def _run(
    args: list[str], *, check: bool = True, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()  # env-policy: allow
    env["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(PROJECT_ROOT), env.get("PYTHONPATH", "")) if entry
    )
    result = subprocess.run(
        args,
        capture_output=True,
        check=False,
        cwd=cwd,
        env=env,
        text=True,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            f"command failed: {' '.join(args)}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result
