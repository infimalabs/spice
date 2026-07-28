"""Mounted release plans keep preview authority separate from command execution."""

from __future__ import annotations

import io
import json
import os
from contextlib import redirect_stderr, redirect_stdout
from types import SimpleNamespace
from unittest.mock import patch

import pytest

import spice.commandownership as commandownership
import spice.release as release
from spice.cli.mounts import MountedCommand, run_mounted_command
from spice.commandownership import (
    COMMAND_PLAN_EXECUTION_DIGEST_ENV,
    COMMAND_PLAN_EXECUTOR,
    LEGACY_COMMAND_OWNER_CANDIDATE_VERSION,
    LEGACY_COMMAND_OWNER_PARENT_VERSION,
    MOUNTED_COMMAND_ENV,
    MOUNTED_RUNTIME_PYTHON_ENV,
    defer_command_owned_apply,
)
from spice.commandplan import command_plan_payload
from spice.errors import SpiceError


def _release_mount(tmp_path, monkeypatch):
    effects = []
    calls = []

    monkeypatch.setattr(release, "repo_root", lambda: tmp_path)
    monkeypatch.setattr(release, "ensure_clean_worktree", lambda root: None)
    monkeypatch.setattr(release, "ensure_release_preconditions", lambda root: None)
    monkeypatch.setattr(release, "preview_bumped_version", lambda bump: "0.30.2")
    monkeypatch.setattr(release, "current_version", lambda: "0.30.1")
    monkeypatch.setattr(release, "git", lambda *args: "source-head")
    monkeypatch.setattr(
        release,
        "apply_release_plan",
        lambda args, root, plan: (
            effects.append(
                (
                    plan.payload(),
                    os.environ.get(  # env-policy: allow
                        COMMAND_PLAN_EXECUTION_DIGEST_ENV
                    ),
                )
            )
            or 0
        ),
    )
    monkeypatch.setattr(
        "spice.cli.mounts.require_repository_config_approval",
        lambda *args, **kwargs: None,
    )

    def run_release(argv, **kwargs):
        stdout = io.StringIO()
        stderr = io.StringIO()
        calls.append(
            (
                list(argv),
                kwargs["env"].get(COMMAND_PLAN_EXECUTION_DIGEST_ENV),
                kwargs["capture_output"],
            )
        )
        with (
            patch.dict(  # env-policy: allow
                os.environ, kwargs["env"], clear=True
            ),
            redirect_stdout(stdout),
            redirect_stderr(stderr),
        ):
            returncode = release.main(list(argv[1:]))
        return SimpleNamespace(
            returncode=returncode,
            stdout=stdout.getvalue(),
            stderr=stderr.getvalue(),
        )

    monkeypatch.setattr(
        "spice.cli.mounts.run_parent_lifetime_command",
        run_release,
    )
    return MountedCommand(("release",), ("release-tool",), tmp_path), effects, calls


def test_mounted_release_prepare_and_publish_previews_render_without_mutation(
    tmp_path, monkeypatch, capsys
):
    mount, effects, calls = _release_mount(tmp_path, monkeypatch)
    notes = tmp_path / "notes.md"
    notes.write_text("## Highlights\n\n- Curated.\n", encoding="utf-8")

    assert run_mounted_command(mount, ["prepare", "patch"]) == 0
    human_prepare = capsys.readouterr().out
    assert "release-plan schema=1 action=prepare version=0.30.2" in human_prepare
    assert "1. verify-installed-runtime" in human_prepare

    assert run_mounted_command(mount, ["prepare", "patch", "--json"]) == 0
    machine_prepare = json.loads(capsys.readouterr().out)
    assert machine_prepare["action"] == "prepare"
    assert all(
        operation["executor"] == COMMAND_PLAN_EXECUTOR
        for operation in machine_prepare["operations"]
    )

    publish_args = ["publish", "--notes-file", str(notes)]
    assert run_mounted_command(mount, publish_args) == 0
    human_publish = capsys.readouterr().out
    assert "release-plan schema=1 action=publish version=0.30.1" in human_publish
    assert "1. check-release-notes" in human_publish

    assert run_mounted_command(mount, [*publish_args, "--json"]) == 0
    machine_publish = json.loads(capsys.readouterr().out)
    assert machine_publish["action"] == "publish"
    assert machine_publish["plan_digest"]
    assert effects == []
    assert len(calls) == 4


def test_digest_authorized_mounted_release_apply_executes_once(tmp_path, monkeypatch):
    mount, effects, calls = _release_mount(tmp_path, monkeypatch)
    args = release.build_release_parser().parse_args(["prepare", "patch"])
    digest = release.plan_release(args, tmp_path).payload()["plan_digest"]

    assert (
        run_mounted_command(
            mount,
            ["prepare", "patch", f"--apply={digest}"],
        )
        == 0
    )

    assert len(effects) == 1
    assert effects[0][0]["plan_digest"] == digest
    assert effects[0][1] is None
    assert calls == [
        (
            ["release-tool", "prepare", "patch", f"--apply={digest}"],
            "",
            True,
        ),
        (
            ["release-tool", "prepare", "patch", f"--apply={digest}"],
            digest,
            False,
        ),
    ]


def test_mounted_release_apply_requires_an_explicit_digest(tmp_path, monkeypatch):
    mount, effects, calls = _release_mount(tmp_path, monkeypatch)

    with pytest.raises(SpiceError, match="requires --apply="):
        run_mounted_command(mount, ["prepare", "patch", "--apply"])

    assert effects == []
    assert calls == [(["release-tool", "prepare", "patch", "--apply"], "", True)]


