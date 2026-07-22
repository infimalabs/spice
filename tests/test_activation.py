"""Activation packet rows that teach first-run harness behavior."""

import json
import re
import shutil
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

from spice.agent import cli as agent_cli
from spice.agent.activation import (
    activation_browser_validation_lines,
    activation_command_surface_lines,
)
from spice.agent.driver import DRIVER
from spice.agent.rtkhealth import RtkHealth
from spice.tasks import claimstate, config, create, identity

ACTOR = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


@pytest.fixture(autouse=True)
def _active_rtk(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        "spice.agent.rtkhealth.probe_rtk_health",
        lambda _repo: RtkHealth(
            "rtk", "active", "rewrite protocol valid (exit 3)", "0.42.4"
        ),
    )


@pytest.mark.parametrize(
    "health",
    [
        RtkHealth("rtk", "active", "rewrite protocol valid (exit 3)", "0.42.4"),
        RtkHealth("missing-rtk", "missing", "launch failed"),
        RtkHealth("old-rtk", "obsolete", "RTK 0.41.0 is obsolete", "0.41.0"),
        RtkHealth("invalid-rtk", "protocol-invalid", "rewrite probe invalid"),
    ],
)
def test_activation_reports_rtk_health_and_completes_every_setup_step(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    health: RtkHealth,
) -> None:
    events: list[str] = []

    def probe(_repo: Path) -> RtkHealth:
        events.append("rtk-probe")
        return health

    monkeypatch.setattr("spice.agent.rtkhealth.probe_rtk_health", probe)
    monkeypatch.setattr(
        "spice.agent.lifecycle.bind_ambient_agent_thread",
        lambda _repo: events.append("bind") or SimpleNamespace(thread_id="actor-a"),
    )
    monkeypatch.setattr(
        "spice.hooks.install.install_hooks_for_repo",
        lambda _repo: events.append("hooks") or ["hook-row"],
    )
    monkeypatch.setattr(
        "spice.agent.lifecycle.materialize_worktree_skill",
        lambda _repo: events.append("skill") or tmp_path / ".agents/skills/spice.md",
    )
    monkeypatch.setattr(
        "spice.tasks.gitsync.fast_forward_if_safe",
        lambda _repo: events.append("baseline") or SimpleNamespace(notes=["current"]),
    )
    monkeypatch.setattr(
        "spice.tasks.claimstate.renew_claim",
        lambda *, actor=None: (
            events.append(f"renew:{actor}")
            or claimstate.ClaimRenewalResult(False, "no_active_claim")
        ),
    )
    monkeypatch.setattr("spice.mail.steeringkey.steering_token", lambda _repo: "tok")

    packet = agent_cli.render_activation_packet(tmp_path)
    status_line = next(
        line for line in packet.splitlines() if line.startswith("rtk_status=")
    )
    payload = json.loads(status_line.removeprefix("rtk_status="))

    assert events == [
        "rtk-probe",
        "bind",
        "hooks",
        "skill",
        "baseline",
        "renew:actor-a",
    ]
    assert payload == {
        "detail": health.detail,
        "executable": health.executable,
        "mode": health.mode,
        "state": health.state,
        "version": health.version or None,
    }
    assert "dev_hooks_detail=hook-row" in packet
    assert "claim_renewal=skipped no_active_claim" in packet
    assert "baseline_refresh=current" in packet


