"""Universal command-plan documents, digest assertions, and mounted apply."""

from __future__ import annotations

import json
import stat
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice import commandplan as commandplan_module
from spice.cli.mounts import MountedCommand, run_mounted_command
from spice.cli.parser import build_parser
from spice.commandplan import (
    COMMAND_PLAN_PROTOCOL,
    PLAN_DIGEST_HEX_LENGTH,
    apply_receipted_mounted_plan,
    assert_mounted_plan_digest,
    assert_plan_digest,
    command_plan_payload,
    load_mounted_plan_receipt,
    mounted_command_receipt_path,
    parse_command_plan_document,
)
from spice.errors import SpiceError
from spice.release import build_release_parser

FILE_MODE = 0o640
PRIVATE_FILE_MODE = 0o600


def _file_operation(
    target: str,
    *,
    before: str | None = None,
    before_mode: int | None = None,
    after: str | None = "generated\n",
    after_mode: int | None = FILE_MODE,
) -> dict[str, object]:
    return {
        "kind": "file",
        "target": target,
        "scope": "worktree-file",
        "observed_before": {"value": before, "mode": before_mode},
        "intended_after": {"value": after, "mode": after_mode},
    }


def _document(*operations: dict[str, object]):
    payload = command_plan_payload(command="fixture", operations=operations)
    document = parse_command_plan_document(json.dumps(payload))
    assert document is not None
    return document


