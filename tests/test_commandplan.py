"""Universal command-plan documents, digest assertions, and mounted apply."""

from __future__ import annotations

import json
import stat
import subprocess
from types import SimpleNamespace

import pytest

from spice.cli.mounts import MountedCommand, run_mounted_command
from spice.cli.parser import build_parser
from spice.commandplan import (
    COMMAND_PLAN_PROTOCOL,
    PLAN_DIGEST_HEX_LENGTH,
    apply_mounted_plan,
    assert_plan_digest,
    command_plan_payload,
    parse_command_plan_document,
)
from spice.errors import SpiceError
from spice.release import build_release_parser

FILE_MODE = 0o640


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
    document = _document(*operations)

    assert_plan_digest(document, document.digest)
    applied = apply_mounted_plan(document, tmp_path)
    reversed_payload = document.reversed_payload()
    reversed_document = parse_command_plan_document(json.dumps(reversed_payload))

    assert reversed_document is not None
    unapplied = apply_mounted_plan(reversed_document, tmp_path)
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
        apply_mounted_plan(document, tmp_path)

    assert not (tmp_path / "first.txt").exists()
    assert existing.read_text(encoding="utf-8") == "observed\n"


def test_mounted_plan_preserves_an_explicitly_unmanaged_target(tmp_path):
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

    applied = apply_mounted_plan(_document(operation), tmp_path)

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

    applied = apply_mounted_plan(document, repo)

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
    document = _document(_file_operation(target))

    with pytest.raises(SpiceError, match="escapes the repository"):
        apply_mounted_plan(document, tmp_path)


def test_mounted_non_plan_output_and_exit_status_pass_through_unchanged(
    tmp_path, monkeypatch, capsys
):
    observed: dict[str, object] = {}

    def fake_run(argv, **kwargs):
        observed["argv"] = argv
        observed.update(kwargs)
        return SimpleNamespace(
            returncode=7, stdout="ordinary output\n", stderr="warning\n"
        )

    monkeypatch.setattr("spice.cli.mounts.run_parent_lifetime_command", fake_run)
    mount = MountedCommand(("probe",), ("project-tool",), tmp_path)

    result = run_mounted_command(mount, ["--apply=opaque", "--flag"])

    captured = capsys.readouterr()
    assert result == 7
    assert captured.out == "ordinary output\n"
    assert captured.err == "warning\n"
    assert observed["argv"] == ["project-tool", "--apply=opaque", "--flag"]
    assert observed["capture_output"] is True
    assert observed["text"] is True


def test_mounted_plan_is_applied_by_spice_without_a_second_child_command(
    tmp_path, monkeypatch, capsys
):
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


def test_mounted_apply_replans_and_refuses_digest_after_authored_input_changes(
    tmp_path, monkeypatch, capsys
):
    authored = tmp_path / "authored.txt"
    authored.write_text("first\n", encoding="utf-8")
    calls: list[list[str]] = []

    def fake_run(argv, **_kwargs):
        calls.append(argv)
        payload = command_plan_payload(
            command="generate",
            operations=[
                _file_operation(
                    "generated.txt",
                    after=authored.read_text(encoding="utf-8"),
                )
            ],
        )
        if not _kwargs.get("capture_output"):
            print(json.dumps(payload))
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


def test_protocol_marker_with_invalid_digest_refuses_instead_of_passing_through():
    payload = command_plan_payload(
        command="fixture", operations=[_file_operation("generated.txt")]
    )
    payload["plan_digest"] = "0" * len(str(payload["plan_digest"]))

    with pytest.raises(SpiceError, match="invalid command plan digest"):
        parse_command_plan_document(json.dumps(payload))


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
        (build_parser(), ["deinit"]),
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
