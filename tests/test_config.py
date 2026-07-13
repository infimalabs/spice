"""Harness configuration: project defaults and worktree overrides."""

import argparse
import json
import tomllib
from collections.abc import Callable
from dataclasses import dataclass

import pytest

from spice import config
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.configcli import handle_config

SAY_TIMEOUT_MINUTE_FLOOR_SECONDS = 60.0
SAY_TIMEOUT_OVERRIDE_SECONDS = 12.5


@dataclass(frozen=True)
class ConfigMutationOutcome:
    state: str
    message: str


def _config_mutation_outcome(operation: Callable[[], object]) -> ConfigMutationOutcome:
    try:
        operation()
    except SpiceError as exc:
        return ConfigMutationOutcome("rejected", str(exc))
    return ConfigMutationOutcome("applied", "configuration applied")


def _redirect_system_config(tmp_path, monkeypatch):
    system_root = tmp_path / "installed-spice"
    system_root.mkdir()
    system_path = system_root / "spice.toml"
    system_path.write_text(
        "[say]\nwords_per_minute = 100\n\n"
        '[agent]\nmodel = "system-model"\neffort = "system-effort"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr("spice.config.runtime_spice_source", lambda: system_root)
    monkeypatch.setattr(
        "spice.configlayer.paths.runtime_spice_source", lambda: system_root
    )
    return system_path


def test_pyproject_agent_layer_provides_launch_defaults(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )

    assert config.configured_agent_model(tmp_path) == "gpt-project"
    assert config.configured_agent_effort(tmp_path) == "low"
    assert config.layer_table(tmp_path, config.PYPROJECT_SOURCE, "agent") == {
        "model": "gpt-project",
        "effort": "low",
    }


def test_worktree_agent_layer_overrides_pyproject_defaults(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
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


def test_config_overview_shows_layers_effective_values_and_provenance(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.AGENT_KEY,
        {config.AGENT_EFFORT_KEY: "medium"},
    )

    overview = config.config_overview(tmp_path)

    assert tuple(overview["layers"]) == config.CONFIG_SCOPE_NAMES
    assert overview["layers"]["pyproject"]["path"] == str(tmp_path / "pyproject.toml")
    assert overview["layers"]["worktree"]["values"] == {"agent": {"effort": "medium"}}
    assert overview["effective"]["agent"]["model"] == "gpt-project"
    assert overview["effective"]["agent"]["effort"] == "medium"
    assert overview["provenance"]["agent.model"] == {
        "scope": "pyproject",
        "path": str(tmp_path / "pyproject.toml"),
    }
    assert overview["provenance"]["agent.effort"] == {
        "scope": "worktree",
        "path": str(config.worktree_config_path(tmp_path)),
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
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
        "agent pyproject driver=- model=- effort=-\n"
        "agent repository driver=- model=- effort=-\n"
        "agent worktree driver=- model=- effort=-\n"
        "agent effective driver=codex model=gpt-5.5 effort=xhigh\n"
    )


def test_config_system_help_and_parser_contract():
    parser = build_parser()
    args = parser.parse_args(["config", "system"])
    subparsers = next(
        action
        for action in parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )
    config_help = subparsers.choices["config"].format_help()

    assert args.config_action == "system"
    assert args.func == handle_config
    assert "system" in config_help
    assert "Print the effective agent configuration." in config_help


def test_config_system_renders_effective_agent_config_read_only(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )

    result = handle_config(build_parser().parse_args(["config", "system"]))

    assert result == 0
    rendered = json.loads(capsys.readouterr().out)
    assert rendered["effective"] == {
        "driver": "codex",
        "model": "gpt-project",
        "effort": "low",
    }
    assert rendered["provenance"]["agent.model"]["scope"] == "pyproject"
    assert sorted(path.name for path in tmp_path.iterdir()) == ["pyproject.toml"]


def test_config_agent_writes_project_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="pyproject",
            clear=False,
            model="gpt-project",
            effort="high",
        )
    )

    assert result == 0
    assert config.layer_table(tmp_path, config.PYPROJECT_SOURCE, "agent") == {
        "model": "gpt-project",
        "effort": "high",
    }
    assert (
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
        "agent pyproject driver=- model=gpt-project effort=high\n"
        "agent repository driver=- model=- effort=-\n"
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
    assert config.layer_table(tmp_path, config.WORKTREE_SOURCE, "agent") == {
        "model": "gpt-worktree",
        "effort": "low",
    }
    assert (
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
        "agent pyproject driver=- model=- effort=-\n"
        "agent repository driver=- model=- effort=-\n"
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
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
        "agent pyproject driver=- model=- effort=-\n"
        "agent repository driver=- model=- effort=-\n"
        "agent worktree driver=claude model=- effort=-\n"
        "agent effective driver=claude model=claude-opus-4-8 effort=xhigh\n"
    )


def test_four_scope_precedence_clears_to_reveal_each_earlier_layer(
    tmp_path, monkeypatch, capsys
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    parser = build_parser()
    values = (
        ("system", 110, "system-agent", "low"),
        ("pyproject", 120, "pyproject-agent", "medium"),
        ("repository", 130, "repository-agent", "high"),
        ("worktree", 140, "worktree-agent", "xhigh"),
    )
    for scope, rate, model, effort in values:
        handle_config(
            parser.parse_args(
                [
                    "config",
                    "say",
                    "--scope",
                    scope,
                    "--words-per-minute",
                    str(rate),
                ]
            )
        )
        handle_config(
            parser.parse_args(
                [
                    "config",
                    "agent",
                    "--scope",
                    scope,
                    "--model",
                    model,
                    "--effort",
                    effort,
                ]
            )
        )

    observed = []
    for scope in ("worktree", "repository", "pyproject"):
        observed.append(
            (
                config.configured_say_words_per_minute(tmp_path),
                config.configured_agent_model(tmp_path),
                config.configured_agent_effort(tmp_path),
            )
        )
        handle_config(parser.parse_args(["config", "say", "--scope", scope, "--clear"]))
        handle_config(
            parser.parse_args(["config", "agent", "--scope", scope, "--clear"])
        )
    observed.append(
        (
            config.configured_say_words_per_minute(tmp_path),
            config.configured_agent_model(tmp_path),
            config.configured_agent_effort(tmp_path),
        )
    )

    assert observed == [
        (140, "worktree-agent", "xhigh"),
        (130, "repository-agent", "high"),
        (120, "pyproject-agent", "medium"),
        (110, "system-agent", "low"),
    ]
    capsys.readouterr()


@pytest.mark.parametrize("scope", config.CONFIG_SCOPE_NAMES)
def test_personality_and_judge_setters_write_each_named_scope(
    tmp_path, monkeypatch, scope
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    parser = build_parser()

    handle_config(
        parser.parse_args(["config", "personality", "friendly", "--scope", scope])
    )
    handle_config(
        parser.parse_args(
            ["config", "judge", "--bin", f"judge-{scope}", "--scope", scope]
        )
    )

    assert config.layer_table(tmp_path, scope, "agent")["personality"] == "friendly"
    assert config.layer_table(tmp_path, scope, "judge")["bin"] == f"judge-{scope}"


def test_judge_cli_toggles_worktree_adjudication_mode(tmp_path, monkeypatch):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    parser = build_parser()
    modes: list[str] = []

    for flag in ("--enable", "--disable"):
        handle_config(parser.parse_args(["config", "judge", flag]))
        modes.append(
            "adjudicated"
            if config.maxim_adjudication_enabled(tmp_path)
            else "judge-free"
        )

    assert modes == ["adjudicated", "judge-free"]
    assert config.layer_table(tmp_path, config.WORKTREE_SOURCE, "judge") == {
        "enabled": False
    }


def test_config_help_names_exact_scope_vocabulary():
    parser = build_parser()
    root_actions = next(
        action
        for action in parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )
    config_parser = root_actions.choices["config"]
    config_actions = next(
        action
        for action in config_parser._actions
        if isinstance(action, argparse._SubParsersAction)
    )

    for action in ("agent", "personality", "say", "judge"):
        help_text = config_actions.choices[action].format_help()
        assert "{system,pyproject,repository,worktree}" in help_text


def test_invalid_value_reports_selected_source_before_mutation(tmp_path, monkeypatch):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    before = config.config_overview(tmp_path)["layers"]["repository"]

    outcome = _config_mutation_outcome(
        lambda: handle_config(
            build_parser().parse_args(
                [
                    "config",
                    "say",
                    "--scope",
                    "repository",
                    "--words-per-minute",
                    "0",
                ]
            )
        )
    )

    assert outcome.state == "rejected"
    assert "scope=repository" in outcome.message
    assert f"path={tmp_path / 'spice.toml'}" in outcome.message
    assert config.config_overview(tmp_path)["layers"]["repository"] == before


def test_unwritable_system_scope_reports_source_before_mutation(tmp_path, monkeypatch):
    system_path = _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr(config.os, "access", lambda _path, _mode: False)
    before = system_path.read_bytes()

    outcome = _config_mutation_outcome(
        lambda: config.set_scope_section(
            tmp_path,
            config.SYSTEM_SOURCE,
            config.AGENT_KEY,
            {config.AGENT_MODEL_KEY: "blocked-model"},
        )
    )

    assert outcome == ConfigMutationOutcome(
        "rejected", f"configuration scope=system path={system_path} is not writable"
    )
    assert system_path.read_bytes() == before


def test_effective_agent_config_keeps_claude_sonnet_family(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
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
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
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
            scope="worktree",
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
            scope="worktree",
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
                scope="worktree",
                clear=False,
                backend="external",
                command=None,
                content_type=None,
                voice=None,
                words_per_minute=None,
            )
        )


def test_repository_say_validation_rejects_command_borrowed_from_worktree(
    tmp_path, monkeypatch
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    repository_path = tmp_path / "spice.toml"
    repository_path.write_text("# repository settings\n", encoding="utf-8")
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {config.SAY_COMMAND_KEY: "later-worktree-command"},
    )
    original = repository_path.read_bytes()
    parser = build_parser()

    outcome = _config_mutation_outcome(
        lambda: handle_config(
            parser.parse_args(
                [
                    "config",
                    "say",
                    "--scope",
                    "repository",
                    "--backend",
                    "external",
                ]
            )
        )
    )

    assert outcome.state == "rejected"
    assert "requires --command" in outcome.message
    assert f"scope=repository path={repository_path}" in outcome.message
    assert repository_path.read_bytes() == original


def test_repository_say_validation_accepts_command_from_earlier_scope(
    tmp_path, monkeypatch
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    config.set_scope_section(
        tmp_path,
        config.PYPROJECT_SOURCE,
        config.SAY_KEY,
        {config.SAY_COMMAND_KEY: "earlier-project-command"},
    )
    parser = build_parser()

    outcome = _config_mutation_outcome(
        lambda: handle_config(
            parser.parse_args(
                [
                    "config",
                    "say",
                    "--scope",
                    "repository",
                    "--backend",
                    "external",
                ]
            )
        )
    )

    assert outcome.state == "applied"
    assert config.configured_say_backend(tmp_path) == "external"
    assert config.configured_say_command(tmp_path) == "earlier-project-command"


def test_clearing_worktree_say_rejects_invalid_revealed_stack_without_writing(
    tmp_path, monkeypatch
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    (tmp_path / "spice.toml").write_text(
        '[say]\nbackend = "external"\n', encoding="utf-8"
    )
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {config.SAY_COMMAND_KEY: "later-worktree-command"},
    )
    worktree_path = config.worktree_config_path(tmp_path)
    original = worktree_path.read_bytes()
    parser = build_parser()

    outcome = _config_mutation_outcome(
        lambda: handle_config(
            parser.parse_args(["config", "say", "--scope", "worktree", "--clear"])
        )
    )

    assert outcome.state == "rejected"
    assert "requires --command" in outcome.message
    assert f"scope=worktree path={worktree_path}" in outcome.message
    assert worktree_path.read_bytes() == original


def test_configured_judge_bin_defaults_to_platform_adapter(tmp_path, monkeypatch):
    monkeypatch.setattr("sys.platform", "darwin")
    assert config.configured_judge_bin(tmp_path) == config.DEFAULT_JUDGE_BIN

    monkeypatch.setattr("sys.platform", "linux")
    assert config.configured_judge_bin(tmp_path) == config.PORTABLE_JUDGE_BIN


def test_explicit_judge_bin_overrides_platform_default(tmp_path, monkeypatch):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.JUDGE_KEY,
        {config.JUDGE_BIN_KEY: "/opt/my-judge"},
    )

    monkeypatch.setattr("sys.platform", "linux")
    assert config.configured_judge_bin(tmp_path) == "/opt/my-judge"

    monkeypatch.setattr("sys.platform", "darwin")
    assert config.configured_judge_bin(tmp_path) == "/opt/my-judge"


def test_say_timeout_defaults_generously_above_a_minute(tmp_path):
    assert config.configured_say_timeout(tmp_path) == config.DEFAULT_SAY_TIMEOUT_SECONDS
    assert config.configured_say_timeout(tmp_path) > SAY_TIMEOUT_MINUTE_FLOOR_SECONDS


def test_say_timeout_honors_positive_override(tmp_path):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {config.SAY_TIMEOUT_SECONDS_KEY: SAY_TIMEOUT_OVERRIDE_SECONDS},
    )
    assert config.configured_say_timeout(tmp_path) == SAY_TIMEOUT_OVERRIDE_SECONDS


def test_say_timeout_falls_back_when_non_positive_or_invalid(tmp_path):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {config.SAY_TIMEOUT_SECONDS_KEY: 0},
    )
    assert config.configured_say_timeout(tmp_path) == config.DEFAULT_SAY_TIMEOUT_SECONDS

    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {config.SAY_TIMEOUT_SECONDS_KEY: "nonsense"},
    )
    assert config.configured_say_timeout(tmp_path) == config.DEFAULT_SAY_TIMEOUT_SECONDS


