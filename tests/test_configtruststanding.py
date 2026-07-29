"""Shared, capability-scoped repository configuration authority."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.agent import lifecycle
from spice.cli.parser import build_parser
from spice.config import trust
from spice.config.trustpolicy import (
    CommitSignature,
    load_repository_trust_state,
    record_exact_approvals,
    repository_trust_log_path,
)
from spice.configcli import handle_config
from spice.errors import SpiceError
from spice.hooks import doctor
from spice.paths import git_common_dir, shared_state_path
from spice.tasks.git import boundaries
from tests.test_configtrusthelpers import approve_repository_config

TRUSTED_SIGNER = "SHA256:operator-approved-signer"


def test_exact_capability_approval_is_shared_across_linked_worktrees(tmp_path):
    repo = _repository(tmp_path / "primary")
    _write_config(
        repo,
        command="first",
        wrapper="first",
    )
    _commit(repo, "initial executable configuration")
    linked = tmp_path / "linked"
    _git(repo, "worktree", "add", "-q", "-b", "lane", str(linked))

    approve_repository_config(repo)

    _require_probe(linked, "first")
    authority_path = repository_trust_log_path(linked)
    migration_marker = shared_state_path(
        repo,
        trust.SHARED_AUTHORITY_MIGRATION_PATH,
    )
    assert migration_marker.is_file()
    assert authority_path == git_common_dir(repo) / ".spice" / authority_path.name
    assert authority_path != repo / ".spice" / authority_path.name

    _write_config(linked, command="first", wrapper="changed")
    _require_probe(linked, "first")
    with pytest.raises(SpiceError, match="changed since operator approval") as raised:
        _require_wrapper(linked, "changed")

    assert "wrappers" in str(raised.value)
    assert "commands" not in str(raised.value).split("refusing command", 1)[0]


def test_standing_grant_is_previewed_and_stored_only_in_common_git_state(
    tmp_path, monkeypatch, capsys
):
    repo, remote = _remote_repository(tmp_path)
    monkeypatch.chdir(repo)
    parser = build_parser()

    preview_args = parser.parse_args(
        ["config", "trust", "grant", "--signer", TRUSTED_SIGNER]
    )
    assert handle_config(preview_args) == 0
    preview = capsys.readouterr().out
    digest = preview.split("digest=", 1)[1].splitlines()[0]
    authority_path = repository_trust_log_path(repo)
    assert "preview: no changes applied" in preview
    assert not authority_path.exists()

    apply_args = parser.parse_args(
        [
            "config",
            "trust",
            "grant",
            "--signer",
            TRUSTED_SIGNER,
            f"--apply={digest}",
        ]
    )
    assert handle_config(apply_args) == 0

    state = load_repository_trust_state(repo)
    assert state.active_grant is not None
    assert state.active_grant.trusted_signers == (TRUSTED_SIGNER,)
    assert authority_path == git_common_dir(repo) / ".spice" / authority_path.name
    assert not (repo / ".spice" / authority_path.name).exists()
    capsys.readouterr()
    assert handle_config(parser.parse_args(["config", "trust", "show"])) == 0
    shown = json.loads(capsys.readouterr().out)
    assert shown["current"]["approved"] is True
    assert shown["active_grant"]["grant_id"] == state.active_grant.grant_id

    clone = _clone_peer(tmp_path, remote)
    assert load_repository_trust_state(clone).active_grant is None
    assert not repository_trust_log_path(clone).exists()


def test_revocation_invalidates_delegated_anchor_approval_and_records_reason(
    tmp_path,
):
    repo, _remote = _remote_repository(tmp_path)
    grant = trust.plan_standing_repository_trust(
        repo,
        capabilities=("commands",),
        trusted_signers=(TRUSTED_SIGNER,),
    )
    trust.apply_standing_trust_plan(grant)
    _require_probe(repo, "first")

    revoke = trust.plan_standing_repository_revocation(
        repo,
        reason="operator rotated signing authority",
    )
    trust.apply_standing_trust_plan(revoke)

    state = load_repository_trust_state(repo)
    assert state.active_grant is None
    with pytest.raises(SpiceError, match="no operator approval or active standing"):
        _require_probe(repo, "first")
    assert "operator rotated signing authority" in repository_trust_log_path(
        repo
    ).read_text(encoding="utf-8")


def test_revocation_without_a_standing_grant_invalidates_exact_approvals(tmp_path):
    repo = _repository(tmp_path / "repo")
    _write_config(repo, command="first", wrapper="first")
    _commit(repo, "initial executable configuration")
    approve_repository_config(repo)
    _require_probe(repo, "first")

    revoke = trust.plan_standing_repository_revocation(
        repo,
        reason="withdraw exact executable authority",
    )
    assert revoke.grant is None
    trust.apply_standing_trust_plan(revoke)

    with pytest.raises(SpiceError, match="no operator approval or active standing"):
        _require_probe(repo, "first")


def test_revocation_refuses_an_older_exact_approval_plan(tmp_path):
    repo = _repository(tmp_path / "repo")
    _write_config(repo, command="first", wrapper="first")
    _commit(repo, "initial executable configuration")
    approve_repository_config(repo)
    stale_exact = trust.plan_exact_repository_config_approval(repo)
    revoke = trust.plan_standing_repository_revocation(
        repo,
        reason="invalidate every earlier exact plan",
    )
    trust.apply_standing_trust_plan(revoke)

    with pytest.raises(SpiceError, match="authority changed after plan preview"):
        trust.record_planned_repository_config_approval(
            repo,
            stale_exact,
            source="replayed stale init plan",
        )

    with pytest.raises(SpiceError, match="no operator approval or active standing"):
        _require_probe(repo, "first")


def test_shared_migration_marker_prevents_linked_legacy_receipt_resurrection(
    tmp_path,
):
    repo = _repository(tmp_path / "primary")
    _write_config(repo, command="first", wrapper="first")
    _commit(repo, "initial executable configuration")
    linked = tmp_path / "linked"
    _git(repo, "worktree", "add", "-q", "-b", "lane", str(linked))
    approve_repository_config(repo)
    approve_repository_config(linked)
    repository_trust_log_path(repo).unlink()
    shared_state_path(repo, trust.SHARED_AUTHORITY_MIGRATION_PATH).unlink()

    _require_probe(repo, "first")
    revoke = trust.plan_standing_repository_revocation(
        repo,
        reason="revoke migrated legacy authority",
    )
    trust.apply_standing_trust_plan(revoke)

    with pytest.raises(SpiceError, match="no operator approval or active standing"):
        _require_probe(linked, "first")


def test_revocation_refuses_authority_appended_after_its_preview(tmp_path):
    repo = _repository(tmp_path / "repo")
    _write_config(repo, command="first", wrapper="first")
    _commit(repo, "initial executable configuration")
    approve_repository_config(repo)
    revoke = trust.plan_standing_repository_revocation(
        repo,
        reason="stale revocation preview",
    )
    _write_config(repo, command="second", wrapper="first")
    trust.record_planned_repository_config_approval(
        repo,
        trust.plan_exact_repository_config_approval(repo),
        source="concurrent exact operator approval",
    )

    with pytest.raises(SpiceError, match="authority changed after plan preview"):
        trust.apply_standing_trust_plan(revoke)

    assert '"event":"revoke"' not in repository_trust_log_path(repo).read_text(
        encoding="utf-8"
    )


def test_nonprivate_shared_authority_log_is_refused(tmp_path):
    repo = _repository(tmp_path / "repo")
    _write_config(repo, command="first", wrapper="first")
    _commit(repo, "initial executable configuration")
    approve_repository_config(repo)
    path = repository_trust_log_path(repo)
    path.chmod(0o644)

    with pytest.raises(SpiceError, match="not private mode 0600"):
        load_repository_trust_state(repo)


def test_unknown_shared_authority_event_is_a_typed_refusal(tmp_path):
    repo = _repository(tmp_path / "repo")
    path = repository_trust_log_path(repo)
    path.parent.mkdir(parents=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "event": "agent-approved",
                "recorded_at": "2026-07-28T00:00:00Z",
            }
        )
        + "\n",
        encoding="utf-8",
    )
    path.chmod(0o600)

    with pytest.raises(SpiceError, match="trust event is unsupported"):
        load_repository_trust_state(repo)


def test_serve_launch_fast_forward_derives_only_from_trusted_signed_provenance(
    tmp_path, monkeypatch
):
    repo, remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    peer = _clone_peer(tmp_path, remote)
    _write_config(peer, command="trusted", wrapper="first")
    _commit(peer, "trusted config advance")
    _git(peer, "push", "origin", "main")
    _patch_trusted_signatures(monkeypatch)
    _patch_agent_launch(monkeypatch, tmp_path)

    lifecycle.start_agent(
        repo,
        action="start",
        command=["agent", "run"],
        model="test",
        reasoning_effort="medium",
        resume_thread_id="",
        prompt_skill_path=tmp_path / "SKILL.md",
        fast_mode=False,
        supervise_stdout=False,
        launch_claim=None,
    )

    assert _git(repo, "rev-parse", "HEAD") == _git(repo, "rev-parse", "origin/main")
    _require_probe(repo, "trusted")
    state = load_repository_trust_state(repo)
    assert (
        trust.repository_executable_config_digests(repo)["commands"]
        in (state.delegated_approvals["commands"])
    )
    assert '"event":"derive"' in repository_trust_log_path(repo).read_text(
        encoding="utf-8"
    )


def test_cached_delegated_digest_still_refuses_a_later_divergent_head(
    tmp_path, monkeypatch
):
    repo, remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    peer = _clone_peer(tmp_path, remote)
    _write_config(peer, command="trusted", wrapper="first")
    _commit(peer, "trusted config advance")
    _git(peer, "push", "origin", "main")
    boundaries.fast_forward_if_safe(repo)
    _patch_trusted_signatures(monkeypatch)
    _require_probe(repo, "trusted")

    (repo / "README.md").write_text("agent-local commit\n", encoding="utf-8")
    _git(repo, "add", "README.md")
    _git(repo, "commit", "-q", "-m", "agent-local non-config change")

    with pytest.raises(SpiceError, match="agent-local or divergent HEAD"):
        _require_probe(repo, "trusted")


def test_cached_anchor_digest_still_checks_intermediate_commit_signatures(tmp_path):
    repo, remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    peer = _clone_peer(tmp_path, remote)
    _write_config(peer, command="transient-unsigned", wrapper="first")
    _commit(peer, "unsigned executable change")
    unsigned_commit = _git(peer, "rev-parse", "HEAD")
    _write_config(peer, command="first", wrapper="first")
    _commit(peer, "unsigned executable revert")
    _git(peer, "push", "origin", "main")
    boundaries.fast_forward_if_safe(repo)

    with pytest.raises(SpiceError, match="unsigned executable-config commit") as raised:
        _require_probe(repo, "first")

    assert unsigned_commit in str(raised.value)


def test_task_release_publish_never_turns_unsigned_agent_action_into_authority(
    tmp_path,
):
    repo, _remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    anchor_digest = trust.repository_executable_config_digests(repo)["commands"]
    _write_config(repo, command="agent-authored", wrapper="first")
    _commit(repo, "agent-authored executable configuration")
    agent_commit = _git(repo, "rev-parse", "HEAD")

    boundaries.integrate_and_publish("TASK-1kTrustRelease", repo_root=repo)

    with pytest.raises(SpiceError, match="unsigned executable-config commit") as raised:
        _require_probe(repo, "agent-authored")
    message = str(raised.value)
    assert agent_commit in message
    assert "authority is never inferred" not in message
    assert load_repository_trust_state(repo).delegated_approvals[
        "commands"
    ] == frozenset({anchor_digest})


def test_untrusted_signer_after_fast_forward_is_refused_with_commit_identity(
    tmp_path, monkeypatch
):
    repo, remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    peer = _clone_peer(tmp_path, remote)
    _write_config(peer, command="untrusted", wrapper="first")
    _commit(peer, "untrusted config advance")
    untrusted_commit = _git(peer, "rev-parse", "HEAD")
    _git(peer, "push", "origin", "main")
    boundaries.fast_forward_if_safe(repo)
    monkeypatch.setattr(
        trust,
        "commit_signature",
        lambda _repo, _commit: CommitSignature(
            True,
            "SHA256:not-delegated",
            "Untrusted",
            "verified",
        ),
    )

    with pytest.raises(SpiceError, match="untrusted signer") as raised:
        _require_probe(repo, "untrusted")

    assert untrusted_commit in str(raised.value)
    assert "SHA256:not-delegated" in str(raised.value)


def test_changed_repository_remote_cannot_supply_standing_authority(tmp_path):
    repo, _remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    _write_config(repo, command="changed-remote", wrapper="first")
    _git(repo, "remote", "set-url", "origin", str(tmp_path / "other.git"))

    with pytest.raises(SpiceError, match="untrusted repository remote provenance"):
        _require_probe(repo, "changed-remote")


def test_relative_remote_is_too_ambiguous_for_a_cross_worktree_grant(tmp_path):
    repo, _remote = _remote_repository(tmp_path)
    _git(repo, "remote", "set-url", "origin", "../remote.git")

    with pytest.raises(SpiceError, match="relative remote URL"):
        trust.plan_standing_repository_trust(
            repo,
            capabilities=("commands",),
            trusted_signers=(TRUSTED_SIGNER,),
        )


def test_tracked_symlink_config_is_too_ambiguous_for_a_standing_grant(tmp_path):
    repo, _remote = _remote_repository(tmp_path)
    external = tmp_path / "operator-untracked.toml"
    external.write_text(
        '[commands]\nprobe = ["tool", "external"]\n',
        encoding="utf-8",
    )
    (repo / "spice.toml").unlink()
    (repo / "spice.toml").symlink_to(external)
    _commit(repo, "replace executable configuration with a symlink")
    _git(repo, "push", "-q", "origin", "main")

    with pytest.raises(SpiceError, match="tracked, clean spice.toml bytes"):
        trust.plan_standing_repository_trust(
            repo,
            capabilities=("commands",),
            trusted_signers=(TRUSTED_SIGNER,),
        )


def test_doctor_checks_only_the_disabled_builtin_capability(tmp_path):
    repo = _repository(tmp_path / "repo")
    _write_config(repo, command="unapproved-command", wrapper="first")
    path = repo / "spice.toml"
    path.write_text(
        path.read_text(encoding="utf-8")
        + "\n[policy.pre_commit_builtins]\nformatters = false\n",
        encoding="utf-8",
    )
    _commit(repo, "disable one builtin beside other executable config")
    digests = trust.repository_executable_config_digests(repo)
    record_exact_approvals(
        repo,
        {"policy.pre_commit_builtins": digests["policy.pre_commit_builtins"]},
        commit=_git(repo, "rev-parse", "HEAD"),
        source="test exact operator approval",
    )

    check = doctor._pre_commit_builtin_disablement_check(repo)

    assert check.status == "ok"
    assert "repository disablement approved" in check.detail
    with pytest.raises(SpiceError, match="no operator approval"):
        _require_probe(repo, "unapproved-command")


@pytest.mark.parametrize(
    ("change", "expected"),
    [
        ("uncommitted", "provenance-ambiguous uncommitted"),
        ("local-commit", "agent-local or divergent HEAD"),
    ],
)
def test_uncommitted_and_divergent_agent_changes_never_inherit_standing_authority(
    tmp_path,
    change,
    expected,
):
    repo, _remote = _remote_repository(tmp_path)
    _apply_standing_grant(repo)
    _write_config(repo, command=change, wrapper="first")
    if change == "local-commit":
        _commit(repo, "local divergent config")

    with pytest.raises(SpiceError, match=expected):
        _require_probe(repo, change)


def _apply_standing_grant(repo: Path) -> None:
    plan = trust.plan_standing_repository_trust(
        repo,
        capabilities=("commands",),
        trusted_signers=(TRUSTED_SIGNER,),
    )
    trust.apply_standing_trust_plan(plan)


def _patch_trusted_signatures(monkeypatch) -> None:
    monkeypatch.setattr(
        trust,
        "commit_signature",
        lambda _repo, _commit: CommitSignature(
            True,
            TRUSTED_SIGNER,
            "Repository Operator",
            "verified",
        ),
    )


def _patch_agent_launch(monkeypatch, tmp_path: Path) -> None:
    log_path = tmp_path / "agent.log"
    monkeypatch.setattr(lifecycle, "next_agent_log_path", lambda _repo: log_path)
    monkeypatch.setattr(
        lifecycle,
        "spawn_agent",
        lambda *_args, **_kwargs: SimpleNamespace(pid=4321),
    )
    monkeypatch.setattr(
        lifecycle,
        "require_started_process",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "started_agent_thread_id",
        lambda *_args, **_kwargs: "a" * 32,
    )
    monkeypatch.setattr(
        lifecycle,
        "settle_agent_log_path",
        lambda *_args, **_kwargs: log_path,
    )
    monkeypatch.setattr(lifecycle, "write_agent_state", lambda *_args: None)
    monkeypatch.setattr(
        lifecycle,
        "reap_process_when_done",
        lambda *_args, **_kwargs: None,
    )


def _remote_repository(tmp_path: Path) -> tuple[Path, Path]:
    remote = tmp_path / "remote.git"
    _git(tmp_path, "init", "--bare", "-q", "-b", "main", str(remote))
    repo = _repository(tmp_path / "repo")
    _write_config(repo, command="first", wrapper="first")
    _commit(repo, "initial executable configuration")
    _git(repo, "remote", "add", "origin", str(remote))
    _git(repo, "push", "-q", "-u", "origin", "main")
    _git(repo, "remote", "set-head", "origin", "--auto")
    return repo, remote


def _clone_peer(tmp_path: Path, remote: Path) -> Path:
    peer = tmp_path / "peer"
    _git(tmp_path, "clone", "-q", str(remote), str(peer))
    _git(peer, "config", "user.email", "peer@example.com")
    _git(peer, "config", "user.name", "Peer")
    return peer


def _repository(path: Path) -> Path:
    path.mkdir(parents=True)
    _git(path, "init", "-q", "-b", "main")
    _git(path, "config", "user.email", "fixture@example.com")
    _git(path, "config", "user.name", "Fixture")
    return path


def _write_config(repo: Path, *, command: str, wrapper: str) -> None:
    (repo / "spice.toml").write_text(
        "[commands]\n"
        f'probe = ["tool", {json.dumps(command)}]\n\n'
        "[wrappers.common.probe]\n"
        f'argv = ["wrapper-tool", {json.dumps(wrapper)}]\n',
        encoding="utf-8",
    )


def _commit(repo: Path, message: str) -> None:
    _git(repo, "add", "spice.toml")
    _git(repo, "commit", "-q", "-m", message)


def _require_probe(repo: Path, value: str) -> None:
    trust.require_repository_config_approval(
        repo,
        ("commands", "probe"),
        command=f"tool {value}",
    )


def _require_wrapper(repo: Path, value: str) -> None:
    trust.require_repository_config_approval(
        repo,
        ("wrappers", "common", "probe"),
        command=f"wrapper-tool {value}",
    )


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