def test_activation_command_surface_mentions_shell_ack_and_public_tasks():
    text = "\n".join(activation_command_surface_lines(rtk_active=True))

    assert "command_surface=run shell commands normally" in text
    assert "reexec the first zsh/bash command shell through spice agent run" in text
    assert "descendant shells use static hooks and precomputed wrappers" in text
    assert "agent-run child shells enter the static hook stage" in text
    assert "snapshot/descendant state is captured" in text
    assert "rtk_contract=RTK is an optional command-output optimization" in text
    assert "preserves native command execution" in text
    assert "rtk_guidance=RTK rewrite support is active" in text
    assert "session=spice session briefing" in text
    assert "task_next=spice task next --wait" in text
    assert (
        "task_drain_contract=drive/drain lanes are not done after a task phase boundary"
        in text
    )
    assert "wakes allocation through the task event" in text
    assert "task_steer_contract=steer lanes treat allocator continuation" in text
    assert "manual task claims are exceptional" in text
    assert "task_capture_contract=operator requests to create or capture tasks" in text
    assert "TASK directive that starts on its own line" in text
    assert "ACK <key>: captured the request." in text
    assert "TASK title=... | project=<stem.child> [| acceptance=...]" in text
    assert "omitted acceptance with no flow starts in plan" in text
    assert "same key=value batch format as task add" in text
    assert "repeat acceptance=... for multiple criteria" in text
    assert "immediate task capture is not allocator selection" in text
    assert "ack_inline=spice is a real-time interactive loop" in text
    assert "lead each working assistant message with ACK <key>" in text
    assert "reasoned NACK <key>: <why this cannot be done>" in text
    assert "acknowledged/refused keys clear from pending" in text
    assert "do not bury ACKs or NACKs mid-message" in text
    assert "task_add_public=TASK title=... | project=<stem.child>" in text
    assert "omitted acceptance with no flow creates a plan-phase task" in text
    assert "must start on its own line" in text
    assert "same task-add batch format" in text
    assert "repeat acceptance=... for multiple criteria" in text
    assert "task_project_depth=public task project depth bounds" in text


def test_activation_command_surface_explains_pending_count_recovery():
    text = "\n".join(activation_command_surface_lines(rtk_active=False))

    assert "pending_inbox_recovery=" in text
    assert "spice session briefing only shows pending=N without bodies" in text
    assert "run the next command through spice agent run --" in text


def test_activation_command_surface_ordinary_agent_command_allowlist():
    text = "\n".join(activation_command_surface_lines(rtk_active=True))
    agent_commands = sorted(set(re.findall(r"\b(spice agent [a-z][a-z0-9-]*)", text)))

    assert agent_commands == ["spice agent run"]


def test_activation_gives_discrete_read_guidance_only_for_active_rtk():
    active = "\n".join(activation_command_surface_lines(rtk_active=True))
    native = "\n".join(activation_command_surface_lines(rtk_active=False))

    assert {
        "active_guidance": "run read-heavy commands" in active,
        "native_contract": "preserves native command execution" in native,
        "native_guidance_rows": [
            line for line in native.splitlines() if line.startswith("rtk_guidance=")
        ],
    } == {
        "active_guidance": True,
        "native_contract": True,
        "native_guidance_rows": [],
    }


def test_activation_browser_validation_uses_repo_local_node_playwright():
    text = "\n".join(activation_browser_validation_lines())

    assert "use the repo-local Node Playwright package" in text
    assert "run npm install when node_modules is absent" in text
    assert "npm exec" in text
    assert "Node require('playwright')" in text
    assert "repo-local serve Playwright harness" in text
    assert ".spice/agents/playwright-mcp.json browser.contextOptions" in text
    assert "matches the operator's system appearance" in text
    assert "distinguish missing Node dependencies" in text


def test_activation_packet_reports_claim_renewal(tmp_path, monkeypatch):
    seen: dict[str, str | None] = {}

    monkeypatch.setattr(
        "spice.agent.lifecycle.bind_ambient_agent_thread",
        lambda _repo: SimpleNamespace(thread_id="actor-a"),
    )
    monkeypatch.setattr("spice.hooks.install.install_hooks_for_repo", lambda _repo: [])
    monkeypatch.setattr(
        "spice.agent.lifecycle.materialize_worktree_skill", lambda _repo: None
    )
    monkeypatch.setattr(
        "spice.tasks.gitsync.fast_forward_if_safe",
        lambda _repo: SimpleNamespace(notes=["current"]),
    )
    monkeypatch.setattr("spice.mail.steeringkey.steering_token", lambda _repo: "tok")

    def fake_renew_claim(*, actor=None):
        seen["actor"] = actor
        return claimstate.ClaimRenewalResult(
            True,
            "renewed",
            handle="TASK-1k4Q5gJw",
            claim_until="2026-07-09T06:00:00.000000Z",
        )

    monkeypatch.setattr("spice.tasks.claimstate.renew_claim", fake_renew_claim)

    packet = agent_cli.render_activation_packet(tmp_path)

    assert seen == {"actor": "actor-a"}
    assert (
        "claim_renewal=renewed TASK-1k4Q5gJw until 2026-07-09T06:00:00.000000Z"
    ) in packet
    assert "baseline_refresh=current" in packet


