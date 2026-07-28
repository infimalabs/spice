"""Receipt-shaped initialization planning across repository layouts and modes."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from dataclasses import replace
from pathlib import Path

import pytest

from spice.agent.lifecycle import WORKTREE_SKILL_RELATIVE_PATH
from spice.errors import SpiceError
from spice.hooks.initplan import (
    InitOperation,
    InitOperationKind,
    InitOperationScope,
    InitReceiptEvent,
    INIT_RECEIPT_MODE,
    OWNERSHIP_DIGEST_BYTES,
    RECEIPT_RECORD_MAX_BYTES,
    InitReceiptStatus,
    InitializationMode,
    InitializationPlan,
    InitializationReceiptRecord,
    append_initialization_receipt_record,
    apply_initialization_plan,
    encode_initialization_receipt_record,
    git_config_file_get,
    initialization_plan_digest,
    initialization_plan_payload,
    initialization_preview_rows,
    initialization_receipt_digest,
    initialization_receipt_path,
    initialization_receipt_record_payload,
    load_initialization_receipt,
    load_initialization_receipt_records,
    plan_initialization,
)

OPEN_MODE_DEFAULT = 0o777


def test_git_config_file_get_distinguishes_present_absent_and_failure(tmp_path):
    config = tmp_path / "config"
    _run(["git", "config", "--file", str(config), "core.hooksPath", ".shared-hooks"])
    assert git_config_file_get(config, "core.hooksPath") == ".shared-hooks"
    assert git_config_file_get(config, "missing.key") is None
    config.write_text("[broken\n", encoding="utf-8")
    with pytest.raises(SpiceError, match="Git config.*bad config line"):
        git_config_file_get(config, "core.hooksPath")


def test_full_plan_is_ordered_complete_deterministic_and_side_effect_free(tmp_path):
    repo = _git_init(tmp_path / "repo")
    config = repo / ".git" / "config"
    before_entries = tuple(sorted(path.name for path in repo.iterdir()))
    before_git_entries = tuple(sorted(path.name for path in (repo / ".git").iterdir()))
    before_config = config.read_text(encoding="utf-8")

    plan = plan_initialization(repo)
    repeated = plan_initialization(repo)

    assert plan == repeated
    assert tuple(operation.target for operation in plan.operations) == (
        "extensions.worktreeConfig",
        "core.bare",
        ".spice/hooks/pre-commit",
        ".spice/hooks/commit-msg",
        ".spice/hooks/reference-transaction",
        "core.hooksPath",
        ".spice/.gitignore",
        ".agents/skills/spice/.gitignore",
        ".agents/skills/spice/SKILL.md",
    )
    assert {operation.initialization_mode for operation in plan.operations} == {
        InitializationMode.FULL
    }
    assert tuple(sorted(path.name for path in repo.iterdir())) == before_entries
    assert tuple(sorted(path.name for path in (repo / ".git").iterdir())) == (
        before_git_entries
    )
    assert config.read_text(encoding="utf-8") == before_config
    assert {
        operation.target: operation.generated_mode
        for operation in plan.operations
        if operation.kind is InitOperationKind.FILE
    } == {
        ".spice/hooks/pre-commit": 0o755,
        ".spice/hooks/commit-msg": 0o755,
        ".spice/hooks/reference-transaction": 0o755,
        ".spice/.gitignore": 0o644,
        ".agents/skills/spice/.gitignore": 0o600,
        ".agents/skills/spice/SKILL.md": 0o600,
    }
    assert {len(operation.ownership_digest) for operation in plan.operations} == {
        OWNERSHIP_DIGEST_BYTES * 2
    }
    assert {
        len(bytes.fromhex(operation.ownership_digest)) for operation in plan.operations
    } == {OWNERSHIP_DIGEST_BYTES}


def test_apply_consumes_the_existing_plan_and_realizes_every_managed_operation(
    tmp_path,
):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo)
    planned_operations = plan.operations

    result = apply_initialization_plan(plan)

    assert result.status is InitReceiptStatus.COMPLETE
    assert {operation.completed for operation in result.operations} == {True}
    assert plan.operations == planned_operations
    assert tuple(_realized_value(repo, operation) for operation in plan.operations) == (
        tuple(operation.generated_value for operation in plan.operations)
    )
    assert tuple(
        stat.S_IMODE((repo / operation.target).stat().st_mode)
        for operation in plan.operations
        if operation.kind is InitOperationKind.FILE
    ) == tuple(
        operation.generated_mode
        for operation in plan.operations
        if operation.kind is InitOperationKind.FILE
    )


def test_full_plan_materializes_in_place_oops_triage_guidance(tmp_path):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo)
    skill = _operation(
        plan,
        WORKTREE_SKILL_RELATIVE_PATH.as_posix(),
        InitOperationScope.WORKTREE_FILE,
    )
    content = str(skill.generated_value)

    assert "spice task claim <handle>" in content
    assert "Oops rows already use the plan flow" in content
    assert "origin=task:<oops-handle>" in content
    assert "spice task depends <oops-handle> --after <child...>" in content
    assert "without a separate wake-path write" in content


def test_gates_only_plan_uses_the_same_model_for_its_bounded_surface(tmp_path):
    repo = _git_init(tmp_path / "repo")

    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
    apply_initialization_plan(plan)

    assert tuple(operation.target for operation in plan.operations) == (
        "extensions.worktreeConfig",
        "core.bare",
        ".spice/hooks/pre-commit",
        ".spice/hooks/commit-msg",
        "core.hooksPath",
        ".spice/.gitignore",
    )
    assert {operation.initialization_mode for operation in plan.operations} == {
        InitializationMode.GATES_ONLY
    }
    assert tuple(
        sorted(
            path.relative_to(repo).as_posix()
            for path in (repo / ".spice").rglob("*")
            if path.is_file()
        )
    ) == (
        ".spice/.gitignore",
        ".spice/hooks/commit-msg",
        ".spice/hooks/pre-commit",
    )


def test_human_and_json_preview_share_one_plan_and_leave_identical_bytes(tmp_path):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo)
    before = _tree_identity(repo)

    human = _run([sys.executable, "-m", "spice", "init"], cwd=repo)
    after_human = _tree_identity(repo)
    machine = _run(
        [sys.executable, "-m", "spice", "init", "--json"],
        cwd=repo,
    )
    after_machine = _tree_identity(repo)

    assert human.stdout.splitlines() == initialization_preview_rows(plan)
    machine_payload = json.loads(machine.stdout)
    assert machine_payload == initialization_plan_payload(plan)
    assert machine_payload["protocol"] == "spice.command-plan"
    assert len(machine_payload["plan_digest"]) == OWNERSHIP_DIGEST_BYTES * 2
    assert [item["order"] for item in machine_payload["operations"]] == list(
        range(1, len(plan.operations) + 1)
    )
    assert (before, after_human, after_machine) == (before, before, before)


def test_init_digest_refuses_when_repository_changes_after_preview(tmp_path):
    repo = _git_init(tmp_path / "repo")
    preview = initialization_plan_payload(plan_initialization(repo))
    digest = str(preview["plan_digest"])
    hook = repo / ".spice" / "hooks" / "pre-commit"
    hook.parent.mkdir(parents=True)
    hook.write_text("#!/bin/sh\necho operator-change\n", encoding="utf-8")
    before = hook.read_bytes()

    result = subprocess.run(
        [sys.executable, "-m", "spice", "init", f"--apply={digest}"],
        check=False,
        capture_output=True,
        cwd=repo,
        text=True,
    )

    assert result.returncode == 2
    assert "stale command plan digest" in result.stderr
    assert ".spice/hooks/pre-commit" in result.stderr
    assert hook.read_bytes() == before
    assert not initialization_receipt_path(repo).exists()


def test_apply_appends_one_private_total_record_for_each_completed_operation(tmp_path):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)

    receipt = apply_initialization_plan(plan)
    receipt_path = initialization_receipt_path(repo)
    records = load_initialization_receipt_records(repo)
    stored = tuple(
        json.loads(line)
        for line in receipt_path.read_text(encoding="utf-8").splitlines()
    )

    assert receipt.status is InitReceiptStatus.COMPLETE
    assert stored == tuple(
        initialization_receipt_record_payload(record) for record in records
    )
    assert len(records) == len(plan.operations)
    assert {record.event for record in records} == {InitReceiptEvent.APPLY}
    assert tuple(record.operation_index for record in records) == tuple(
        range(len(plan.operations))
    )
    assert {record.operation_count for record in records} == {len(plan.operations)}
    assert stat.S_IMODE(receipt_path.stat().st_mode) == INIT_RECEIPT_MODE
    assert tuple(
        (
            operation.operation.target,
            operation.operation.previous_value,
            operation.operation.generated_value,
            operation.operation.previous_mode,
            operation.operation.generated_mode,
            operation.operation.ownership_digest,
            operation.operation.scope.value,
            operation.operation.initialization_mode.value,
            operation.completed,
        )
        for operation in receipt.operations
    ) == tuple(
        (
            operation.target,
            operation.previous_value,
            operation.generated_value,
            operation.previous_mode,
            operation.generated_mode,
            operation.ownership_digest,
            operation.scope.value,
            operation.initialization_mode.value,
            True,
        )
        for operation in plan.operations
    )
    assert initialization_receipt_digest(receipt) == initialization_plan_digest(plan)


def test_repeated_apply_preserves_the_complete_receipt_and_repository_bytes(tmp_path):
    repo = _git_init(tmp_path / "repo")
    first = apply_initialization_plan(plan_initialization(repo))
    after_first = _tree_identity(repo)

    second = apply_initialization_plan(plan_initialization(repo))
    after_second = _tree_identity(repo)

    assert second == first
    assert (after_first, after_second) == (after_first, after_first)


def test_repository_config_approval_appends_one_replayable_fact(tmp_path):
    repo = _git_init(tmp_path / "repo")
    apply_initialization_plan(plan_initialization(repo, InitializationMode.GATES_ONLY))
    path = initialization_receipt_path(repo)
    prefix = path.read_bytes()
    before = load_initialization_receipt_records(repo)

    approved = apply_initialization_plan(
        plan_initialization(repo, InitializationMode.GATES_ONLY),
        approve_repository_config=True,
    )

    records = load_initialization_receipt_records(repo)
    replayed = load_initialization_receipt(repo)
    assert path.read_bytes().startswith(prefix)
    assert len(records) == len(before) + 1
    assert records[-1].event is InitReceiptEvent.APPROVAL
    assert records[-1].approved_repository_config_digest is not None
    assert replayed == approved


def test_gates_receipt_promotes_to_full_without_losing_first_introduction(tmp_path):
    repo = _git_init(tmp_path / "repo")
    gates = apply_initialization_plan(
        plan_initialization(repo, InitializationMode.GATES_ONLY)
    )

    full = apply_initialization_plan(plan_initialization(repo, InitializationMode.FULL))
    by_target = {
        receipt_operation.operation.target: receipt_operation.operation
        for receipt_operation in full.operations
    }

    assert (gates.mode, full.mode, full.status) == (
        InitializationMode.GATES_ONLY,
        InitializationMode.FULL,
        InitReceiptStatus.COMPLETE,
    )
    assert (
        by_target["extensions.worktreeConfig"].previous_value,
        by_target["extensions.worktreeConfig"].initialization_mode,
        by_target[".spice/hooks/reference-transaction"].previous_value,
        by_target[".spice/hooks/reference-transaction"].initialization_mode,
    ) == (None, InitializationMode.GATES_ONLY, None, InitializationMode.FULL)


def test_apply_resumes_incomplete_receipt_and_preserves_original_file_provenance(
    tmp_path, monkeypatch
):
    import spice.hooks.initplan as initplan

    repo = _git_init(tmp_path / "repo")
    hook = repo / ".spice/hooks/pre-commit"
    hook.parent.mkdir(parents=True)
    hook.write_text("#!/bin/sh\necho custom\n", encoding="utf-8")
    hook.chmod(0o700)
    plan = plan_initialization(repo)
    real_append = initplan.append_initialization_receipt_record
    appends = 0

    def interrupt_after_third_completion(record, *, encoded=None):
        nonlocal appends
        real_append(record, encoded=encoded)
        appends += 1
        if appends == 3:
            raise RuntimeError("simulated interruption")

    monkeypatch.setattr(
        initplan,
        "append_initialization_receipt_record",
        interrupt_after_third_completion,
    )
    with pytest.raises(RuntimeError, match="simulated interruption"):
        apply_initialization_plan(plan)

    interrupted_records = load_initialization_receipt_records(repo)
    interrupted = load_initialization_receipt(repo)
    assert interrupted is not None
    assert interrupted.status is InitReceiptStatus.APPLYING
    assert tuple(record.operation.target for record in interrupted_records) == tuple(
        operation.target for operation in plan.operations[:3]
    )
    assert {item.completed for item in interrupted.operations} == {True}

    monkeypatch.setattr(
        initplan,
        "append_initialization_receipt_record",
        real_append,
    )
    hook.unlink()

    resumed = apply_initialization_plan(plan_initialization(repo))
    stored = load_initialization_receipt(repo)
    pre_commit = next(
        operation.operation
        for operation in resumed.operations
        if operation.operation.target == ".spice/hooks/pre-commit"
    )

    assert resumed.status is InitReceiptStatus.COMPLETE
    assert stored == resumed
    assert (pre_commit.previous_value, pre_commit.previous_mode) == (
        "#!/bin/sh\necho custom\n",
        0o700,
    )
    assert hook.read_text(encoding="utf-8") == pre_commit.generated_value
    assert stat.S_IMODE(hook.stat().st_mode) == pre_commit.generated_mode


def test_receipt_append_uses_one_bounded_unbuffered_o_append_write(
    tmp_path, monkeypatch
):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
    record = InitializationReceiptRecord(
        repo_root=repo.resolve(),
        mode=plan.mode,
        plan_schema_version=plan.schema_version,
        event=InitReceiptEvent.APPLY,
        operation_index=0,
        operation_count=len(plan.operations),
        operation=plan.operations[0],
    )
    real_open = os.open
    real_write = os.write
    opened_flags: list[int] = []
    writes: list[bytes] = []
    receipt_path = initialization_receipt_path(repo)

    def observed_open(path, flags, mode=OPEN_MODE_DEFAULT):
        if Path(path) == receipt_path:
            opened_flags.append(flags)
        return real_open(path, flags, mode)

    def observed_write(descriptor, payload):
        writes.append(payload)
        return real_write(descriptor, payload)

    def refuse_receipt_rebuild(*_args, **_kwargs):
        raise AssertionError("receipt append must not use temporary replacement")

    monkeypatch.setattr("spice.hooks.initplan.os.open", observed_open)
    monkeypatch.setattr("spice.hooks.initplan.os.write", observed_write)
    monkeypatch.setattr(
        "spice.hooks.initplan.atomic_write_text",
        refuse_receipt_rebuild,
    )

    encoded = encode_initialization_receipt_record(record)
    append_initialization_receipt_record(record, encoded=encoded)

    assert len(opened_flags) == 1
    assert opened_flags[0] & os.O_APPEND
    assert writes == [encoded]


def test_oversized_receipt_record_refuses_before_creating_the_log(tmp_path):
    repo = _git_init(tmp_path / "repo")
    plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
    oversized = replace(
        plan.operations[0],
        generated_value="x" * RECEIPT_RECORD_MAX_BYTES,
    )
    record = InitializationReceiptRecord(
        repo_root=repo.resolve(),
        mode=plan.mode,
        plan_schema_version=plan.schema_version,
        event=InitReceiptEvent.APPLY,
        operation_index=0,
        operation_count=len(plan.operations),
        operation=oversized,
    )

    with pytest.raises(
        SpiceError,
        match="receipt record exceeds encoded byte bound",
    ):
        append_initialization_receipt_record(record)

    assert not initialization_receipt_path(repo).exists()


def test_concurrent_process_appends_preserve_every_complete_record(tmp_path):
    repo = _git_init(tmp_path / "repo")
    operation_count = len(
        plan_initialization(repo, InitializationMode.GATES_ONLY).operations
    )
    script = """
