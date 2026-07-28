"""Worktree skill materialization contracts."""

import re
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.agent import cli as agent_cli
from spice.agent import lifecycle, lifecyclebinding
from spice.errors import SpiceError
from spice.tasks import claimstate
from tests.test_permissionhelpers import REQUIRES_MODE_BIT_DENIAL

# A UTF-16 byte order mark opens this, so the very first byte is one UTF-8
# rejects outright: the packaged skill is the tracked file an advance rewrites,
# so its bytes are whatever a peer committed rather than guaranteed text.
UNDECODABLE_PACKAGED_SKILL_BYTES = b"\xff\xfepackaged skill\n"

LAST_GOOD_WORKTREE_SKILL = "---\nname: spice\n---\nlast good skill\n"

# The generated worktree copy can be the corrupt file instead: it is untracked
# state nothing validates, so a truncated or partial write leaves bytes here
# that decode no better than a bad packaged source does.
UNDECODABLE_WORKTREE_SKILL_BYTES = b"\xff\xfegenerated skill\n"

# Readable and searchable but not writable, so an existing copy can still be
# read back while every repair write fails.
SEALED_DIRECTORY_MODE = 0o500
READABLE_DIRECTORY_MODE = 0o700


def test_agent_guidance_routes_authority_gate_refusals_to_the_operator():
    skill = lifecycle.packaged_skill_path().read_text(encoding="utf-8")
    doctrine = (Path(__file__).resolve().parents[1] / "AGENTS.md").read_text(
        encoding="utf-8"
    )

    assert "## Operator Decision Boundaries" in skill
    assert "refusals to\nsurface, not speed bumps to log or route around" in skill
    assert "current\n  capability digest may execute" in skill
    assert "command words. Do not run `spice init --apply`" in skill
    assert "`spice config trust grant` / `revoke` plan" in skill
    assert "A fetched ref,\n  successful task publication" in skill
    assert "current ordered plan may mutate\n  operator-authored state" in skill
    assert "Do not add `--apply`" in skill
    assert "unless the operator explicitly decides" in skill
    assert (
        "[spice skill](spice/agent/SKILL.md#operator-decision-boundaries)" in doctrine
    )
    assert "single source of truth" in doctrine
    assert "do not replace either authority decision with a local" in doctrine
    assert "a `spice task oops`, or a route around the gate" in doctrine


def _undecodable_packaged_skill(tmp_path, monkeypatch):
    """Point the packaged-source lookup at bytes this process cannot decode.

    One namespace is patched because only one resolves the default. Every
    entry point that does not name a source of its own reaches
    ``lifecyclebinding``, so this is the whole surface a caller could use to
    escape the condition under test.
    """
    packaged = tmp_path / "packaged-skill.md"
    packaged.write_bytes(UNDECODABLE_PACKAGED_SKILL_BYTES)
    monkeypatch.setattr(lifecyclebinding, "packaged_skill_path", lambda: packaged)
    return packaged


def _activation_packet(repo_root, monkeypatch):
    """Render the real activation packet with only its skill step left live."""
    monkeypatch.setattr(
        agent_cli,
        "_bind_activation_thread",
        lambda _repo: SimpleNamespace(thread_id="actor-a"),
    )
    monkeypatch.setattr(agent_cli, "_install_activation_hooks", lambda _repo: [])
    monkeypatch.setattr(
        agent_cli,
        "_refresh_activation_baseline",
        lambda _repo: SimpleNamespace(notes=["current"]),
    )
    monkeypatch.setattr(
        agent_cli,
        "_renew_activation_claim",
        lambda *, actor=None: claimstate.ClaimRenewalResult(False, "no_active_claim"),
    )
    monkeypatch.setattr(agent_cli, "_activation_steering_token", lambda _repo: "tok")
    return agent_cli.render_activation_packet(repo_root)


def test_packaged_skill_uses_uniform_spice_command_surface():
    text = lifecycle.packaged_skill_path().read_text(encoding="utf-8")
    agent_commands = sorted(
        command for command in set(re.findall(r"`(spice agent [^`]+)`", text))
    )

    assert "using the\n`spice` command directly" in text
    assert "agents should not switch entrypoints" in text
    assert "reexecs itself through `spice agent run`" in text
    assert "RTK rewrite routing before the requested command" in text
    assert "RTK as a command-output optimization" in text
    assert "activation reports `rtk_status` mode `native`" in text
    assert "Only when activation reports mode `active`" in text
    assert "same key=value batch format as task-add batch input" in text
    assert "Repeat `acceptance=...` for multiple criteria" in text
    assert "starts on its own line" in text
    assert "`ACK <key>: captured the request.`" in text
    assert "`TASK title=... | project=<stem.child> [| acceptance=...]`" in text
    assert (
        "Omitting acceptance with no explicit flow starts public tasks in plan" in text
    )
    assert "immediate task capture, not allocator selection" in text
    assert "on its own line with the task-add batch format" in text
    assert "`spice task status` and `spice task doctor` report" in text
    assert "Small findings may be fixed, validated, and committed during review" in text
    assert (
        "only unresolved non-clean findings require `--then` or `--followup` "
        "tracking" in text
    )
    assert "A spice session is a real-time interactive loop" in text
    assert (
        "lead your next working assistant message with a plain-text ACK header "
        "for completed/accepted keys or a reasoned NACK header for refused keys" in text
    )
    assert "`NACK <key>: <why this cannot be done>`" in text
    assert "ACKed or NACKed keys clear from pending" in text
    assert "Do not bury ACKs or NACKs mid-message" in text
    assert agent_commands == [
        "spice agent activation",
        "spice agent run",
        "spice agent run -- <command>",
    ]


