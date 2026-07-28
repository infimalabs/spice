"""Ownership-safe deinitialization and recovery contracts."""

from __future__ import annotations

import json
import stat
import subprocess
import sys
from dataclasses import replace
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.hooks.initplan import (
    RECEIPT_DIGEST_BYTES,
    InitOperationScope,
    InitReceiptStatus,
    InitializationReceipt,
    InitializationMode,
    apply_initialization_plan,
    initialization_receipt_path,
    load_initialization_receipt,
    plan_initialization,
    write_initialization_receipt,
)
from spice.hooks.deinitplan import (
    DeinitOutcome,
    DeinitializationReport,
    DeinitializationReceipt,
    apply_deinitialization_plan,
    deinitialization_plan_payload,
    deinitialization_plan_rows,
    load_deinitialization_receipt,
    deinitialization_receipt_path,
    plan_deinitialization,
)
from spice.paths import git_common_dir, git_dir


def _deinitialize(repo: Path) -> DeinitializationReport:
    return apply_deinitialization_plan(plan_deinitialization(repo))


def test_full_cli_round_trip_restores_whole_tree_and_git_config_identity(tmp_path):
    repo = _git_init(tmp_path / "repo")
    (repo / "README.md").write_text("pre-init state\n", encoding="utf-8")
    before_tree = _worktree_identity(repo)
    before_config = _git_config_identity(repo)

    _run([sys.executable, "-m", "spice", "init"], cwd=repo)
    after_preview = (_worktree_identity(repo), _git_config_identity(repo))
    _run([sys.executable, "-m", "spice", "init", "--apply"], cwd=repo)
    _run(
        [sys.executable, "-m", "spice", "init", "--unapply", "--apply"],
        cwd=repo,
    )
    after_reversal = (_worktree_identity(repo), _git_config_identity(repo))

    assert (after_preview, after_reversal) == (
        (before_tree, before_config),
        (before_tree, before_config),
    )


def test_round_trip_preserves_preexisting_empty_initialization_containers(tmp_path):
    repo = _git_init(tmp_path / "repo")
    (repo / ".spice/hooks").mkdir(parents=True)
    worktree_config = git_dir(repo) / "config.worktree"
    worktree_config.write_bytes(b"")
    worktree_config.chmod(0o600)
    before = (_worktree_identity(repo), _git_config_identity(repo))

    apply_initialization_plan(plan_initialization(repo))
    _deinitialize(repo)

    assert (_worktree_identity(repo), _git_config_identity(repo)) == before


def test_deinit_restores_owned_files_modes_and_scoped_config_in_reverse_order(
    tmp_path,
):
    repo = _git_init(tmp_path / "repo")
    _git(repo, "config", "core.hooksPath", ".legacy-hooks")
    hook = repo / ".spice/hooks/pre-commit"
    hook.parent.mkdir(parents=True)
    hook.write_text("#!/bin/sh\necho legacy\n", encoding="utf-8")
    hook.chmod(0o700)
    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
    apply_initialization_plan(plan)

    preview = deinitialization_plan_payload(plan_deinitialization(repo))
    report = _deinitialize(repo)

    assert report["status"] == "complete"
    assert [item["predicted_outcome"] for item in preview["operations"]] == [
        item["outcome"] for item in report["operations"]
    ]
    assert [item["target"] for item in report["operations"]] == [
        operation.target for operation in reversed(plan.operations)
    ]
    assert {item["outcome"] for item in report["operations"]} == {
        DeinitOutcome.RESTORED.value
    }
    assert (hook.read_text(encoding="utf-8"), stat.S_IMODE(hook.stat().st_mode)) == (
        "#!/bin/sh\necho legacy\n",
        0o700,
    )
    assert _git_config(repo, "core.hooksPath") == ".legacy-hooks"
    assert _git_config(repo, "extensions.worktreeConfig") is None
    assert (
        initialization_receipt_path(repo).exists(),
        deinitialization_receipt_path(repo).exists(),
        (repo / ".spice/.gitignore").exists(),
        (repo / ".spice/hooks/commit-msg").exists(),
    ) == (False, False, False, False)

    after_first = _tree_identity(repo)
    repeated = _deinitialize(repo)

    assert repeated == {
        "schema_version": 1,
        "repository": str(repo.resolve()),
        "status": "not-initialized",
        "operations": [],
        "residues": [],
        "recovery_handle": None,
    }
    assert _tree_identity(repo) == after_first


