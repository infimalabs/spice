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
    assert "session=spice session briefing" in text
    assert (
        "task_drain_contract=drive/drain lanes are not done after a task phase boundary"
        in text
    )
    assert "task_steer_contract=steer lanes treat allocator continuation" in text
    assert "manual task claims are exceptional" in text
    assert "ack_inline=ACK pending inbox keys" in text
    assert "task_add_public=spice task add ... --project <stem>" in text


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
    assert "distinguish missing Node dependencies" in text


def test_package_json_makes_node_playwright_available():
    package = json.loads(Path("package.json").read_text(encoding="utf-8"))

    assert package["private"] is True
    assert package["devDependencies"]["playwright"] == "1.61.0"
