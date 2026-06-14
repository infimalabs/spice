"""Harness configuration: project defaults and worktree overrides."""

import argparse

from spice import config
from spice.configcli import handle_config


def test_project_agent_config_provides_launch_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\nthinking = "low"\n',
        encoding="utf-8",
    )

    assert config.configured_agent_model(tmp_path) == "gpt-project"
    assert config.configured_agent_thinking(tmp_path) == "low"
    assert config.project_agent_config(tmp_path) == {
        "model": "gpt-project",
        "thinking": "low",
    }


def test_worktree_agent_config_overrides_project_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\nthinking = "low"\n',
        encoding="utf-8",
    )
    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {
            config.AGENT_MODEL_KEY: "gpt-worktree",
            config.AGENT_THINKING_KEY: "medium",
        },
    )

    assert config.configured_agent_model(tmp_path) == "gpt-worktree"
    assert config.configured_agent_thinking(tmp_path) == "medium"
    assert config.effective_agent_config(tmp_path) == {
        "model": "gpt-worktree",
        "thinking": "medium",
    }


def test_config_overview_shows_project_worktree_and_effective_agent_config(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\nthinking = "low"\n',
        encoding="utf-8",
    )
    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {config.AGENT_THINKING_KEY: "medium"},
    )

    assert config.config_overview(tmp_path) == {
        "schema": config.CONFIG_SCHEMA_VERSION,
        "project": {
            "agent": {
                "model": "gpt-project",
                "thinking": "low",
            }
        },
        "worktree": {
            "schema": config.CONFIG_SCHEMA_VERSION,
            "agent": {"thinking": "medium"},
        },
        "effective": {
            "agent": {
                "model": "gpt-project",
                "thinking": "medium",
            }
        },
    }


def test_config_agent_writes_project_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="project",
            clear=False,
            model="gpt-project",
            thinking="high",
        )
    )

    assert result == 0
    assert config.project_agent_config(tmp_path) == {
        "model": "gpt-project",
        "thinking": "high",
    }
    assert (
        capsys.readouterr().out
        == "agent project driver=- model=gpt-project thinking=high\n"
        "agent worktree driver=- model=- thinking=-\n"
        "agent effective driver=- model=gpt-project thinking=high\n"
    )


def test_config_agent_writes_worktree_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="worktree",
            clear=False,
            model="gpt-worktree",
            thinking="low",
        )
    )

    assert result == 0
    assert config.worktree_agent_config(tmp_path) == {
        "model": "gpt-worktree",
        "thinking": "low",
    }
    assert (
        capsys.readouterr().out == "agent project driver=- model=- thinking=-\n"
        "agent worktree driver=- model=gpt-worktree thinking=low\n"
        "agent effective driver=- model=gpt-worktree thinking=low\n"
    )


def test_config_agent_writes_driver_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="worktree",
            clear=False,
            model=None,
            thinking=None,
            driver="claude",
        )
    )

    assert result == 0
    assert config.configured_agent_driver(tmp_path) == "claude"
    assert "agent worktree driver=claude" in capsys.readouterr().out