import sys
from pathlib import Path
from spice.hooks.initplan import (
    InitReceiptEvent,
    InitializationMode,
    InitializationReceiptRecord,
    append_initialization_receipt_record,
    plan_initialization,
)
repo = Path(sys.argv[1]).resolve()
index = int(sys.argv[2])
plan = plan_initialization(repo, InitializationMode.GATES_ONLY)
append_initialization_receipt_record(
    InitializationReceiptRecord(
        repo_root=repo,
        mode=plan.mode,
        plan_schema_version=plan.schema_version,
        event=InitReceiptEvent.APPLY,
        operation_index=index,
        operation_count=len(plan.operations),
        operation=plan.operations[index],
    )
)
"""
    processes = [
        subprocess.Popen(
            [sys.executable, "-c", script, str(repo), str(index)],
            cwd=Path(__file__).parents[1],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for index in range(operation_count)
    ]
    failures = [
        (process.returncode, stdout, stderr)
        for process in processes
        for stdout, stderr in [process.communicate()]
        if process.returncode != 0
    ]

    assert failures == []
    path = initialization_receipt_path(repo)
    lines = path.read_bytes().splitlines(keepends=True)
    records = load_initialization_receipt_records(repo)
    receipt = load_initialization_receipt(repo)

    assert len(lines) == operation_count
    assert all(line.endswith(b"\n") for line in lines)
    assert all(isinstance(json.loads(line), dict) for line in lines)
    assert {record.operation_index for record in records} == set(range(operation_count))
    assert receipt is not None
    assert receipt.status is InitReceiptStatus.COMPLETE


def test_custom_common_hooks_path_is_preserved_as_effective_prior_state(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _git(repo, "config", "core.hooksPath", ".custom-hooks")

    plan = plan_initialization(repo)
    operation = _operation(
        plan,
        "core.hooksPath",
        InitOperationScope.WORKTREE_GIT_CONFIG,
    )

    assert (
        operation.previous_value,
        operation.previous_effective_value,
        operation.generated_value,
        operation.introduced,
    ) == (None, ".custom-hooks", ".spice/hooks", True)
    apply_initialization_plan(plan)
    assert _git_config_file(repo / ".git" / "config", "core.hooksPath") == (
        ".custom-hooks"
    )
    assert _git_config_file(repo / ".git" / "config.worktree", "core.hooksPath") == (
        ".spice/hooks"
    )


def test_preexisting_shared_and_worktree_values_are_not_marked_introduced(tmp_path):
    repo = _git_init(tmp_path / "repo")
    _git(repo, "config", "extensions.worktreeConfig", "true")
    _git(repo, "config", "--worktree", "core.bare", "false")
    _git(repo, "config", "--worktree", "core.hooksPath", ".spice/hooks")

    plan = plan_initialization(repo)
    provenance = tuple(
        (
            operation.target,
            operation.scope,
            operation.previous_value,
            operation.introduced,
            operation.will_change,
        )
        for operation in plan.operations
        if operation.kind is InitOperationKind.GIT_CONFIG
    )

    assert provenance == (
        (
            "extensions.worktreeConfig",
            InitOperationScope.COMMON_GIT_CONFIG,
            "true",
            False,
            False,
        ),
        (
            "core.bare",
            InitOperationScope.WORKTREE_GIT_CONFIG,
            "false",
            False,
            False,
        ),
        (
            "core.hooksPath",
            InitOperationScope.WORKTREE_GIT_CONFIG,
            ".spice/hooks",
            False,
            False,
        ),
    )


def test_linked_bare_common_worktree_can_be_planned_before_git_bootstrap(tmp_path):
    from spice.hooks.cli import init_repo_root

    seed = _git_init(tmp_path / "seed")
    (seed / "README.md").write_text("seed\n", encoding="utf-8")
    _git(seed, "add", "README.md")
    _git(seed, "commit", "-m", "seed")
    common = tmp_path / "common.git"
    _run(["git", "clone", "--bare", str(seed), str(common)])
    lane = tmp_path / "lane"
    _git(common, "worktree", "add", str(lane), "main")
    common_config_before = (common / "config").read_text(encoding="utf-8")

    discovered = init_repo_root(lane)
    plan = plan_initialization(discovered)
    bare = _operation(plan, "core.bare", InitOperationScope.WORKTREE_GIT_CONFIG)

    assert (
        bare.scope_path,
        bare.previous_value,
        bare.previous_effective_value,
        bare.generated_value,
        bare.introduced,
    ) == (
        common / "worktrees" / "lane" / "config.worktree",
        None,
        "true",
        "false",
        True,
    )
    assert (discovered, (common / "config").read_text(encoding="utf-8")) == (
        lane,
        common_config_before,
    )
    apply_initialization_plan(plan)
    assert _git(lane, "rev-parse", "--is-bare-repository").stdout.strip() == "false"
    assert _git(
        lane, "config", "--worktree", "--get", "core.hooksPath"
    ).stdout.strip() == (".spice/hooks")


def test_bare_common_lane_git_and_marker_discovery_match(tmp_path, monkeypatch):
    """The documented fallback yields the primary resolver's exact plan."""
    from spice.hooks import cli

    seed = _git_init(tmp_path / "seed")
    (seed / "README.md").write_text("seed\n", encoding="utf-8")
    _git(seed, "add", "README.md")
    _git(seed, "commit", "-m", "seed")
    common = tmp_path / "common.git"
    _run(["git", "clone", "--bare", str(seed), str(common)])
    lane = tmp_path / "lane"
    _git(common, "worktree", "add", str(lane), "main")

    asked_git = cli.init_repo_root(lane)
    monkeypatch.setattr(cli, "repo_root_from_cwd", lambda _cwd=None: None)
    walked_marker = cli.init_repo_root(lane)
    scope_paths = tuple(
        _operation(
            plan_initialization(root),
            "core.bare",
            InitOperationScope.WORKTREE_GIT_CONFIG,
        ).scope_path
        for root in (asked_git, walked_marker)
    )

    assert (asked_git, walked_marker, scope_paths) == (
        lane,
        lane,
        (common / "worktrees" / "lane" / "config.worktree",) * 2,
    )