def test_set_scope_section_preserves_comments_and_scalar_types(tmp_path):
    config_path = config.worktree_config_path(tmp_path)
    config_path.parent.mkdir(parents=True)
    config_path.write_text(
        "# keep this header\n"
        "[custom]\n"
        "flag = true\n\n"
        "[say]\n"
        'voice = "Alex"\n'
        "words_per_minute = 150 # operator rate\n",
        encoding="utf-8",
    )

    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {config.SAY_WORDS_PER_MINUTE_KEY: 200},
    )

    text = config_path.read_text(encoding="utf-8")
    assert "# keep this header" in text
    assert "words_per_minute = 200 # operator rate" in text
    parsed = tomllib.loads(text)
    assert parsed["say"] == {"voice": "Alex", "words_per_minute": 200}
    assert parsed["custom"] == {"flag": True}


def test_clear_scope_section_preserves_unrelated_tables(tmp_path):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.JUDGE_KEY,
        {config.JUDGE_BIN_KEY: "j"},
    )
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.AGENT_KEY,
        {config.AGENT_DRIVER_KEY: "claude"},
    )

    config.clear_scope_section(tmp_path, config.WORKTREE_SOURCE, config.JUDGE_KEY)

    parsed = tomllib.loads(
        config.worktree_config_path(tmp_path).read_text(encoding="utf-8")
    )
    assert "judge" not in parsed
    assert parsed["agent"] == {"driver": "claude"}


