"""The flex+sticky study scans must be pure unless persistence is opt-in.

A reporting/study caller (the default) must not advance the on-disk sticky
floor; only a committing gate passes ``persist=True``. These tests assert the
sticky JSON in the git dir is untouched by a default scan and written by a
``persist=True`` scan, for both fileloc and complexity.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from spice.flexstate import (
    FLEX_SLICE_CLAIM_TTL_SECONDS,
    FLEX_SLICE_CLAIMS_VERSION,
    FlexSliceClaim,
    flex_slice_claims_state_path,
    git_state_path,
    load_flex_slice_claims,
    save_flex_slice_claims,
)
from spice.studies import complexity, fileloc, repodocs


def test_fileloc_reporting_scan_does_not_persist_sticky(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")

    fileloc.scan_staged_loc_violations(
        [Path("big.py")], root=repo, limit=5, flex_limit_value=7
    )

    assert not git_state_path(
        fileloc.FILE_LOC_STICKY_STATE_GIT_PATH, root=repo
    ).exists()


def test_fileloc_gate_scan_persists_sticky(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")

    fileloc.scan_staged_loc_violations(
        [Path("big.py")], root=repo, limit=5, flex_limit_value=7, persist=True
    )

    assert git_state_path(fileloc.FILE_LOC_STICKY_STATE_GIT_PATH, root=repo).exists()


def test_fileloc_sticky_latch_self_heals_when_file_drops_under_base(tmp_path):
    repo = _init_repo(tmp_path)
    big = repo / "big.py"
    big.write_text("x = 1\n" * 8, encoding="utf-8")
    state = git_state_path(fileloc.FILE_LOC_STICKY_STATE_GIT_PATH, root=repo)

    fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
    )
    assert state.exists()  # 8 lines > flex 7 -> latched

    big.write_text("x = 1\n" * 4, encoding="utf-8")  # 4 <= base 5
    healed = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
    )

    # The latch retires the moment any scan sees the file back under base, even
    # though no fully clean commit ran between the two scans.
    assert healed == []
    assert not state.exists()


def test_fileloc_sticky_latch_holds_in_flex_band_until_under_base(tmp_path):
    repo = _init_repo(tmp_path)
    big = repo / "big.py"
    big.write_text("x = 1\n" * 8, encoding="utf-8")

    fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
    )

    big.write_text("x = 1\n" * 6, encoding="utf-8")  # base 5 < 6 <= flex 7
    findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
    )

    # A fresh 6-line file passes in the flex band, but a latched one is held to
    # base until it drops under base -- self-heal must not clear early.
    assert [finding.path for finding in findings] == ["big.py"]
    assert findings[0].line_limit == 5
    assert git_state_path(fileloc.FILE_LOC_STICKY_STATE_GIT_PATH, root=repo).exists()


def test_fileloc_board_distinguishes_current_breach_from_latch_held(tmp_path):
    repo = _init_repo(tmp_path)
    big = repo / "big.py"
    big.write_text("x = 1\n" * 8, encoding="utf-8")

    breach_findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
    )

    # A file over the flex limit on its current measured content: the remedy is
    # to split, so the board renders the split guidance and no ledger tag.
    assert fileloc.render_loc_board(
        breach_findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5",
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam",
        ]
    )

    big.write_text("x = 1\n" * 6, encoding="utf-8")  # base 5 < 6 <= flex 7
    latched_findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
    )

    # The same file is within flex now and fails only because the sticky latch
    # holds it to base. The reason names the ledger file so the untracked git-dir
    # state is reachable, and the guidance points at drop-under-base / peer heal
    # rather than a split it does not need.
    assert fileloc.render_loc_board(
        latched_findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 6 lines > 5 (held at base by .spice/file-loc-sticky.json)",
            "  a held-at-base path is within flex now but latched by an earlier "
            "breach recorded in the named ledger; it clears when any scan sees it "
            "back under its base limit, so a latch left by a peer worktree heals "
            "once the fix lands on the shared baseline",
        ]
    )


def test_complexity_reporting_scan_does_not_persist_sticky(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path)
    record = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=20, nloc=20
    )
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    complexity.scan_staged_complexity_violations(
        [Path("big.py")], root=repo, max_length=3, length_flex_limit_value=4
    )

    assert not git_state_path(
        complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH, root=repo
    ).exists()


def test_complexity_gate_scan_persists_sticky(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path)
    record = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=20, nloc=20
    )
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    assert git_state_path(
        complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH, root=repo
    ).exists()


def test_complexity_sticky_latch_self_heals_when_routine_drops_under_base(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path)
    current = {
        "record": complexity.ComplexityRecord(
            path="big.py", function_name="f", ccn=1, length=20, nloc=20
        )
    }
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [current["record"]],
    )
    state = git_state_path(complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH, root=repo)

    complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )
    assert state.exists()  # length 20 > flex 4 -> latched

    current["record"] = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=2, nloc=2
    )  # 2 <= base 3
    healed = complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    # The latch retires the moment any scan re-measures the routine back under
    # base, even without a fully clean commit between the two scans.
    assert healed == []
    assert not state.exists()


def test_complexity_sticky_latch_holds_in_flex_band_until_under_base(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path)
    current = {
        "record": complexity.ComplexityRecord(
            path="big.py", function_name="f", ccn=1, length=20, nloc=20
        )
    }
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [current["record"]],
    )

    complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    current["record"] = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=4, nloc=4
    )  # base 3 < 4 <= flex 4
    findings = complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    # A fresh 4-length routine passes in the flex band, but a latched one is held
    # to base until it drops under base -- self-heal must not clear early.
    assert [finding.record.function_name for finding in findings] == ["f"]
    assert findings[0].length_limit == 3
    assert git_state_path(
        complexity.COMPLEXITY_LENGTH_STICKY_GIT_PATH, root=repo
    ).exists()


def test_complexity_board_distinguishes_current_breach_from_latch_held(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("def f():\n    return 1\n", encoding="utf-8")
    current = {
        "record": complexity.ComplexityRecord(
            path="big.py", function_name="f", ccn=8, length=1, nloc=1
        )
    }
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [current["record"]],
    )

    breach_findings = complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_ccn=5,
        ccn_flex_limit_value=7,
        max_length=100,
        length_flex_limit_value=100,
        persist=True,
    )

    # ccn 8 is over the flex limit on its current measured content: a real
    # breach, rendered without a ledger tag.
    assert complexity.render_complexity_board(
        breach_findings, max_ccn=5, max_length=100
    ) == "\n".join(
        [
            "complexity: 1 violation(s)",
            "  FAIL  big.py:f: ccn 8 > 5",
        ]
    )

    current["record"] = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=6, length=1, nloc=1
    )  # base 5 < 6 <= flex 7
    latched_findings = complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_ccn=5,
        ccn_flex_limit_value=7,
        max_length=100,
        length_flex_limit_value=100,
        persist=True,
    )

    # The same routine is within flex now and fails only because the sticky latch
    # holds it to base. The reason names the ccn ledger and the guidance points at
    # drop-under-base / peer heal -- the identical distinction the file-loc board
    # draws, because both boards render through the shared reconcile_sticky_latch
    # seam.
    assert complexity.render_complexity_board(
        latched_findings, max_ccn=5, max_length=100
    ) == "\n".join(
        [
            "complexity: 1 violation(s)",
            "  FAIL  big.py:f: ccn 6 > 5 (held at base by "
            ".spice/complexity-ccn-sticky.json)",
            "  a held-at-base routine is within flex now but latched by an earlier "
            "breach recorded in the named ledger; it clears when any scan sees it "
            "back under its base limit, so a latch left by a peer worktree heals "
            "once the fix lands on the shared baseline",
        ]
    )


def test_complexity_length_board_names_its_own_latch_ledger(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("def f():\n    return 1\n", encoding="utf-8")
    current = {
        "record": complexity.ComplexityRecord(
            path="big.py", function_name="f", ccn=1, length=5, nloc=5
        )
    }
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [current["record"]],
    )

    complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    current["record"] = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=1, length=4, nloc=4
    )  # base 3 < 4 <= flex 4
    latched_findings = complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_length=3,
        length_flex_limit_value=4,
        persist=True,
    )

    # The length board draws the same latch-held distinction as ccn, and names
    # its own ledger file -- the two dimensions do not share one ledger.
    assert complexity.render_complexity_board(
        latched_findings, max_length=3
    ) == "\n".join(
        [
            "complexity: 1 violation(s)",
            "  FAIL  big.py:f: length 4 > 3 (held at base by "
            ".spice/complexity-length-sticky.json)",
            "  a held-at-base routine is within flex now but latched by an earlier "
            "breach recorded in the named ledger; it clears when any scan sees it "
            "back under its base limit, so a latch left by a peer worktree heals "
            "once the fix lands on the shared baseline",
        ]
    )


def test_flex_slice_claim_round_trips_live_claim(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "src").mkdir()
    (repo / "src/app.py").write_text("x = 1\n", encoding="utf-8")
    claim = FlexSliceClaim(
        path=Path("./src\\app.py"),
        actor="actor-a",
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )
    loaded = FlexSliceClaim(
        path=Path("src/app.py"),
        actor="actor-a",
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )

    save_flex_slice_claims((claim,), root=repo)

    assert load_flex_slice_claims(root=repo, now=101.0) == (loaded,)
    assert _flex_slice_claims_payload(repo) == {
        "claims": [
            {
                "actor": "actor-a",
                "created_at": 100.0,
                "expires_at": 100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
                "path": "src/app.py",
            }
        ],
        "ttl_seconds": FLEX_SLICE_CLAIM_TTL_SECONDS,
        "version": FLEX_SLICE_CLAIMS_VERSION,
    }


def test_flex_slice_claim_prune_persists_only_active_claims(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "expired.py").write_text("old = 1\n", encoding="utf-8")
    (repo / "live.py").write_text("new = 1\n", encoding="utf-8")
    expired = FlexSliceClaim(
        path=Path("expired.py"),
        actor="actor-expired",
        created_at=10.0,
        expires_at=20.0,
    )
    live = FlexSliceClaim(
        path=Path("live.py"),
        actor="actor-live",
        created_at=30.0,
        expires_at=300.0,
    )
    save_flex_slice_claims((expired, live), root=repo)

    active = load_flex_slice_claims(root=repo, now=100.0)
    save_flex_slice_claims(active, root=repo)

    assert active == (live,)
    assert load_flex_slice_claims(root=repo, now=100.0) == (live,)
    assert _flex_slice_claims_payload(repo)["claims"] == [
        {
            "actor": "actor-live",
            "created_at": 30.0,
            "expires_at": 300.0,
            "path": "live.py",
        }
    ]


def test_flex_slice_claim_prune_follows_renames_and_existing_paths(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "live.py").write_text("keep = 1\n", encoding="utf-8")
    (repo / "new.py").write_text("moved = 1\n", encoding="utf-8")
    live = FlexSliceClaim(
        path=Path("live.py"),
        actor="actor-live",
        created_at=20.0,
        expires_at=300.0,
    )
    renamed = FlexSliceClaim(
        path=Path("old.py"),
        actor="actor-rename",
        created_at=10.0,
        expires_at=300.0,
    )
    missing = FlexSliceClaim(
        path=Path("missing.py"),
        actor="actor-missing",
        created_at=30.0,
        expires_at=300.0,
    )
    expected = (
        live,
        FlexSliceClaim(
            path=Path("new.py"),
            actor="actor-rename",
            created_at=10.0,
            expires_at=300.0,
        ),
    )
    renames = {Path("old.py"): Path("new.py")}
    save_flex_slice_claims((renamed, missing, live), root=repo)

    active = load_flex_slice_claims(root=repo, renames=renames, now=100.0)
    save_flex_slice_claims(active, root=repo)

    assert active == expected
    assert load_flex_slice_claims(root=repo, now=100.0) == expected
    assert _flex_slice_claims_payload(repo)["claims"] == [
        {
            "actor": "actor-live",
            "created_at": 20.0,
            "expires_at": 300.0,
            "path": "live.py",
        },
        {
            "actor": "actor-rename",
            "created_at": 10.0,
            "expires_at": 300.0,
            "path": "new.py",
        },
    ]


def test_fileloc_first_tripper_records_claim_and_gets_normal_guidance(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")

    findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-a",
        flex_claim_now=100.0,
    )

    assert load_flex_slice_claims(root=repo, now=101.0) == (
        FlexSliceClaim(
            path=Path("big.py"),
            actor="actor-a",
            created_at=100.0,
            expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
        ),
    )
    assert fileloc.render_loc_board(
        findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5",
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam",
        ]
    )


def test_fileloc_same_actor_refreshes_claim_and_gets_normal_guidance(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")
    save_flex_slice_claims(
        (
            FlexSliceClaim(
                path=Path("big.py"),
                actor="actor-a",
                created_at=100.0,
                expires_at=200.0,
            ),
        ),
        root=repo,
    )

    findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-a",
        flex_claim_now=150.0,
    )

    assert load_flex_slice_claims(root=repo, now=151.0) == (
        FlexSliceClaim(
            path=Path("big.py"),
            actor="actor-a",
            created_at=100.0,
            expires_at=150.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
        ),
    )
    assert fileloc.render_loc_board(
        findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5",
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam",
        ]
    )


def test_fileloc_peer_actor_redirects_same_path_without_refactor_guidance(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")
    peer_claim = FlexSliceClaim(
        path=Path("big.py"),
        actor="actor-a",
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )
    save_flex_slice_claims((peer_claim,), root=repo)

    findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-b",
        flex_claim_now=200.0,
    )

    assert load_flex_slice_claims(root=repo, now=201.0) == (peer_claim,)
    assert fileloc.render_loc_board(
        findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5; live flex slice held by actor-a "
            "until 1970-01-01T06:01:40Z; keep this change append-only or "
            "move to another seam",
            "  peer-held flex slices redirect duplicate refactors; keep changes "
            "append-only or move to another seam",
        ]
    )


def test_fileloc_unrelated_peer_claim_does_not_block_new_path(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")
    (repo / "other.py").write_text("other = 1\n", encoding="utf-8")
    other_claim = FlexSliceClaim(
        path=Path("other.py"),
        actor="actor-a",
        created_at=100.0,
        expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )
    new_claim = FlexSliceClaim(
        path=Path("big.py"),
        actor="actor-b",
        created_at=200.0,
        expires_at=200.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
    )
    save_flex_slice_claims((other_claim,), root=repo)

    findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-b",
        flex_claim_now=200.0,
    )

    assert load_flex_slice_claims(root=repo, now=201.0) == (
        new_claim,
        other_claim,
    )
    assert fileloc.render_loc_board(
        findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5",
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam",
        ]
    )


def test_fileloc_linked_worktrees_share_slice_claims_and_ttl(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("x = 1\n" * 8, encoding="utf-8")
    _commit_all(repo, "seed hot path")
    peer_repo = tmp_path / "peer"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "-q", str(peer_repo), "HEAD"],
        check=True,
    )

    first_findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-a",
        flex_claim_now=100.0,
    )
    peer_findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=peer_repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-b",
        flex_claim_now=200.0,
    )
    late_findings = fileloc.scan_staged_loc_violations(
        [Path("big.py")],
        root=peer_repo,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
        persist=True,
        flex_actor="actor-b",
        flex_claim_now=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS + 1.0,
    )

    assert flex_slice_claims_state_path(root=repo) == flex_slice_claims_state_path(
        root=peer_repo
    )
    assert fileloc.render_loc_board(
        first_findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5",
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam",
        ]
    )
    assert fileloc.render_loc_board(
        peer_findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5; live flex slice held by actor-a "
            "until 1970-01-01T06:01:40Z; keep this change append-only or "
            "move to another seam",
            "  peer-held flex slices redirect duplicate refactors; keep changes "
            "append-only or move to another seam",
        ]
    )
    assert fileloc.render_loc_board(
        late_findings,
        limit=5,
        flex_limit_value=7,
        byte_limit=1000,
        byte_flex_limit_value=1000,
    ) == "\n".join(
        [
            "file-loc: 1 violation(s)",
            "  FAIL  big.py: 8 lines > 5",
            "  a file that breached flex stays held to the base limit until it "
            "shrinks back under it; split by naming the seam",
        ]
    )
    assert load_flex_slice_claims(
        root=repo, now=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS + 2.0
    ) == (
        FlexSliceClaim(
            path=Path("big.py"),
            actor="actor-b",
            created_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS + 1.0,
            expires_at=100.0 + (2 * FLEX_SLICE_CLAIM_TTL_SECONDS) + 1.0,
        ),
    )


def test_complexity_peer_claim_redirects_routine_board(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path)
    (repo / "big.py").write_text("def f():\n    return 1\n", encoding="utf-8")
    save_flex_slice_claims(
        (
            FlexSliceClaim(
                path=Path("big.py"),
                actor="actor-a",
                created_at=100.0,
                expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
            ),
        ),
        root=repo,
    )
    record = complexity.ComplexityRecord(
        path="big.py", function_name="f", ccn=8, length=1, nloc=1
    )
    monkeypatch.setattr(
        complexity,
        "collect_complexity_records",
        lambda _paths, *, root, suffixes: [record],
    )

    findings = complexity.scan_staged_complexity_violations(
        [Path("big.py")],
        root=repo,
        max_ccn=5,
        ccn_flex_limit_value=7,
        max_length=100,
        length_flex_limit_value=100,
        persist=True,
        flex_actor="actor-b",
        flex_claim_now=200.0,
    )

    assert complexity.render_complexity_board(
        findings, max_ccn=5, max_length=100
    ) == "\n".join(
        [
            "complexity: 1 violation(s)",
            "  FAIL  big.py:f: ccn 8 > 5; live flex slice held by actor-a "
            "until 1970-01-01T06:01:40Z; keep this change append-only or "
            "move to another seam",
            "  peer-held flex slices redirect duplicate refactors; keep changes "
            "append-only or move to another seam",
        ]
    )


def test_repo_doc_peer_claim_redirects_guard_error(tmp_path):
    repo = _init_repo(tmp_path)
    (repo / "spice.toml").write_text(
        "[policy]\n"
        'repo_truth_docs = ["AGENTS.md"]\n'
        "\n"
        "[policy.limits]\n"
        "repo_truth_doc_chars = 5\n"
        "\n"
        "[policy.flex]\n"
        "ratio = 1.5\n"
        "\n"
        "[policy.markdown_depth_budget]\n"
        "extensions = []\n",
        encoding="utf-8",
    )
    (repo / "AGENTS.md").write_text("x" * 8, encoding="utf-8")
    save_flex_slice_claims(
        (
            FlexSliceClaim(
                path=Path("AGENTS.md"),
                actor="actor-a",
                created_at=100.0,
                expires_at=100.0 + FLEX_SLICE_CLAIM_TTL_SECONDS,
            ),
        ),
        root=repo,
    )

    findings = repodocs.repo_truth_doc_findings(
        repo,
        persist=True,
        flex_actor="actor-b",
        flex_claim_now=200.0,
    )

    assert repodocs.render_repo_truth_doc_guard_error(findings) == "\n".join(
        [
            "repo-truth docs hit peer-held flex slices; keep this change "
            "append-only or move to another seam:",
            "  AGENTS.md: 8 characters (cap 5; live flex slice held by actor-a "
            "until 1970-01-01T06:01:40Z; keep this change append-only or "
            "move to another seam)",
        ]
    )


def test_repo_doc_sticky_latch_self_heals_when_doc_drops_under_base(tmp_path):
    repo = _init_repo_with_doc_policy(tmp_path)
    doc = repo / "AGENTS.md"
    doc.write_text("x" * 8, encoding="utf-8")  # 8 > flex 7 -> latched

    repodocs.repo_truth_doc_findings(repo, persist=True)
    state = git_state_path(repodocs.REPO_DOC_CHAR_STICKY_STATE_GIT_PATH, root=repo)
    assert state.exists()

    doc.write_text("x" * 4, encoding="utf-8")  # 4 <= base 5
    healed = repodocs.repo_truth_doc_findings(repo, persist=True)

    # The latch retires the moment any scan re-measures the doc back under base,
    # even without a fully clean commit between the two scans.
    assert healed == []
    assert not state.exists()


def test_repo_doc_sticky_latch_holds_in_flex_band_until_under_base(tmp_path):
    repo = _init_repo_with_doc_policy(tmp_path)
    doc = repo / "AGENTS.md"
    doc.write_text("x" * 8, encoding="utf-8")

    repodocs.repo_truth_doc_findings(repo, persist=True)

    doc.write_text("x" * 6, encoding="utf-8")  # base 5 < 6 <= flex 7
    findings = repodocs.repo_truth_doc_findings(repo, persist=True)

    # A fresh 6-char doc passes in the flex band, but a latched one is held to
    # base until it drops under base -- self-heal must not clear early.
    assert [finding.path.as_posix() for finding in findings] == ["AGENTS.md"]
    assert findings[0].limit == 5
    state = git_state_path(repodocs.REPO_DOC_CHAR_STICKY_STATE_GIT_PATH, root=repo)
    assert state.exists()


def _init_repo_with_doc_policy(tmp_path: Path) -> Path:
    repo = _init_repo(tmp_path)
    (repo / "spice.toml").write_text(
        "[policy]\n"
        'repo_truth_docs = ["AGENTS.md"]\n'
        "\n"
        "[policy.limits]\n"
        "repo_truth_doc_chars = 5\n"
        "\n"
        "[policy.flex]\n"
        "ratio = 1.5\n"
        "\n"
        "[policy.markdown_depth_budget]\n"
        "extensions = []\n",
        encoding="utf-8",
    )
    return repo


def _flex_slice_claims_payload(repo: Path) -> dict:
    return json.loads(
        flex_slice_claims_state_path(root=repo).read_text(encoding="utf-8")
    )


def _init_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo


def _commit_all(repo: Path, message: str) -> None:
    subprocess.run(["git", "add", "."], cwd=repo, check=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.email=spice@example.test",
            "-c",
            "user.name=Spice Tests",
            "commit",
            "-q",
            "-m",
            message,
        ],
        cwd=repo,
        check=True,
    )