def test_bare_common_lane_plan_names_a_failing_git_rather_than_the_tree(
    tmp_path, monkeypatch
):
    """Under contention this plan reports the git that failed, not a false verdict."""
    import spice.paths as paths

    seed = _git_init(tmp_path / "seed")
    (seed / "README.md").write_text("seed\n", encoding="utf-8")
    _git(seed, "add", "README.md")
    _git(seed, "commit", "-m", "seed")
    common = tmp_path / "common.git"
    _run(["git", "clone", "--bare", str(seed), str(common)])
    lane = tmp_path / "lane"
    _git(common, "worktree", "add", str(lane), "main")
    healthy = _operation(
        plan_initialization(lane), "core.bare", InitOperationScope.WORKTREE_GIT_CONFIG
    )

    def contended(command, **_kwargs):
        return subprocess.CompletedProcess(
            list(command),
            returncode=128,
            stdout="",
            stderr="fatal: Unable to create 'index.lock': File exists.\n",
        )

    monkeypatch.setattr(paths, "run_git_command", contended)
    with pytest.raises(SpiceError) as contended_failure:
        plan_initialization(lane)

    message = str(contended_failure.value)
    assert healthy.scope_path == common / "worktrees" / "lane" / "config.worktree"
    assert "git command failed" in message
    assert "--git-common-dir" in message
    assert "index.lock" in message