def test_available_skill_path_materializes_into_the_worktree(tmp_path):
    located = lifecycle.available_skill_path(tmp_path, required=True)

    expected = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    ignore = tmp_path / lifecycle.WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH
    assert located == expected.resolve()
    assert located.read_text(
        encoding="utf-8"
    ) == lifecycle.packaged_skill_path().read_text(encoding="utf-8")
    assert ignore.read_text(encoding="utf-8").startswith(
        "# Autogenerated by spice; do not edit.\n"
    )


def test_available_skill_path_required_fails_without_worktree_skill(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        lifecyclebinding, "packaged_skill_path", lambda: tmp_path / "missing-package.md"
    )

    with pytest.raises(SpiceError, match="missing spice skill at"):
        lifecycle.available_skill_path(tmp_path, required=True)


def test_materialized_worktree_skill_is_ignored_in_git_repos(tmp_path):
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)

    located = lifecycle.available_skill_path(tmp_path, required=True)

    assert located == (tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH).resolve()
    assert (tmp_path / lifecycle.WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH).read_text(
        encoding="utf-8"
    ) == lifecycle.WORKTREE_SKILL_GITIGNORE_CONTENT
    status = subprocess.run(
        ["git", "-C", str(tmp_path), "status", "--short"],
        check=True,
        capture_output=True,
        text=True,
    )
    assert status.stdout == ""


def test_available_skill_path_leaves_tracked_worktree_skill_clean_when_it_drifts(
    tmp_path,
):
    stale = "---\nname: spice\n---\nrepo-owned skill\n"

    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    subprocess.run(
        ["git", "-C", str(tmp_path), "config", "user.email", "agent@example.test"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(tmp_path), "config", "user.name", "Agent"],
        check=True,
    )
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_text(stale, encoding="utf-8")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "--", target.as_posix()], check=True
    )
    subprocess.run(
        ["git", "-C", str(tmp_path), "commit", "-q", "-m", "track worktree skill"],
        check=True,
    )

    located = lifecycle.available_skill_path(tmp_path, required=True)

    assert located == target.resolve()
    assert target.read_text(encoding="utf-8") == stale
    status = subprocess.run(
        ["git", "-C", str(tmp_path), "status", "--short"],
        check=True,
        capture_output=True,
        text=True,
    )
    assert status.stdout == ""


def test_materialize_worktree_skill_refreshes_stale_copies(tmp_path):
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_text("stale content from an older install\n", encoding="utf-8")

    located = lifecycle.materialize_worktree_skill(tmp_path)

    assert located == target.resolve()
    assert target.read_text(
        encoding="utf-8"
    ) == lifecycle.packaged_skill_path().read_text(encoding="utf-8")


def test_materialize_worktree_skill_leaves_current_copy_untouched(
    tmp_path, monkeypatch
):
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_text(
        lifecycle.packaged_skill_path().read_text(encoding="utf-8"), encoding="utf-8"
    )
    original_write_text = Path.write_text

    def fail_if_target_is_rewritten(self, *args, **kwargs):
        if self == target:
            raise AssertionError("materialize_worktree_skill rewrote current content")
        return original_write_text(self, *args, **kwargs)

    monkeypatch.setattr(Path, "write_text", fail_if_target_is_rewritten)

    located = lifecycle.materialize_worktree_skill(tmp_path)

    assert located == target.resolve()


def test_activation_renders_when_packaged_skill_bytes_are_undecodable(
    tmp_path, monkeypatch
):
    _undecodable_packaged_skill(tmp_path, monkeypatch)

    packet = _activation_packet(tmp_path, monkeypatch)

    assert packet.startswith("spice_agent_activation\n")
    assert f"worktree={tmp_path.resolve()}" in packet


