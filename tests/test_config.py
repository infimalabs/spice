"""Harness configuration: project defaults and worktree overrides."""

import argparse
import json
import tomllib
from collections.abc import Callable
from dataclasses import dataclass

import pytest

from spice.config import edit, layers, values
from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER, SPICE_AGENT_DRIVER_ENV
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
    monkeypatch.setattr("spice.config.edit.runtime_spice_source", lambda: system_root)
    monkeypatch.setattr(
        "spice.config.layers.paths.runtime_spice_source", lambda: system_root
    )
    return system_path


def test_repository_agent_layer_provides_launch_defaults(tmp_path):
    (tmp_path / "spice.toml").write_text(
        '[agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )

    assert values.configured_agent_model(tmp_path) == "gpt-project"
    assert values.configured_agent_effort(tmp_path) == "low"
    assert layers.layer_table(tmp_path, layers.REPOSITORY_SOURCE, "agent") == {
        "model": "gpt-project",
        "effort": "low",
    }


def test_worktree_agent_layer_overrides_repository_defaults(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    (tmp_path / "spice.toml").write_text(
        '[agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {
            values.AGENT_MODEL_KEY: "gpt-worktree",
            values.AGENT_EFFORT_KEY: "medium",
        },
    )

    assert values.configured_agent_model(tmp_path) == "gpt-worktree"
    assert values.configured_agent_effort(tmp_path) == "medium"
    assert values.effective_agent_config(tmp_path) == {
        "driver": "codex",
        "model": "gpt-worktree",
        "effort": "medium",
    }


def test_config_overview_shows_layers_effective_values_and_provenance(
    tmp_path, monkeypatch
):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    (tmp_path / "spice.toml").write_text(
        '[agent]\nmodel = "gpt-project"\neffort = "low"\n',
        encoding="utf-8",
    )
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {values.AGENT_EFFORT_KEY: "medium"},
    )

    overview = values.config_overview(tmp_path)

    assert tuple(overview["layers"]) == layers.CONFIG_SCOPE_NAMES
    assert overview["layers"]["repository"]["path"] == str(tmp_path / "spice.toml")
    assert overview["layers"]["worktree"]["values"] == {"agent": {"effort": "medium"}}
    assert overview["effective"]["agent"]["model"] == "gpt-project"
    assert overview["effective"]["agent"]["effort"] == "medium"
    assert overview["provenance"]["agent.model"] == {
        "scope": "repository",
        "path": str(tmp_path / "spice.toml"),
    }
    assert overview["provenance"]["agent.effort"] == {
        "scope": "worktree",
        "path": str(edit.worktree_config_path(tmp_path)),
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
    (tmp_path / "spice.toml").write_text(
        '[agent]\nmodel = "gpt-project"\neffort = "low"\n',
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
    assert rendered["provenance"]["agent.model"]["scope"] == "repository"
    assert sorted(path.name for path in tmp_path.iterdir()) == ["spice.toml"]


def test_config_set_writes_a_typed_schema_leaf_and_reports_provenance(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        build_parser().parse_args(
            [
                "config",
                "set",
                "policy.internal_couplings",
                '[{ path = "spice/config.py", test = "tests/test_config.py", '
                'target = "_config" }]',
                "--scope",
                "repository",
            ]
        )
    )

    assert result == 0
    expected = [
        {
            "path": "spice/config.py",
            "test": "tests/test_config.py",
            "target": "_config",
        }
    ]
    assert (
        layers.layer_table(tmp_path, layers.REPOSITORY_SOURCE, "policy")[
            "internal_couplings"
        ]
        == expected
    )
    rendered = json.loads(capsys.readouterr().out)
    assert rendered == {
        "effective": expected,
        "key": "policy.internal_couplings",
        "provenance": {
            "path": str(tmp_path / "spice.toml"),
            "scope": "repository",
        },
        "scope": "repository",
        "value": expected,
    }


def test_config_set_rejects_an_unknown_key_before_mutating_bytes(tmp_path, monkeypatch):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    config_path = tmp_path / "spice.toml"
    config_path.write_text("[policy.limits]\nfile_loc = 200\n", encoding="utf-8")
    before = config_path.read_bytes()

    with pytest.raises(SpiceError) as exc_info:
        handle_config(
            build_parser().parse_args(
                [
                    "config",
                    "set",
                    "policy.limits.file_lco",
                    "300",
                    "--scope",
                    "repository",
                ]
            )
        )

    assert str(exc_info.value) == (
        "unknown configuration key policy.limits.file_lco "
        f"(source=repository path={config_path}); "
        "did you mean policy.limits.file_loc?"
    )
    assert config_path.read_bytes() == before


def test_config_set_false_disables_an_inherited_entry_with_visible_provenance(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    (tmp_path / "spice.toml").write_text(
        '[commands]\naudit = ["echo", "audit"]\n',
        encoding="utf-8",
    )

    handle_config(
        build_parser().parse_args(["config", "set", "commands.audit", "false"])
    )

    assert layers.layer_table(tmp_path, layers.WORKTREE_SOURCE, "commands") == {
        "audit": False
    }
    assert layers.effective_commands(tmp_path) == {}
    rendered = json.loads(capsys.readouterr().out)
    assert rendered["value"] is False
    assert rendered["effective"] is False
    assert rendered["provenance"] == {
        "path": str(edit.worktree_config_path(tmp_path)),
        "scope": "worktree",
    }
    overview = values.config_overview(tmp_path)
    assert overview["effective"]["commands"]["audit"] is False
    assert overview["provenance"]["commands.audit"] == rendered["provenance"]


def test_config_set_preserves_a_quoted_dynamic_key_segment(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    handle_config(
        build_parser().parse_args(
            [
                "config",
                "set",
                'tasks.taskwarrior_urgency."age.coefficient"',
                "2.5",
                "--scope",
                "repository",
            ]
        )
    )

    assert layers.layer_table(
        tmp_path,
        layers.REPOSITORY_SOURCE,
        "tasks",
        "taskwarrior_urgency",
    ) == {"age.coefficient": 2.5}
    assert '"age.coefficient" = 2.5' in (tmp_path / "spice.toml").read_text(
        encoding="utf-8"
    )
    rendered = json.loads(capsys.readouterr().out)
    assert rendered["value"] == 2.5
    assert rendered["provenance"]["scope"] == "repository"


@pytest.mark.parametrize(
    ("key", "raw_value", "expected"),
    (
        ("say.backend", "say", "say"),
        ("say.command", "tts", "tts"),
        ("say.content_type", "audio/wav", "audio/wav"),
        ("say.voice", "Alex", "Alex"),
        ("say.words_per_minute", "190", 190),
        ("say.timeout_seconds", "12.5", 12.5),
        ("judge.bin", "judge", "judge"),
        ("judge.enabled", "false", False),
        ("agent.model", "gpt", "gpt"),
        ("agent.effort", "high", "high"),
        ("agent.driver", "codex", "codex"),
        ("agent.personality", "friendly", "friendly"),
    ),
)
def test_every_specialized_clearable_leaf_is_generic_settable(
    tmp_path, monkeypatch, capsys, key, raw_value, expected
):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    handle_config(build_parser().parse_args(["config", "set", key, raw_value]))

    section, leaf = key.split(".")
    assert (
        layers.layer_table(tmp_path, layers.WORKTREE_SOURCE, section)[leaf] == expected
    )
    capsys.readouterr()


def test_generic_set_closes_the_say_timeout_set_and_clear_gap(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    parser = build_parser()
    handle_config(parser.parse_args(["config", "set", "say.timeout_seconds", "12.5"]))

    assert values.configured_say_timeout(tmp_path) == SAY_TIMEOUT_OVERRIDE_SECONDS

    handle_config(parser.parse_args(["config", "say", "--clear"]))

    assert "timeout_seconds" not in layers.layer_table(
        tmp_path, layers.WORKTREE_SOURCE, "say"
    )
    capsys.readouterr()


def test_config_agent_writes_repository_scope(tmp_path, monkeypatch, capsys):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        argparse.Namespace(
            config_action="agent",
            scope="repository",
            clear=False,
            model="gpt-project",
            effort="high",
        )
    )

    assert result == 0
    assert layers.layer_table(tmp_path, layers.REPOSITORY_SOURCE, "agent") == {
        "model": "gpt-project",
        "effort": "high",
    }
    assert (
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
        "agent repository driver=- model=gpt-project effort=high\n"
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
    assert layers.layer_table(tmp_path, layers.WORKTREE_SOURCE, "agent") == {
        "model": "gpt-worktree",
        "effort": "low",
    }
    assert (
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
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
    assert values.configured_agent_driver(tmp_path) == "claude"
    assert (
        capsys.readouterr().out == "agent system driver=- model=- effort=-\n"
        "agent repository driver=- model=- effort=-\n"
        "agent worktree driver=claude model=- effort=-\n"
        "agent effective driver=claude model=claude-opus-4-8 effort=xhigh\n"
    )


def test_three_scope_precedence_clears_to_reveal_each_earlier_layer(
    tmp_path, monkeypatch, capsys
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    parser = build_parser()
    scope_layers = (
        ("system", 110, "system-agent", "low"),
        ("repository", 130, "repository-agent", "high"),
        ("worktree", 140, "worktree-agent", "xhigh"),
    )
    for scope, rate, model, effort in scope_layers:
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
    for scope in ("worktree", "repository"):
        observed.append(
            (
                values.configured_say_words_per_minute(tmp_path),
                values.configured_agent_model(tmp_path),
                values.configured_agent_effort(tmp_path),
            )
        )
        handle_config(parser.parse_args(["config", "say", "--scope", scope, "--clear"]))
        handle_config(
            parser.parse_args(["config", "agent", "--scope", scope, "--clear"])
        )
    observed.append(
        (
            values.configured_say_words_per_minute(tmp_path),
            values.configured_agent_model(tmp_path),
            values.configured_agent_effort(tmp_path),
        )
    )

    assert observed == [
        (140, "worktree-agent", "xhigh"),
        (130, "repository-agent", "high"),
        (110, "system-agent", "low"),
    ]
    capsys.readouterr()


@pytest.mark.parametrize("scope", layers.CONFIG_SCOPE_NAMES)
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

    assert layers.layer_table(tmp_path, scope, "agent")["personality"] == "friendly"
    assert layers.layer_table(tmp_path, scope, "judge")["bin"] == f"judge-{scope}"


@pytest.mark.parametrize(
    ("driver", "note"),
    [
        (CODEX_DRIVER, "driver=codex carries personality into every launch"),
        (
            CLAUDE_DRIVER,
            "driver=claude has no launch-time seam for personality, "
            "so this value does not reach the agent",
        ),
    ],
)
def test_personality_setter_names_whether_the_active_driver_carries_it(
    tmp_path, monkeypatch, capsys, driver, note
):
    # The value is written either way; what differs is whether it means anything
    # at launch, so the setter says which driver will be reading it and whether
    # that driver has a seam to carry it.
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    monkeypatch.setattr("spice.agent.driver.driver_for", lambda _repo_root: driver)
    parser = build_parser()

    handle_config(parser.parse_args(["config", "personality", "friendly"]))
    handle_config(parser.parse_args(["config", "personality"]))

    assert capsys.readouterr().out.splitlines() == [
        "personality=friendly",
        note,
        "personality=friendly",
        note,
    ]


def test_judge_cli_toggles_worktree_adjudication_mode(tmp_path, monkeypatch):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    parser = build_parser()
    modes: list[str] = []

    for flag in ("--enable", "--disable"):
        handle_config(parser.parse_args(["config", "judge", flag]))
        modes.append(
            "adjudicated"
            if values.maxim_adjudication_enabled(tmp_path)
            else "judge-free"
        )

    assert modes == ["adjudicated", "judge-free"]
    assert layers.layer_table(tmp_path, layers.WORKTREE_SOURCE, "judge") == {
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

    for action in ("set", "agent", "personality", "say", "judge"):
        help_text = config_actions.choices[action].format_help()
        assert "{system,repository,worktree}" in help_text


def test_invalid_value_reports_selected_source_before_mutation(tmp_path, monkeypatch):
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    before = values.config_overview(tmp_path)["layers"]["repository"]

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
    assert values.config_overview(tmp_path)["layers"]["repository"] == before


def test_unwritable_system_scope_reports_source_before_mutation(tmp_path, monkeypatch):
    system_path = _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr(edit.os, "access", lambda _path, _mode: False)
    before = system_path.read_bytes()

    outcome = _config_mutation_outcome(
        lambda: edit.set_scope_section(
            tmp_path,
            layers.SYSTEM_SOURCE,
            values.AGENT_KEY,
            {values.AGENT_MODEL_KEY: "blocked-model"},
        )
    )

    assert outcome == ConfigMutationOutcome(
        "rejected", f"configuration scope=system path={system_path} is not writable"
    )
    assert system_path.read_bytes() == before


def test_effective_agent_config_keeps_claude_sonnet_family(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {"driver": "claude", "model": "sonnet"},
    )

    assert values.configured_agent_model(tmp_path) == "sonnet"
    assert values.effective_agent_config(tmp_path) == {
        "driver": "claude",
        "model": "sonnet",
        "effort": "xhigh",
    }


def test_effective_agent_config_preserves_explicit_claude_model(tmp_path, monkeypatch):
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {"driver": "claude", "model": "claude-sonnet-4-6"},
    )

    assert values.effective_agent_config(tmp_path) == {
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
    assert values.configured_say_backend(tmp_path) == "say"
    assert values.say_command_args(tmp_path) == ["say", "-v", "Samantha", "-r", "190"]
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
    assert values.configured_say_backend(tmp_path) == "external"
    assert values.configured_say_command(tmp_path) == "tts-engine --wav"
    assert values.configured_say_content_type(tmp_path) == "audio/wav"
    assert capsys.readouterr().out == (
        "say backend=external command=tts-engine --wav content_type=audio/wav\n"
    )


@pytest.mark.parametrize(
    ("section", "key", "invalid", "choices", "load"),
    (
        (
            values.SAY_KEY,
            values.SAY_BACKEND_KEY,
            "whispered",
            values.SAY_BACKEND_CHOICES,
            values.configured_say_backend,
        ),
        (
            values.AGENT_KEY,
            values.AGENT_PERSONALITY_KEY,
            "reckless",
            values.AGENT_PERSONALITY_CHOICES,
            values.configured_agent_personality,
        ),
    ),
)
def test_out_of_set_choice_refuses_with_key_value_and_valid_set(
    tmp_path, section, key, invalid, choices, load
):
    config_path = tmp_path / "spice.toml"
    config_path.write_text(
        f'[{section}]\n{key} = "{invalid}"\n',
        encoding="utf-8",
    )

    with pytest.raises(SpiceError) as exc_info:
        load(tmp_path)

    valid = ", ".join(repr(choice) for choice in choices)
    assert str(exc_info.value) == (
        f"{section}.{key} (source=repository path={config_path}): "
        f"has invalid value {invalid!r}; expected one of {valid}"
    )


def test_every_choice_coercion_path_rejects_instead_of_substituting_its_default():
    policies = {
        path: policy for path, policy in values.SCALAR_SCHEMA.items() if policy.choices
    }

    assert tuple(policies) == (
        (values.SAY_KEY, values.SAY_BACKEND_KEY),
        (values.AGENT_KEY, values.AGENT_PERSONALITY_KEY),
    )
    for path, policy in policies.items():
        with pytest.raises(SpiceError, match="invalid value '__outside_valid_set__'"):
            policy.coerce("__outside_valid_set__", policy, path)


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
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {values.SAY_COMMAND_KEY: "later-worktree-command"},
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
    edit.set_scope_section(
        tmp_path,
        layers.SYSTEM_SOURCE,
        values.SAY_KEY,
        {values.SAY_COMMAND_KEY: "earlier-system-command"},
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
    assert values.configured_say_backend(tmp_path) == "external"
    assert values.configured_say_command(tmp_path) == "earlier-system-command"


def test_clearing_worktree_say_rejects_invalid_revealed_stack_without_writing(
    tmp_path, monkeypatch
):
    _redirect_system_config(tmp_path, monkeypatch)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)
    (tmp_path / "spice.toml").write_text(
        '[say]\nbackend = "external"\n', encoding="utf-8"
    )
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {values.SAY_COMMAND_KEY: "later-worktree-command"},
    )
    worktree_path = edit.worktree_config_path(tmp_path)
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
    assert values.configured_judge_bin(tmp_path) == values.DEFAULT_JUDGE_BIN

    monkeypatch.setattr("sys.platform", "linux")
    assert values.configured_judge_bin(tmp_path) == values.PORTABLE_JUDGE_BIN


def test_explicit_judge_bin_overrides_platform_default(tmp_path, monkeypatch):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_BIN_KEY: "/opt/my-judge"},
    )

    monkeypatch.setattr("sys.platform", "linux")
    assert values.configured_judge_bin(tmp_path) == "/opt/my-judge"

    monkeypatch.setattr("sys.platform", "darwin")
    assert values.configured_judge_bin(tmp_path) == "/opt/my-judge"


def test_say_timeout_defaults_generously_above_a_minute(tmp_path):
    assert values.configured_say_timeout(tmp_path) == values.DEFAULT_SAY_TIMEOUT_SECONDS
    assert values.configured_say_timeout(tmp_path) > SAY_TIMEOUT_MINUTE_FLOOR_SECONDS


def test_say_timeout_honors_positive_override(tmp_path):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {values.SAY_TIMEOUT_SECONDS_KEY: SAY_TIMEOUT_OVERRIDE_SECONDS},
    )
    assert values.configured_say_timeout(tmp_path) == SAY_TIMEOUT_OVERRIDE_SECONDS


def test_say_timeout_falls_back_when_non_positive_or_invalid(tmp_path):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {values.SAY_TIMEOUT_SECONDS_KEY: 0},
    )
    assert values.configured_say_timeout(tmp_path) == values.DEFAULT_SAY_TIMEOUT_SECONDS

    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {values.SAY_TIMEOUT_SECONDS_KEY: "nonsense"},
    )
    assert values.configured_say_timeout(tmp_path) == values.DEFAULT_SAY_TIMEOUT_SECONDS


def test_set_scope_section_preserves_comments_and_scalar_types(tmp_path):
    config_path = edit.worktree_config_path(tmp_path)
    config_path.parent.mkdir(parents=True)
    config_path.write_text(
        "# keep this header\n"
        "[serve]\n"
        'brand = "spice"\n\n'
        "[say]\n"
        'voice = "Alex"\n'
        "words_per_minute = 150 # operator rate\n",
        encoding="utf-8",
    )

    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {values.SAY_WORDS_PER_MINUTE_KEY: 200},
    )

    text = config_path.read_text(encoding="utf-8")
    assert "# keep this header" in text
    assert "words_per_minute = 200 # operator rate" in text
    parsed = tomllib.loads(text)
    assert parsed["say"] == {"voice": "Alex", "words_per_minute": 200}
    assert parsed["serve"] == {"brand": "spice"}


def test_clear_scope_section_preserves_unrelated_tables(tmp_path):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_BIN_KEY: "j"},
    )
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {values.AGENT_DRIVER_KEY: "claude"},
    )

    edit.clear_scope_section(tmp_path, layers.WORKTREE_SOURCE, values.JUDGE_KEY)

    parsed = tomllib.loads(
        edit.worktree_config_path(tmp_path).read_text(encoding="utf-8")
    )
    assert "judge" not in parsed
    assert parsed["agent"] == {"driver": "claude"}