def test_activation_packet_renews_claim_after_baseline_refresh(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _repo_with_upstream(tmp_path)
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR)
    config.set_backend(str(backend))
    try:
        handle = create.add(
            "Activation claim follows fast-forward",
            project="task.unit",
            origin="ack:1jN54zJJ",
            acceptance=["claim metadata reflects post-refresh HEAD"],
            claim=True,
        )
        old_head = _git(repo, "rev-parse", "HEAD")
        _advance_upstream(tmp_path)

        monkeypatch.setattr(
            "spice.agent.lifecycle.bind_ambient_agent_thread",
            lambda _repo: SimpleNamespace(thread_id=ACTOR),
        )
        monkeypatch.setattr(
            "spice.hooks.install.install_hooks_for_repo", lambda _repo: []
        )
        monkeypatch.setattr(
            "spice.agent.lifecycle.materialize_worktree_skill", lambda _repo: None
        )
        monkeypatch.setattr(
            "spice.mail.steeringkey.steering_token", lambda _repo: "tok"
        )

        packet = agent_cli.render_activation_packet(repo)
        row = identity.resolve(handle)
        refreshed_head = _git(repo, "rev-parse", "HEAD")

        assert old_head != refreshed_head
        assert "baseline_refresh=updated working tree to the current baseline" in packet
        assert f"claim_renewal=renewed {handle} until " in packet
        assert row["claim_head"] == refreshed_head
    finally:
        config.set_backend(None)


def test_activation_packet_reports_failed_claim_renewal(tmp_path, monkeypatch):
    monkeypatch.setattr(
        "spice.agent.lifecycle.bind_ambient_agent_thread",
        lambda _repo: SimpleNamespace(thread_id="actor-a"),
    )
    monkeypatch.setattr("spice.hooks.install.install_hooks_for_repo", lambda _repo: [])
    monkeypatch.setattr(
        "spice.agent.lifecycle.materialize_worktree_skill", lambda _repo: None
    )
    monkeypatch.setattr(
        "spice.tasks.gitsync.fast_forward_if_safe",
        lambda _repo: SimpleNamespace(notes=["current"]),
    )
    monkeypatch.setattr("spice.mail.steeringkey.steering_token", lambda _repo: "tok")
    monkeypatch.setattr(
        "spice.tasks.claimstate.renew_claim",
        lambda *, actor=None: claimstate.ClaimRenewalResult(
            False, "backend_error", detail="backend offline"
        ),
    )

    packet = agent_cli.render_activation_packet(tmp_path)

    assert "claim_renewal=failed backend_error detail=backend offline" in packet
    assert "baseline_refresh=current" in packet


def test_package_json_makes_node_playwright_available():
    package = json.loads(Path("package.json").read_text(encoding="utf-8"))

    assert package["private"] is True
    assert package["devDependencies"]["playwright"] == "1.61.0"


def _repo_with_upstream(tmp_path: Path) -> Path:
    remote = tmp_path / "remote.git"
    repo = tmp_path / "repo"
    _run(tmp_path, "git", "init", "--bare", "-b", "main", str(remote))
    _run(tmp_path, "git", "clone", str(remote), str(repo))
    _run(repo, "git", "config", "user.email", "spice@example.test")
    _run(repo, "git", "config", "user.name", "Spice Tests")
    repo.joinpath("README.md").write_text("initial\n", encoding="utf-8")
    _run(repo, "git", "add", "README.md")
    _run(repo, "git", "commit", "-m", "initial")
    _run(repo, "git", "push", "-u", "origin", "main")
    _run(repo, "git", "remote", "set-head", "origin", "--auto")
    return repo


def _advance_upstream(tmp_path: Path) -> None:
    remote = tmp_path / "remote.git"
    peer = tmp_path / "peer"
    _run(tmp_path, "git", "clone", str(remote), str(peer))
    _run(peer, "git", "config", "user.email", "spice@example.test")
    _run(peer, "git", "config", "user.name", "Spice Tests")
    peer.joinpath("README.md").write_text("advanced\n", encoding="utf-8")
    _run(peer, "git", "add", "README.md")
    _run(peer, "git", "commit", "-m", "advance upstream")
    _run(peer, "git", "push", "origin", "main")


def _git(cwd: Path, *args: str) -> str:
    return _run(cwd, "git", *args).stdout.strip()


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)
