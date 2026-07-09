"""Activation packet rows that teach first-run harness behavior."""

import json
import re
from pathlib import Path
from types import SimpleNamespace

from spice.agent import cli as agent_cli
from spice.agent.activation import (
    activation_browser_validation_lines,
    activation_command_surface_lines,
)
from spice.tasks import ops


def test_activation_command_surface_mentions_shell_ack_and_public_tasks():
    text = "\n".join(activation_command_surface_lines())

    assert "command_surface=run shell commands normally" in text
    assert "reexec the first zsh/bash command shell through spice agent run" in text
    assert "descendant shells use static hooks and precomputed wrappers" in text
    assert "agent-run child shells enter the static hook stage" in text
    assert "snapshot/descendant state is captured" in text
    assert "session=spice session briefing" in text
    assert (
        "task_drain_contract=drive/drain lanes are not done after a task phase boundary"
        in text
    )
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
    text = "\n".join(activation_command_surface_lines())

    assert "pending_inbox_recovery=" in text
    assert "spice session briefing only shows pending=N without bodies" in text
    assert "run the next command through spice agent run --" in text


def test_activation_command_surface_ordinary_agent_command_allowlist():
    text = "\n".join(activation_command_surface_lines())
    agent_commands = sorted(set(re.findall(r"\b(spice agent [a-z][a-z0-9-]*)", text)))

    assert agent_commands == ["spice agent run"]


def test_activation_browser_validation_uses_repo_local_node_playwright():
    text = "\n".join(activation_browser_validation_lines())

    assert "use the repo-local Node Playwright package" in text
    assert "run npm install when node_modules is absent" in text
    assert "npm exec" in text
    assert "Node require('playwright')" in text
    assert "repo-local serve Playwright harness" in text
    assert ".spice/agent/playwright-mcp.json browser.contextOptions" in text
    assert "matches the operator's system appearance" in text
    assert "distinguish missing Node dependencies" in text


def test_activation_packet_reports_claim_renewal(tmp_path, monkeypatch):
    seen: dict[str, str | None] = {}

    monkeypatch.setattr(
        "spice.agent.lifecycle.bind_ambient_agent_activation",
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
        return ops.ClaimRenewalResult(
            True,
            "renewed",
            handle="TASK-1k4Q5gJw",
            claim_until="2026-07-09T06:00:00.000000Z",
        )

    monkeypatch.setattr("spice.tasks.ops.renew_claim", fake_renew_claim)

    packet = agent_cli.render_activation_packet(tmp_path)

    assert seen == {"actor": "actor-a"}
    assert (
        "claim_renewal=renewed TASK-1k4Q5gJw until 2026-07-09T06:00:00.000000Z"
    ) in packet
    assert "baseline_refresh=current" in packet


def test_activation_packet_reports_failed_claim_renewal(tmp_path, monkeypatch):
    monkeypatch.setattr(
        "spice.agent.lifecycle.bind_ambient_agent_activation",
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
        "spice.tasks.ops.renew_claim",
        lambda *, actor=None: ops.ClaimRenewalResult(
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