def _init_repo(path):
    subprocess.run(
        ["git", "init", "-q", "-b", "main"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    )
    return path


def _planner(monkeypatch, document):
    calls: list[list[str]] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(document.payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    return calls


@pytest.mark.parametrize(
    "operations",
    (
        (_file_operation("one.txt"),),
        (_file_operation("one.txt"), _file_operation("two.txt")),
    ),
    ids=("single-operation", "multi-operation"),
)
def test_single_and_multi_operation_plans_share_digest_apply_and_reverse_protocol(
    tmp_path, operations
):
    _init_repo(tmp_path)
    document = _document(*operations)

    assert_plan_digest(document, document.digest)
    applied = apply_receipted_mounted_plan(document, tmp_path, "forward")
    reversed_payload = document.reversed_payload()
    reversed_document = parse_command_plan_document(json.dumps(reversed_payload))

    assert reversed_document is not None
    unapplied = apply_receipted_mounted_plan(
        reversed_document,
        tmp_path,
        "reverse",
    )
    assert document.payload["protocol"] == COMMAND_PLAN_PROTOCOL
    assert reversed_document.payload["protocol"] == COMMAND_PLAN_PROTOCOL
    assert reversed_document.payload["direction"] == "unapply"
    assert len(applied) == len(operations)
    assert len(unapplied) == len(operations)
    assert len(reversed_document.operations) == len(operations)
    assert reversed_document.digest != document.digest
    assert [operation["target"] for operation in reversed_document.operations] == [
        operation["target"] for operation in reversed(operations)
    ]
    assert all(
        not (tmp_path / str(operation["target"])).exists() for operation in operations
    )


def test_stale_digest_refusal_names_current_ordered_operations():
    preview = _document(_file_operation("generated.txt", after="first\n"))
    changed = _document(_file_operation("generated.txt", after="second\n"))

    with pytest.raises(SpiceError) as exc_info:
        assert_plan_digest(changed, preview.digest)

    message = str(exc_info.value)
    assert "stale command plan digest" in message
    assert f"expected={preview.digest}" in message
    assert f"observed={changed.digest}" in message
    assert "1:file:worktree-file:generated.txt" in message


def test_explicit_empty_digest_is_not_treated_as_bare_apply():
    document = _document(_file_operation("generated.txt"))

    with pytest.raises(SpiceError, match="non-empty string"):
        assert_plan_digest(document, "")


def test_mounted_plan_preflights_every_operation_before_the_first_write(tmp_path):
    _init_repo(tmp_path)
    existing = tmp_path / "existing.txt"
    existing.write_text("observed\n", encoding="utf-8")
    existing.chmod(FILE_MODE)
    document = _document(
        _file_operation("first.txt"),
        _file_operation(
            "existing.txt",
            before="stale\n",
            before_mode=FILE_MODE,
        ),
    )

    with pytest.raises(SpiceError, match="existing.txt"):
        apply_receipted_mounted_plan(document, tmp_path, "fixture")

    assert not (tmp_path / "first.txt").exists()
    assert existing.read_text(encoding="utf-8") == "observed\n"


def test_mounted_plan_preserves_an_explicitly_unmanaged_target(tmp_path):
    _init_repo(tmp_path)
    target = tmp_path / "operator.txt"
    target.write_text("operator-owned\n", encoding="utf-8")
    target.chmod(FILE_MODE)
    operation = _file_operation(
        "operator.txt",
        before="operator-owned\n",
        before_mode=FILE_MODE,
        after="generated\n",
    )
    operation["managed"] = False

    applied = apply_receipted_mounted_plan(
        _document(operation),
        tmp_path,
        "fixture",
    )

    assert applied == ["preserved-unmanaged:file:worktree-file:operator.txt"]
    assert target.read_text(encoding="utf-8") == "operator-owned\n"
    assert stat.S_IMODE(target.stat().st_mode) == FILE_MODE


def test_mounted_plan_applies_common_then_worktree_git_config_in_order(tmp_path):
    repo = tmp_path / "repo"
    subprocess.run(
        ["git", "init", "-b", "main", str(repo)],
        check=True,
        capture_output=True,
        text=True,
    )
    document = _document(
        {
            "kind": "git-config",
            "target": "extensions.worktreeConfig",
            "scope": "common-git-config",
            "observed_before": {"value": None, "mode": None},
            "intended_after": {"value": "true", "mode": None},
        },
        {
            "kind": "git-config",
            "target": "spice.fixture",
            "scope": "worktree-git-config",
            "observed_before": {"value": None, "mode": None},
            "intended_after": {"value": "applied", "mode": None},
        },
    )

    applied = apply_receipted_mounted_plan(document, repo, "fixture")

    common = subprocess.run(
        ["git", "-C", str(repo), "config", "--get", "extensions.worktreeConfig"],
        check=True,
        capture_output=True,
        text=True,
    )
    worktree = subprocess.run(
        ["git", "-C", str(repo), "config", "--worktree", "--get", "spice.fixture"],
        check=True,
        capture_output=True,
        text=True,
    )
    assert applied == [
        "git-config:common-git-config:extensions.worktreeConfig",
        "git-config:worktree-git-config:spice.fixture",
    ]
    assert common.stdout.strip() == "true"
    assert worktree.stdout.strip() == "applied"


@pytest.mark.parametrize("target", ("../outside.txt", "/tmp/outside.txt"))
def test_mounted_plan_refuses_file_targets_outside_the_repository(tmp_path, target):
    _init_repo(tmp_path)
    document = _document(_file_operation(target))

    with pytest.raises(SpiceError, match="escapes the repository"):
        apply_receipted_mounted_plan(document, tmp_path, "fixture")


@pytest.mark.parametrize(
    "args",
    ([], ["--apply=opaque", "--flag"]),
    ids=("bare", "apply-shaped"),
)
@pytest.mark.parametrize("returncode", (0, 7), ids=("success", "child-failure"))
def test_mounted_non_plan_output_and_exit_status_pass_through_unchanged(
    tmp_path, monkeypatch, capsys, args, returncode
):
    observed: dict[str, object] = {}

    def fake_run(argv, **kwargs):
        observed["argv"] = argv
        observed.update(kwargs)
        return SimpleNamespace(
            returncode=returncode, stdout="ordinary output\n", stderr="warning\n"
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("probe",), ("project-tool",), tmp_path)

    result = run_mounted_command(mount, args)

    captured = capsys.readouterr()
    assert result == returncode
    assert captured.out == "ordinary output\n"
    assert captured.err == "warning\n"
    assert observed["argv"] == ["project-tool", *args]
    assert observed["capture_output"] is True
    assert observed["text"] is True


def test_mounted_plan_is_applied_by_spice_without_a_second_child_command(
    tmp_path, monkeypatch, capsys
):
    _init_repo(tmp_path)
    document = _document(_file_operation("generated.txt"))
    calls: list[list[str]] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(document.payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    result = run_mounted_command(mount, [f"--apply={document.digest}"])

    assert result == 0
    assert calls == [["project-planner", f"--apply={document.digest}"]]
    target = tmp_path / "generated.txt"
    assert target.read_text(encoding="utf-8") == "generated\n"
    assert stat.S_IMODE(target.stat().st_mode) == FILE_MODE
    assert capsys.readouterr().out.splitlines() == [
        f"applied command-plan digest={document.digest} operations=1",
        "1. file:worktree-file:generated.txt",
    ]


def test_mounted_nondestructive_creation_accepts_bare_apply(
    tmp_path, monkeypatch, capsys
):
    _init_repo(tmp_path)
    document = _document(_file_operation("generated.txt"))
    calls: list[list[str]] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(document.payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    assert run_mounted_command(mount, ["--apply"]) == 0

    assert calls == [["project-planner", "--apply"]]
    assert (tmp_path / "generated.txt").read_text(encoding="utf-8") == "generated\n"
    assert f"digest={document.digest}" in capsys.readouterr().out


def test_mounted_destructive_plan_requires_digest_before_the_first_write(
    tmp_path, monkeypatch
):
    _init_repo(tmp_path)
    target = tmp_path / "victim.txt"
    target.write_text("keep\n", encoding="utf-8")
    target.chmod(FILE_MODE)
    document = _document(
        _file_operation(
            "victim.txt",
            before="keep\n",
            before_mode=FILE_MODE,
            after=None,
            after_mode=None,
        )
    )
    calls: list[list[str]] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(document.payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    with pytest.raises(SpiceError, match="destructive mounted command plan"):
        run_mounted_command(mount, ["--apply"])

    assert target.read_text(encoding="utf-8") == "keep\n"
    assert run_mounted_command(mount, [f"--apply={document.digest}"]) == 0
    assert not target.exists()
    assert calls == [
        ["project-planner", "--apply"],
        ["project-planner", f"--apply={document.digest}"],
    ]


@pytest.mark.parametrize(
    "operation",
    (
        _file_operation(
            "existing.txt",
            before="before\n",
            before_mode=FILE_MODE,
            after="after\n",
        ),
        _file_operation(
            "existing.txt",
            before="same\n",
            before_mode=FILE_MODE,
            after="same\n",
            after_mode=PRIVATE_FILE_MODE,
        ),
        {
            "kind": "git-config",
            "target": "spice.fixture",
            "scope": "worktree-git-config",
            "observed_before": {"value": "before", "mode": None},
            "intended_after": {"value": "after", "mode": None},
        },
    ),
    ids=("file-value", "file-mode", "git-config-value"),
)
def test_every_mounted_existing_state_change_requires_a_digest(tmp_path, operation):
    document = _document(operation)

    with pytest.raises(SpiceError, match="existing state would change"):
        assert_mounted_plan_digest(document, tmp_path, None)

    assert_mounted_plan_digest(document, tmp_path, document.digest)


def test_mounted_apply_replans_and_refuses_digest_after_authored_input_changes(
    tmp_path, monkeypatch, capsys
):
    _init_repo(tmp_path)
    authored = tmp_path / "authored.txt"
    authored.write_text("first\n", encoding="utf-8")
    calls: list[list[str]] = []
    captures: list[bool] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        captures.append(bool(_kwargs.get("capture_output")))
        payload = command_plan_payload(
            command="generate",
            operations=[
                _file_operation(
                    "generated.txt",
                    after=authored.read_text(encoding="utf-8"),
                )
            ],
        )
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    assert run_mounted_command(mount, []) == 0
    preview = parse_command_plan_document(capsys.readouterr().out)
    assert preview is not None
    authored.write_text("second\n", encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        run_mounted_command(mount, [f"--apply={preview.digest}"])

    assert "stale command plan digest" in str(exc_info.value)
    assert "generated.txt" in str(exc_info.value)
    assert not (tmp_path / "generated.txt").exists()
    assert calls == [
        ["project-planner"],
        ["project-planner", f"--apply={preview.digest}"],
    ]
    assert captures == [True, True]


def test_single_operation_mounted_plan_receipts_and_round_trips_without_child_unapply(
    tmp_path,
    monkeypatch,
    capsys,
):
    _init_repo(tmp_path)
    document = _document(_file_operation("generated.txt"))
    calls = _planner(monkeypatch, document)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    assert run_mounted_command(mount, []) == 0
    preview = parse_command_plan_document(capsys.readouterr().out)
    assert preview is not None
    assert preview.digest == document.digest
    assert not mounted_command_receipt_path(tmp_path, mount.name).exists()

    assert run_mounted_command(mount, [f"--apply={preview.digest}"]) == 0
    capsys.readouterr()
    receipt_path = mounted_command_receipt_path(tmp_path, mount.name)
    receipt = load_mounted_plan_receipt(tmp_path, mount.name)
    assert receipt is not None
    assert receipt.complete
    assert receipt.digest == document.digest
    assert receipt.operations == document.operations
    assert stat.S_IMODE(receipt_path.stat().st_mode) == PRIVATE_FILE_MODE

    with pytest.raises(SpiceError, match="receipt digest mismatch"):
        run_mounted_command(mount, [f"--unapply={'0' * PLAN_DIGEST_HEX_LENGTH}"])
    assert calls == [
        ["project-planner"],
        ["project-planner", f"--apply={preview.digest}"],
    ]

    assert (
        run_mounted_command(
            mount,
            [f"--unapply={receipt.digest}"],
        )
        == 0
    )
    reverse = parse_command_plan_document(capsys.readouterr().out)
    assert reverse is not None
    assert reverse.payload["direction"] == "unapply"
    assert reverse.payload["receipt_digest"] == receipt.digest
    assert (
        run_mounted_command(
            mount,
            [
                f"--unapply={receipt.digest}",
                f"--apply={reverse.digest}",
            ],
        )
        == 0
    )

    assert not (tmp_path / "generated.txt").exists()
    assert not receipt_path.exists()
    assert len(calls) == 2


def test_uninitialized_mounted_unapply_still_asserts_its_empty_plan_digest(
    tmp_path,
    monkeypatch,
    capsys,
):
    _init_repo(tmp_path)
    calls = _planner(monkeypatch, _document(_file_operation("unused.txt")))
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    assert run_mounted_command(mount, ["--unapply"]) == 0
    preview = parse_command_plan_document(capsys.readouterr().out)
    assert preview is not None
    assert preview.payload["status"] == "not-initialized"
    with pytest.raises(SpiceError, match="stale command plan digest"):
        run_mounted_command(
            mount,
            [
                "--unapply",
                f"--apply={'0' * PLAN_DIGEST_HEX_LENGTH}",
            ],
        )

    assert calls == []


def test_multi_operation_mounted_receipt_round_trips_bytes_modes_and_git_config(
    tmp_path,
    monkeypatch,
    capsys,
):
    _init_repo(tmp_path)
    existing = tmp_path / "existing.txt"
    existing.write_text("operator-before\n", encoding="utf-8")
    existing.chmod(PRIVATE_FILE_MODE)
    document = _document(
        _file_operation(
            "existing.txt",
            before="operator-before\n",
            before_mode=PRIVATE_FILE_MODE,
            after="generated-after\n",
            after_mode=FILE_MODE,
        ),
        {
            "kind": "git-config",
            "target": "extensions.worktreeConfig",
            "scope": "common-git-config",
            "observed_before": {"value": None, "mode": None},
            "intended_after": {"value": "true", "mode": None},
        },
        {
            "kind": "git-config",
            "target": "spice.fixture",
            "scope": "worktree-git-config",
            "observed_before": {"value": None, "mode": None},
            "intended_after": {"value": "mounted", "mode": None},
        },
    )
    calls = _planner(monkeypatch, document)
    mount = MountedCommand(("configure",), ("project-planner",), tmp_path)

    assert run_mounted_command(mount, [f"--apply={document.digest}"]) == 0
    capsys.readouterr()

    receipt_path = mounted_command_receipt_path(tmp_path, mount.name)
    encoded_records = receipt_path.read_text(encoding="utf-8").splitlines()
    assert len(encoded_records) == len(document.operations)
    assert [json.loads(encoded)["operation"] for encoded in encoded_records] == list(
        document.operations
    )
    assert existing.read_text(encoding="utf-8") == "generated-after\n"
    assert stat.S_IMODE(existing.stat().st_mode) == FILE_MODE
    assert _git_config(tmp_path, "extensions.worktreeConfig") == "true"
    assert _git_config(tmp_path, "spice.fixture", worktree=True) == "mounted"

    receipt = load_mounted_plan_receipt(tmp_path, mount.name)
    assert receipt is not None
    assert run_mounted_command(mount, [f"--unapply={receipt.digest}"]) == 0
    reverse = parse_command_plan_document(capsys.readouterr().out)
    assert reverse is not None
    assert len(reverse.operations) == len(document.operations)
    assert (
        run_mounted_command(
            mount,
            [
                f"--unapply={receipt.digest}",
                f"--apply={reverse.digest}",
            ],
        )
        == 0
    )

    assert existing.read_text(encoding="utf-8") == "operator-before\n"
    assert stat.S_IMODE(existing.stat().st_mode) == PRIVATE_FILE_MODE
    assert _git_config(tmp_path, "extensions.worktreeConfig") is None
    assert _git_config(tmp_path, "spice.fixture", worktree=True) is None
    assert not receipt_path.exists()
    assert len(calls) == 1


def test_mounted_apply_and_unapply_resume_from_durable_operation_prefixes(
    tmp_path,
    monkeypatch,
    capsys,
):
    _init_repo(tmp_path)
    document = _document(
        _file_operation("first.txt"),
        _file_operation("second.txt"),
    )
    planner_digests: list[str] = []

    def stateful_planner(_argv, **_kwargs):
        operations = []
        for target in ("first.txt", "second.txt"):
            path = tmp_path / target
            before = path.read_text(encoding="utf-8") if path.exists() else None
            before_mode = stat.S_IMODE(path.stat().st_mode) if path.exists() else None
            operations.append(
                _file_operation(
                    target,
                    before=before,
                    before_mode=before_mode,
                )
            )
        payload = command_plan_payload(command="fixture", operations=operations)
        planner_digests.append(str(payload["plan_digest"]))
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr(
        "spice.cli.mounts.run_parent_lifetime_command",
        stateful_planner,
    )
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)
    real_apply = commandplan_module._apply_operation
    interruption = {"calls": 0, "enabled": True}

    def interrupt_second(repo_root, operation):
        interruption["calls"] += 1
        if interruption["enabled"] and interruption["calls"] == 2:
            raise RuntimeError("interrupt operation two")
        return real_apply(repo_root, operation)

    monkeypatch.setattr(commandplan_module, "_apply_operation", interrupt_second)

    with pytest.raises(RuntimeError, match="operation two"):
        run_mounted_command(mount, [f"--apply={document.digest}"])
    receipt = load_mounted_plan_receipt(tmp_path, mount.name)
    assert receipt is not None
    assert len(receipt.operations) == 1
    assert (tmp_path / "first.txt").is_file()
    assert not (tmp_path / "second.txt").exists()

    interruption.update(calls=0, enabled=False)
    assert run_mounted_command(mount, [f"--apply={document.digest}"]) == 0
    capsys.readouterr()
    assert planner_digests[0] == document.digest
    assert planner_digests[1] != document.digest
    receipt = load_mounted_plan_receipt(tmp_path, mount.name)
    assert receipt is not None
    assert receipt.complete

    assert run_mounted_command(mount, [f"--unapply={receipt.digest}"]) == 0
    reverse = parse_command_plan_document(capsys.readouterr().out)
    assert reverse is not None
    interruption.update(calls=0, enabled=True)
    with pytest.raises(RuntimeError, match="operation two"):
        run_mounted_command(
            mount,
            [
                f"--unapply={receipt.digest}",
                f"--apply={reverse.digest}",
            ],
        )
    interrupted = load_mounted_plan_receipt(tmp_path, mount.name)
    assert interrupted is not None
    assert len(interrupted.reversals) == 1
    assert not (tmp_path / "second.txt").exists()
    assert (tmp_path / "first.txt").is_file()

    interruption["enabled"] = False
    assert (
        run_mounted_command(
            mount,
            [
                f"--unapply={receipt.digest}",
                f"--apply={reverse.digest}",
            ],
        )
        == 0
    )
    assert not (tmp_path / "first.txt").exists()
    assert not mounted_command_receipt_path(tmp_path, mount.name).exists()


def test_mounted_unapply_preserves_divergent_and_unmanaged_state(
    tmp_path,
    monkeypatch,
    capsys,
):
    _init_repo(tmp_path)
    operator = tmp_path / "operator.txt"
    operator.write_text("operator-owned\n", encoding="utf-8")
    operator.chmod(FILE_MODE)
    unmanaged = _file_operation(
        "operator.txt",
        before="operator-owned\n",
        before_mode=FILE_MODE,
        after="generated\n",
    )
    unmanaged["managed"] = False
    document = _document(_file_operation("managed.txt"), unmanaged)
    calls = _planner(monkeypatch, document)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    assert run_mounted_command(mount, [f"--apply={document.digest}"]) == 0
    capsys.readouterr()
    managed = tmp_path / "managed.txt"
    managed.write_text("operator-diverged\n", encoding="utf-8")
    managed.chmod(PRIVATE_FILE_MODE)
    receipt = load_mounted_plan_receipt(tmp_path, mount.name)
    assert receipt is not None

    assert run_mounted_command(mount, [f"--unapply={receipt.digest}"]) == 0
    reverse = parse_command_plan_document(capsys.readouterr().out)
    assert reverse is not None
    assert [operation["predicted_outcome"] for operation in reverse.operations] == [
        "preserved-unmanaged",
        "retained-diverged",
    ]
    assert (
        run_mounted_command(
            mount,
            [
                f"--unapply={receipt.digest}",
                f"--apply={reverse.digest}",
            ],
        )
        == 0
    )
    output = capsys.readouterr().out

    assert managed.read_text(encoding="utf-8") == "operator-diverged\n"
    assert stat.S_IMODE(managed.stat().st_mode) == PRIVATE_FILE_MODE
    assert operator.read_text(encoding="utf-8") == "operator-owned\n"
    recovery = next(
        Path(line.removeprefix("recovery="))
        for line in output.splitlines()
        if line.startswith("recovery=")
    )
    assert recovery.is_file()
    assert not mounted_command_receipt_path(tmp_path, mount.name).exists()
    assert len(calls) == 1


@pytest.mark.parametrize(
    ("invalidity", "message"),
    (
        ("schema", "unsupported command plan"),
        ("order", "order must be consecutive"),
        ("operation-shape", "must contain exactly value and mode"),
        ("digest", "invalid command plan digest"),
    ),
)
@pytest.mark.parametrize("returncode", (0, 7), ids=("success", "child-failure"))
def test_bare_mounted_protocol_claim_with_invalid_document_refuses(
    tmp_path, monkeypatch, invalidity, message, returncode
):
    operation = _file_operation("generated.txt")
    if invalidity == "operation-shape":
        operation["observed_before"] = {"value": None}
    payload = command_plan_payload(command="fixture", operations=[operation])
    if invalidity == "schema":
        payload["schema_version"] = 2
    elif invalidity == "order":
        payload["operations"][0]["order"] = 2
    elif invalidity == "digest":
        payload["plan_digest"] = "0" * len(str(payload["plan_digest"]))
    calls: list[list[str]] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        return SimpleNamespace(
            returncode=returncode,
            stdout=json.dumps(payload) + "\n",
            stderr="",
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("generate",), ("project-planner",), tmp_path)

    with pytest.raises(SpiceError, match=message):
        run_mounted_command(mount, [])

    assert calls == [["project-planner"]]


def test_protocol_document_refuses_noncanonical_operation_order():
    payload = command_plan_payload(
        command="fixture", operations=[_file_operation("generated.txt")]
    )
    payload["operations"][0]["order"] = 2

    with pytest.raises(SpiceError, match="order must be consecutive"):
        parse_command_plan_document(json.dumps(payload))


@pytest.mark.parametrize(
    ("parser", "argv"),
    (
        (build_parser(), ["init"]),
        (build_parser(), ["init", "--unapply"]),
        (
            build_parser(),
            ["task", "ingest", "plan.md", "--project", "task.plan"],
        ),
        (build_release_parser(), ["prepare", "minor"]),
    ),
)
def test_native_plan_verbs_accept_bare_apply_or_a_digest_assertion(parser, argv):
    digest = "a" * PLAN_DIGEST_HEX_LENGTH

    assert parser.parse_args([*argv, "--apply"]).apply is True
    assert parser.parse_args([*argv, f"--apply={digest}"]).apply == digest


def _git_config(repo, key, *, worktree=False):
    result = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "config",
            *(["--worktree"] if worktree else []),
            "--get",
            key,
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode in {0, 1}
    return result.stdout.strip() if result.returncode == 0 else None