def test_tracked_custom_skill_is_inventoried_but_preserved(tmp_path):
    repo = _git_init(tmp_path / "repo")
    skill = repo / WORKTREE_SKILL_RELATIVE_PATH
    skill.parent.mkdir(parents=True)
    skill.write_text("# Repository-owned spice skill\n", encoding="utf-8")
    skill.chmod(0o644)
    _git(repo, "add", WORKTREE_SKILL_RELATIVE_PATH.as_posix())
    _git(repo, "commit", "-m", "repository skill")

    plan = plan_initialization(repo)
    operation = _operation(
        plan,
        WORKTREE_SKILL_RELATIVE_PATH.as_posix(),
        InitOperationScope.WORKTREE_FILE,
    )

    assert (
        operation.previous_value,
        operation.previous_mode,
        operation.generated_mode,
        operation.introduced,
        operation.managed,
        operation.will_change,
    ) == ("# Repository-owned spice skill\n", 0o644, 0o644, False, False, False)
    apply_initialization_plan(plan)
    assert skill.read_text(encoding="utf-8") == "# Repository-owned spice skill\n"


def _operation(
    plan: InitializationPlan,
    target: str,
    scope: InitOperationScope,
) -> InitOperation:
    return next(
        operation
        for operation in plan.operations
        if (operation.target, operation.scope) == (target, scope)
    )


def _realized_value(repo: Path, operation: InitOperation) -> str:
    if operation.kind is InitOperationKind.FILE:
        return (repo / operation.target).read_text(encoding="utf-8")
    return _git_config_file(operation.scope_path, operation.target)


def _git_config_file(path: Path, key: str) -> str:
    return _run(["git", "config", "--file", str(path), "--get", key]).stdout.strip()


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


def _git_init(repo: Path) -> Path:
    _run(["git", "init", "-b", "main", str(repo)])
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Spice Test")
    return repo


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return _run(["git", "-C", str(repo), *args])


def _run(
    argv: list[str], *, cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        check=True,
        capture_output=True,
        cwd=cwd,
        text=True,
    )
