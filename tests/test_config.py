"""Harness configuration: project defaults and worktree overrides."""

import argparse

import pytest

from spice import config
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
from spice.errors import SpiceError
from spice.configcli import handle_config


def test_project_agent_config_provides_launch_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )

    assert config.configured_agent_model(tmp_path) == "gpt-project"
    assert config.configured_agent_effort(tmp_path) == "low"
    assert config.project_agent_config(tmp_path) == {
        "model": "gpt-project",
        "effort": "low",
    }


def test_worktree_agent_config_overrides_project_defaults(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )
    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {
            config.AGENT_MODEL_KEY: "gpt-worktree",
            config.AGENT_EFFORT_KEY: "medium",
        },
    )

    assert config.configured_agent_model(tmp_path) == "gpt-worktree"
    assert config.configured_agent_effort(tmp_path) == "medium"
    assert config.effective_agent_config(tmp_path) == {
        "driver": "codex",
        "model": "gpt-worktree",
        "effort": "medium",
    }


def test_config_overview_shows_project_worktree_and_effective_agent_config(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )
    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {config.AGENT_EFFORT_KEY: "medium"},
    )

    assert config.config_overview(tmp_path) == {
        "schema": config.CONFIG_SCHEMA_VERSION,
        "project": {
            "agent": {
                "model": "gpt-project",
                "effort": "low",
            }
        },
        "worktree": {
            "schema": config.CONFIG_SCHEMA_VERSION,
            "agent": {"effort": "medium"},
        },
        "effective": {
            "agent": {
                "driver": "codex",
                "model": "gpt-project",
                "effort": "medium",
            }
        },
    }


def test_config_agent_reveals_shipped_defaults_without_config(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="worktree",
            clear=False,
            model=None,
            effort=None,
            driver=None,
        )
    )

    assert result == 0
    assert (
        capsys.readouterr().out == "agent project driver=- model=- effort=-\n"
        "agent worktree driver=- model=- effort=-\n"
        "agent effective driver=codex model=gpt-5.5 effort=xhigh\n"
    )


def test_config_agent_writes_project_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="project",
            clear=False,
            model="gpt-project",
            effort="high",
        )
    )

    assert result == 0
    assert config.project_agent_config(tmp_path) == {
        "model": "gpt-project",
        "effort": "high",
    }
    assert (
        capsys.readouterr().out
        == "agent project driver=- model=gpt-project effort=high\n"
        "agent worktree driver=- model=- effort=-\n"
        "agent effective driver=codex model=gpt-project effort=high\n"
    )


def test_config_agent_writes_worktree_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="worktree",
            clear=False,
            model="gpt-worktree",
            effort="low",
        )
    )

    assert result == 0
    assert config.worktree_agent_config(tmp_path) == {
        "model": "gpt-worktree",
        "effort": "low",
    }
    assert (
        capsys.readouterr().out == "agent project driver=- model=- effort=-\n"
        "agent worktree driver=- model=gpt-worktree effort=low\n"
        "agent effective driver=codex model=gpt-worktree effort=low\n"
    )


def test_config_agent_writes_driver_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="worktree",
            clear=False,
            model=None,
            effort=None,
            driver="claude",
        )
    )

    assert result == 0
    assert config.configured_agent_driver(tmp_path) == "claude"
    assert (
        capsys.readouterr().out == "agent project driver=- model=- effort=-\n"
        "agent worktree driver=claude model=- effort=-\n"
        "agent effective driver=claude model=claude-sonnet-5 effort=xhigh\n"
    )


def test_effective_agent_config_keeps_claude_sonnet_family(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {"driver": "claude", "model": "sonnet"},
    )

    assert config.configured_agent_model(tmp_path) == "sonnet"
    assert config.effective_agent_config(tmp_path) == {
        "driver": "claude",
        "model": "sonnet",
        "effort": "xhigh",
    }


def test_effective_agent_config_preserves_explicit_claude_model(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {"driver": "claude", "model": "claude-sonnet-4-6"},
    )

    assert config.effective_agent_config(tmp_path) == {
        "driver": "claude",
        "model": "claude-sonnet-4-6",
        "effort": "xhigh",
    }


def test_config_say_writes_macos_say_settings(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="say",
            clear=False,
            backend=None,
            command=None,
            content_type=None,
            voice="Samantha",
            words_per_minute=190,
        )
    )

    assert result == 0
    assert config.configured_say_backend(tmp_path) == "say"
    assert config.say_command_args(tmp_path) == ["say", "-v", "Samantha", "-r", "190"]
    assert capsys.readouterr().out == ("say backend=say argv=say -v Samantha -r 190\n")


def test_config_say_writes_external_backend(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="say",
            clear=False,
            backend="external",
            command="tts-engine --wav",
            content_type="audio/wav",
            voice=None,
            words_per_minute=None,
        )
    )

    assert result == 0
    assert config.configured_say_backend(tmp_path) == "external"
    assert config.configured_say_command(tmp_path) == "tts-engine --wav"
    assert config.configured_say_content_type(tmp_path) == "audio/wav"
    assert capsys.readouterr().out == (
        "say backend=external command=tts-engine --wav content_type=audio/wav\n"
    )


def test_config_say_rejects_external_backend_without_command(tmp_path, monkeypatch):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    with pytest.raises(SpiceError, match="requires --command"):
        handle_config(
            argparse.Namespace(
                config_action="say",
                clear=False,
                backend="external",
                command=None,
                content_type=None,
                voice=None,
                words_per_minute=None,
            )
        )