def test_maxim_adjudication_off_by_default_and_opt_in_toggles_it(tmp_path):
    modes = [
        "adjudicated" if values.maxim_adjudication_enabled(tmp_path) else "judge-free"
    ]
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_ENABLED_KEY: True},
    )
    modes.append(
        "adjudicated" if values.maxim_adjudication_enabled(tmp_path) else "judge-free"
    )
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_ENABLED_KEY: False},
    )
    modes.append(
        "adjudicated" if values.maxim_adjudication_enabled(tmp_path) else "judge-free"
    )

    assert modes == ["judge-free", "adjudicated", "judge-free"]


def test_maxim_adjudication_honors_committed_config_layers(tmp_path, monkeypatch):
    _redirect_system_config(tmp_path, monkeypatch)
    # A committed spice.toml turns adjudication on for the whole repository,
    # so an install (like spice itself) can enable the judge without editing an
    # uncommitted worktree-local config.
    edit.set_scope_section(
        tmp_path,
        layers.REPOSITORY_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_ENABLED_KEY: True},
    )
    committed_mode = (
        "adjudicated" if values.maxim_adjudication_enabled(tmp_path) else "judge-free"
    )

    # The highest-precedence worktree layer still wins, so a local override can
    # switch adjudication back off even when a committed layer enabled it.
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_ENABLED_KEY: False},
    )

    assert {
        "committed": committed_mode,
        "worktree_override": (
            "adjudicated"
            if values.maxim_adjudication_enabled(tmp_path)
            else "judge-free"
        ),
    } == {
        "committed": "adjudicated",
        "worktree_override": "judge-free",
    }