def test_stale_mounted_release_digest_refuses_before_execution(
    tmp_path, monkeypatch, capsys
):
    mount, effects, calls = _release_mount(tmp_path, monkeypatch)
    stale = "0" * 64

    assert (
        run_mounted_command(
            mount,
            ["prepare", "patch", f"--apply={stale}"],
        )
        == 2
    )

    assert effects == []
    assert len(calls) == 1
    assert "stale command plan digest" in capsys.readouterr().err


def test_unsupported_spice_owned_release_vocabulary_still_refuses(
    tmp_path, monkeypatch
):
    payload = command_plan_payload(
        command="unsupported",
        operations=[
            {
                "kind": "verify-installed-runtime",
                "target": "release runtime",
                "scope": "repository",
            }
        ],
    )
    monkeypatch.setattr(
        "spice.cli.mounts.require_repository_config_approval",
        lambda *args, **kwargs: None,
    )
    monkeypatch.setattr(
        "spice.cli.mounts.run_parent_lifetime_command",
        lambda argv, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(payload),
            stderr="",
        ),
    )
    mount = MountedCommand(("unsupported",), ("planner",), tmp_path)

    with pytest.raises(SpiceError, match="not applicable by Spice"):
        run_mounted_command(mount, ["--json"])


def test_unknown_command_plan_executor_refuses_during_preview(tmp_path, monkeypatch):
    payload = command_plan_payload(
        command="unsupported-owner",
        operations=[
            {
                "kind": "custom",
                "executor": "mystery",
                "target": "effect",
                "scope": "repository",
            }
        ],
    )
    monkeypatch.setattr(
        "spice.cli.mounts.require_repository_config_approval",
        lambda *args, **kwargs: None,
    )
    monkeypatch.setattr(
        "spice.cli.mounts.run_parent_lifetime_command",
        lambda argv, **kwargs: SimpleNamespace(
            returncode=0,
            stdout=json.dumps(payload),
            stderr="",
        ),
    )
    mount = MountedCommand(("unsupported-owner",), ("planner",), tmp_path)

    with pytest.raises(SpiceError, match="unsupported executor"):
        run_mounted_command(mount, ["--json"])


def test_operation_executor_is_bound_into_the_plan_digest():
    operation = {
        "kind": "custom",
        "target": "effect",
        "scope": "repository",
    }
    spice_owned = command_plan_payload(
        command="owner",
        operations=[operation],
    )
    command_owned = command_plan_payload(
        command="owner",
        operations=[{**operation, "executor": COMMAND_PLAN_EXECUTOR}],
    )

    assert spice_owned["plan_digest"] != command_owned["plan_digest"]


def test_candidate_release_can_bootstrap_through_a_pre_ownership_parent(
    tmp_path, monkeypatch
):
    _mount, _effects, _calls = _release_mount(tmp_path, monkeypatch)
    args = release.build_release_parser().parse_args(["prepare", "patch"])
    payload = release.plan_release(args, tmp_path).payload()
    legacy_environ = {MOUNTED_COMMAND_ENV: "1"}

    assert not defer_command_owned_apply(
        payload,
        apply_requested=True,
        environ=legacy_environ,
        candidate_version=LEGACY_COMMAND_OWNER_CANDIDATE_VERSION,
        legacy_parent_version=LEGACY_COMMAND_OWNER_PARENT_VERSION,
    )


def test_bootstrap_positively_probes_the_installed_parent_version(
    tmp_path, monkeypatch
):
    _mount, _effects, _calls = _release_mount(tmp_path, monkeypatch)
    args = release.build_release_parser().parse_args(["prepare", "patch"])
    payload = release.plan_release(args, tmp_path).payload()
    probes = []

    def probe(command, **kwargs):
        probes.append((command, kwargs))
        return SimpleNamespace(returncode=0, stdout="0.30.1\n", stderr="")

    monkeypatch.setattr(commandownership, "run_tool_command", probe)

    assert not defer_command_owned_apply(
        payload,
        apply_requested=True,
        environ={
            MOUNTED_COMMAND_ENV: "1",
            MOUNTED_RUNTIME_PYTHON_ENV: "/installed/python",
        },
        candidate_version="0.30.2",
    )
    assert probes[0][0][:3] == ["/installed/python", "-I", "-c"]
    assert probes[0][1]["policy"] == "release"


@pytest.mark.parametrize(
    ("parent_version", "candidate_version"),
    (
        ("0.30.0", "0.30.2"),
        ("0.30.1", "0.30.3"),
        ("0.30.2", "0.30.3"),
    ),
)
def test_missing_owner_capability_refuses_outside_the_one_release_bridge(
    tmp_path,
    monkeypatch,
    parent_version,
    candidate_version,
):
    _mount, _effects, _calls = _release_mount(tmp_path, monkeypatch)
    args = release.build_release_parser().parse_args(["prepare", "patch"])
    payload = release.plan_release(args, tmp_path).payload()

    with pytest.raises(SpiceError, match="only supported forward bootstrap"):
        defer_command_owned_apply(
            payload,
            apply_requested=True,
            environ={MOUNTED_COMMAND_ENV: "1"},
            candidate_version=candidate_version,
            legacy_parent_version=parent_version,
        )