def test_deinit_preserves_divergent_file_and_config_with_recovery_handles(tmp_path):
    repo = _git_init(tmp_path / "repo")
    apply_initialization_plan(plan_initialization(repo, InitializationMode.GATES_ONLY))
    hook = repo / ".spice/hooks/pre-commit"
    hook.write_text("#!/bin/sh\necho operator-owned\n", encoding="utf-8")
    hook.chmod(0o744)
    _git(repo, "config", "--worktree", "core.hooksPath", ".operator-hooks")

    report = _deinitialize(repo)
    residues = report["residues"]

    assert [(item["target"], item["outcome"]) for item in residues] == [
        ("core.hooksPath", DeinitOutcome.RETAINED_DIVERGED.value),
        (".spice/hooks/pre-commit", DeinitOutcome.RETAINED_DIVERGED.value),
        ("extensions.worktreeConfig", DeinitOutcome.RETAINED_SHARED.value),
    ]
    assert (
        hook.read_text(encoding="utf-8"),
        stat.S_IMODE(hook.stat().st_mode),
        _git_config(repo, "core.hooksPath"),
    ) == ("#!/bin/sh\necho operator-owned\n", 0o744, ".operator-hooks")
    recovery = Path(str(report["recovery_handle"]))
    assert recovery.is_file()
    assert json.loads(recovery.read_text(encoding="utf-8")) == report
    assert [item["recovery_handle"] for item in residues] == [
        f"{recovery}#/residues/0",
        f"{recovery}#/residues/1",
        f"{recovery}#/residues/2",
    ]
    assert (initialization_receipt_path(repo).exists(),) == (False,)


def test_common_config_ownership_moves_to_a_live_worktree_until_final_deinit(
    tmp_path,
):
    primary = _git_init(tmp_path / "primary")
    (primary / "README.md").write_text("shared worktrees\n", encoding="utf-8")
    _git(primary, "add", "README.md")
    _git(primary, "commit", "-m", "seed")
    apply_initialization_plan(
        plan_initialization(primary, InitializationMode.GATES_ONLY)
    )
    secondary = tmp_path / "secondary"
    _git(
        primary,
        "worktree",
        "add",
        "-b",
        "secondary",
        str(secondary),
    )
    apply_initialization_plan(
        plan_initialization(secondary, InitializationMode.GATES_ONLY)
    )

    first_report = _deinitialize(primary)
    receiver = load_initialization_receipt(secondary)
    assert isinstance(receiver, InitializationReceipt)
    transferred = next(
        receipt_operation.operation
        for receipt_operation in receiver.operations
        if (
            receipt_operation.operation.target,
            receipt_operation.operation.scope,
        )
        == ("extensions.worktreeConfig", InitOperationScope.COMMON_GIT_CONFIG)
    )

    assert _git_config(primary, "extensions.worktreeConfig") == "true"
    assert [
        (item["target"], item["shared_owner"]) for item in first_report["residues"]
    ] == [("extensions.worktreeConfig", str(secondary.resolve()))]
    assert (transferred.previous_value, transferred.introduced) == (None, True)

    second_report = _deinitialize(secondary)

    assert (
        _git_config_file(
            git_common_dir(primary) / "config", "extensions.worktreeConfig"
        )
        is None
    )
    assert second_report["residues"] == []


def test_interrupted_deinit_resumes_from_the_durable_reverse_receipt(
    tmp_path, monkeypatch
):
    import spice.hooks.deinitplan as deinitplan

    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
    apply_initialization_plan(plan)
    real_write = deinitplan.write_deinitialization_receipt
    writes = 0

    def interrupt_after_first_reversal(receipt):
        nonlocal writes
        writes += 1
        real_write(receipt)
        if writes == 2:
            raise RuntimeError("simulated interruption")

    monkeypatch.setattr(
        deinitplan,
        "write_deinitialization_receipt",
        interrupt_after_first_reversal,
    )
    with pytest.raises(RuntimeError, match="simulated interruption"):
        _deinitialize(repo)

    interrupted = load_deinitialization_receipt(repo)
    initialization = load_initialization_receipt(repo)
    assert isinstance(interrupted, DeinitializationReceipt)
    assert isinstance(initialization, InitializationReceipt)
    assert (
        interrupted.status.value,
        tuple(state.completed for state in interrupted.operations),
        initialization.status,
    ) == (
        "reversing",
        (True, False, False, False, False, False),
        InitReceiptStatus.DEINITIALIZING,
    )
    with pytest.raises(SpiceError, match="run `spice init --unapply --apply`"):
        apply_initialization_plan(plan_initialization(repo))

    monkeypatch.setattr(
        deinitplan,
        "write_deinitialization_receipt",
        real_write,
    )
    report = _deinitialize(repo)

    assert report["status"] == "complete"
    assert [item["target"] for item in report["operations"]] == [
        operation.target for operation in reversed(plan.operations)
    ]
    assert (
        initialization_receipt_path(repo).exists(),
        deinitialization_receipt_path(repo).exists(),
    ) == (False, False)