def test_maxim_adjudication_off_by_default_and_opt_in_toggles_it(tmp_path):
    modes = [
        "adjudicated" if config.maxim_adjudication_enabled(tmp_path) else "judge-free"
    ]
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.JUDGE_KEY,
        {config.JUDGE_ENABLED_KEY: True},
    )
    modes.append(
        "adjudicated" if config.maxim_adjudication_enabled(tmp_path) else "judge-free"
    )
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.JUDGE_KEY,
        {config.JUDGE_ENABLED_KEY: False},
    )
    modes.append(
        "adjudicated" if config.maxim_adjudication_enabled(tmp_path) else "judge-free"
    )

    assert modes == ["judge-free", "adjudicated", "judge-free"]


def test_maxim_adjudication_reads_only_the_worktree_layer(tmp_path, monkeypatch):
    _redirect_system_config(tmp_path, monkeypatch)
    inherited_scopes = (
        config.SYSTEM_SOURCE,
        config.PYPROJECT_SOURCE,
        config.REPOSITORY_SOURCE,
    )
    for scope in inherited_scopes:
        config.set_scope_section(
            tmp_path,
            scope,
            config.JUDGE_KEY,
            {config.JUDGE_ENABLED_KEY: True},
        )

    inherited_outcome = {
        "mode": (
            "adjudicated"
            if config.maxim_adjudication_enabled(tmp_path)
            else "judge-free"
        ),
        "enabled_layers": tuple(
            scope
            for scope in inherited_scopes
            if config.layer_table(tmp_path, scope, config.JUDGE_KEY).get(
                config.JUDGE_ENABLED_KEY
            )
            is True
        ),
    }
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.JUDGE_KEY,
        {config.JUDGE_ENABLED_KEY: True},
    )

    assert {
        "inherited": inherited_outcome,
        "worktree_mode": (
            "adjudicated"
            if config.maxim_adjudication_enabled(tmp_path)
            else "judge-free"
        ),
    } == {
        "inherited": {
            "mode": "judge-free",
            "enabled_layers": inherited_scopes,
        },
        "worktree_mode": "adjudicated",
    }
