"""Activation packet rows that teach first-run harness behavior."""

from spice.agent.activation import activation_command_surface_lines


def test_activation_command_surface_mentions_shell_ack_and_public_tasks():
    text = "\n".join(activation_command_surface_lines())

    assert "command_surface=run shell commands normally" in text
    assert "reexec zsh/bash commands through spice agent run" in text
    assert "ack_inline=ACK pending inbox keys" in text
    assert "task_add_public=spice task add ... --project <stem>" in text


def test_activation_command_surface_explains_pending_count_recovery():
    text = "\n".join(activation_command_surface_lines())

    assert "pending_inbox_recovery=" in text
    assert "spice session only shows pending=N without bodies" in text
    assert "run the next command through spice agent run --" in text