def test_init_unapply_json_emits_digest_and_applies_the_asserted_receipt(tmp_path):
    repo = _git_init(tmp_path / "repo")
    apply_initialization_plan(plan_initialization(repo, InitializationMode.GATES_ONLY))

    before_human = _worktree_identity(repo)
    human = _run(
        [sys.executable, "-m", "spice", "init", "--unapply"],
        cwd=repo,
    )
    expected_plan = plan_deinitialization(repo)
    assert human.stdout.splitlines() == deinitialization_plan_rows(expected_plan)
    assert _worktree_identity(repo) == before_human

    completed = _run(
        [sys.executable, "-m", "spice", "init", "--unapply", "--json"],
        cwd=repo,
    )
    plan = json.loads(completed.stdout)

    assert (
        plan["protocol"],
        plan["schema_version"],
        plan["status"],
        plan["repository"],
    ) == ("spice.command-plan", 1, "preview", str(repo.resolve()))
    receipt_digest = plan["receipt_digest"]
    plan_digest = plan["plan_digest"]
    assert len(bytes.fromhex(receipt_digest)) == RECEIPT_DIGEST_BYTES
    assert [item["target"] for item in plan["operations"]] == [
        ".spice/.gitignore",
        "core.hooksPath",
        ".spice/hooks/commit-msg",
        ".spice/hooks/pre-commit",
        "core.bare",
        "extensions.worktreeConfig",
    ]
    assert initialization_receipt_path(repo).exists()

    _run(
        [
            sys.executable,
            "-m",
            "spice",
            "init",
            f"--unapply={receipt_digest}",
            f"--apply={plan_digest}",
        ],
        cwd=repo,
    )
    assert not initialization_receipt_path(repo).exists()


def test_init_unapply_refuses_digest_mismatch_and_caller_supplied_path(tmp_path):
    repo = _git_init(tmp_path / "repo")
    apply_initialization_plan(plan_initialization(repo, InitializationMode.GATES_ONLY))
    receipt_before = initialization_receipt_path(repo).read_bytes()

    for assertion in ("0" * (RECEIPT_DIGEST_BYTES * 2), "/tmp/receipt.json"):
        completed = _run_unchecked(
            [
                sys.executable,
                "-m",
                "spice",
                "init",
                f"--unapply={assertion}",
                "--apply",
            ],
            cwd=repo,
        )
        assert completed.returncode == 2
        assert (
            f"expected {assertion}" in completed.stderr
            and "observed " in completed.stderr
        )
        assert initialization_receipt_path(repo).read_bytes() == receipt_before


def test_unapply_apply_boundary_refuses_a_receipt_changed_after_planning(tmp_path):
    repo = _git_init(tmp_path / "repo")
    apply_initialization_plan(plan_initialization(repo, InitializationMode.GATES_ONLY))
    plan = plan_deinitialization(repo)
    receipt = load_initialization_receipt(repo)
    assert isinstance(receipt, InitializationReceipt)
    write_initialization_receipt(replace(receipt, mode=InitializationMode.FULL))

    with pytest.raises(
        SpiceError,
        match=(
            rf"receipt changed after unapply planning: "
            rf"expected {plan.receipt_digest}; observed "
        ),
    ):
        apply_deinitialization_plan(plan)

    assert (repo / ".spice/hooks/pre-commit").is_file()


def test_deinit_refuses_with_withdrawal_release_and_current_replacement(tmp_path):
    repo = _git_init(tmp_path / "repo")

    completed = _run_unchecked(
        [sys.executable, "-m", "spice", "deinit"],
        cwd=repo,
    )

    assert completed.returncode == 2
    assert "withdrawn in v0.30.0" in completed.stderr
    assert "`spice init --unapply`" in completed.stderr


def _git_init(repo: Path) -> Path:
    _run(["git", "init", "-b", "main", str(repo)])
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Spice Test")
    return repo


def _git(repo: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    return _run(["git", "-C", str(repo), *arguments])


def _git_config(repo: Path, key: str) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(repo), "config", "--get", key],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode == 1:
        return None
    result.check_returncode()
    return result.stdout.strip()


def _git_config_file(path: Path, key: str) -> str | None:
    result = subprocess.run(
        ["git", "config", "--file", str(path), "--get", key],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode == 1:
        return None
    result.check_returncode()
    return result.stdout.strip()


def _tree_identity(root: Path) -> tuple[tuple[str, int, bytes], ...]:
    return tuple(
        (
            path.relative_to(root).as_posix(),
            stat.S_IMODE(path.stat().st_mode),
            path.read_bytes(),
        )
        for path in sorted(
            candidate for candidate in root.rglob("*") if candidate.is_file()
        )
    )


def _worktree_identity(root: Path) -> tuple[tuple[str, int, bytes | None], ...]:
    return tuple(
        (
            path.relative_to(root).as_posix(),
            stat.S_IMODE(path.lstat().st_mode),
            path.read_bytes() if path.is_file() else None,
        )
        for path in sorted(
            candidate
            for candidate in root.rglob("*")
            if candidate.relative_to(root).parts[0] != ".git"
        )
    )


def _git_config_identity(
    root: Path,
) -> tuple[tuple[str, int | None, bytes | None], ...]:
    paths = (git_common_dir(root) / "config", git_dir(root) / "config.worktree")
    return tuple(
        (
            path.name,
            stat.S_IMODE(path.stat().st_mode) if path.is_file() else None,
            path.read_bytes() if path.is_file() else None,
        )
        for path in paths
    )


def _run(
    command: list[str], *, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=True,
        capture_output=True,
        cwd=cwd,
        text=True,
    )


def _run_unchecked(
    command: list[str], *, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=False,
        capture_output=True,
        cwd=cwd,
        text=True,
    )
