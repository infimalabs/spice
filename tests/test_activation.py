"""Activation packet rows that teach first-run harness behavior."""

import json
from pathlib import Path

from spice.agent.activation import (
    activation_browser_validation_lines,
    activation_command_surface_lines,
)


def test_activation_command_surface_mentions_shell_ack_and_public_tasks():
    text = "\n".join(activation_command_surface_lines())

    assert "command_surface=run shell commands normally" in text
    assert "reexec the first zsh/bash command shell through spice agent run" in text
    assert "descendant shells use static hooks and precomputed wrappers" in text
    assert "agent launch clears inherited reexec markers" in text
    assert "SPICE_SHELL_HOOK_REEXEC_STAGE=1 is expected inside" in text
    assert "rtk_rewrite_contract=" in text
    assert "complete top-level shell command string to spice agent run" in text
    assert "agent run is the RTK rewrite owner" in text
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
    assert "TASK title=... | project=<stem.child> | acceptance=..." in text
    assert "same key=value batch format as task add" in text
    assert "immediate task capture is not allocator selection" in text
    assert "ack_inline=spice is a real-time interactive loop" in text
    assert "lead each working assistant message with ACK <key>" in text
    assert "acknowledged keys clear from pending" in text
    assert "do not bury ACKs mid-message or defer them to final response" in text
    assert "task_add_public=TASK title=... | project=<stem.child>" in text
    assert "must start on its own line" in text
    assert "same task-add batch format" in text
    assert "task_project_depth=public task project depth bounds" in text


def test_activation_command_surface_explains_pending_count_recovery():
    text = "\n".join(activation_command_surface_lines())

    assert "pending_inbox_recovery=" in text
    assert "spice session briefing only shows pending=N without bodies" in text
    assert "run the next command through spice agent run --" in text


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


def test_package_json_makes_node_playwright_available():
    package = json.loads(Path("package.json").read_text(encoding="utf-8"))

    assert package["private"] is True
    assert package["devDependencies"]["playwright"] == "1.61.0"