def test_activation_serves_the_worktree_copy_when_packaged_bytes_are_undecodable(
    tmp_path, monkeypatch
):
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_text(LAST_GOOD_WORKTREE_SKILL, encoding="utf-8")
    _undecodable_packaged_skill(tmp_path, monkeypatch)

    packet = _activation_packet(tmp_path, monkeypatch)

    assert f"skill={target.resolve()}" in packet
    assert target.read_text(encoding="utf-8") == LAST_GOOD_WORKTREE_SKILL


def test_launch_repairs_an_undecodable_untracked_worktree_skill_and_converges(
    tmp_path,
) -> None:
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_bytes(UNDECODABLE_WORKTREE_SKILL_BYTES)
    packaged_bytes = lifecycle.packaged_skill_path().read_bytes()

    first = lifecycle.resolve_agent_prompt_skill_path(tmp_path)
    second = lifecycle.resolve_agent_prompt_skill_path(tmp_path)

    assert first == target.resolve()
    assert second == first
    assert target.read_bytes() == packaged_bytes


@pytest.mark.parametrize("unrepairable", ["packaged-source", "tracked-copy"])
def test_launch_reports_a_missing_skill_when_undecodable_bytes_cannot_be_repaired(
    tmp_path,
    monkeypatch,
    unrepairable,
) -> None:
    if unrepairable == "packaged-source":
        preserved = _undecodable_packaged_skill(tmp_path, monkeypatch)
        expected = UNDECODABLE_PACKAGED_SKILL_BYTES
    else:
        subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
        preserved = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
        preserved.parent.mkdir(parents=True)
        preserved.write_bytes(UNDECODABLE_WORKTREE_SKILL_BYTES)
        subprocess.run(
            [
                "git",
                "-C",
                str(tmp_path),
                "add",
                "--",
                lifecycle.WORKTREE_SKILL_RELATIVE_PATH.as_posix(),
            ],
            check=True,
        )
        expected = UNDECODABLE_WORKTREE_SKILL_BYTES

    with pytest.raises(SpiceError, match="missing spice skill at"):
        lifecycle.resolve_agent_prompt_skill_path(tmp_path)

    assert preserved.read_bytes() == expected


@REQUIRES_MODE_BIT_DENIAL
def test_unwritable_tree_reports_a_missing_skill_rather_than_the_corrupt_copy(
    tmp_path,
) -> None:
    """A repair that cannot be written must not answer with what it failed to fix.

    The packaged source decodes here, so this copy is repairable in principle
    and only the write stands in the way. The directory is sealed and the
    ignore file is written first, which leaves the repair as the single write
    that can fail, so the missing-skill answer can only come from the corrupt
    copy being judged unservable.
    """
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_bytes(UNDECODABLE_WORKTREE_SKILL_BYTES)
    ignore = tmp_path / lifecycle.WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH
    ignore.write_text(lifecycle.WORKTREE_SKILL_GITIGNORE_CONTENT, encoding="utf-8")

    target.parent.chmod(SEALED_DIRECTORY_MODE)
    try:
        assert lifecyclebinding.available_skill_path(tmp_path, required=False) is None
        with pytest.raises(SpiceError, match="missing spice skill at"):
            lifecycle.resolve_agent_prompt_skill_path(tmp_path)
    finally:
        target.parent.chmod(READABLE_DIRECTORY_MODE)

    assert target.read_bytes() == UNDECODABLE_WORKTREE_SKILL_BYTES


@REQUIRES_MODE_BIT_DENIAL
def test_unwritable_tree_still_serves_a_stale_copy_that_reads_back_as_text(
    tmp_path,
) -> None:
    """Only the undecodable copy loses service when the repair cannot be written.

    A stale copy is the same kind of drift, failing the same write, and it
    stays in service because a caller can still read it. Sealing the directory
    around a readable copy is what separates that from the corrupt case.
    """
    target = tmp_path / lifecycle.WORKTREE_SKILL_RELATIVE_PATH
    target.parent.mkdir(parents=True)
    target.write_text(LAST_GOOD_WORKTREE_SKILL, encoding="utf-8")
    ignore = tmp_path / lifecycle.WORKTREE_SKILL_GITIGNORE_RELATIVE_PATH
    ignore.write_text(lifecycle.WORKTREE_SKILL_GITIGNORE_CONTENT, encoding="utf-8")

    target.parent.chmod(SEALED_DIRECTORY_MODE)
    try:
        located = lifecycle.resolve_agent_prompt_skill_path(tmp_path)
    finally:
        target.parent.chmod(READABLE_DIRECTORY_MODE)

    packaged = lifecycle.packaged_skill_path().read_text(encoding="utf-8")
    assert located == target.resolve()
    assert target.read_text(encoding="utf-8") == LAST_GOOD_WORKTREE_SKILL
    # The served copy is genuinely the stale one, not a quietly repaired file.
    assert located.read_text(encoding="utf-8") != packaged
