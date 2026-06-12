"""Parse-error recovery hints and examples."""

from __future__ import annotations

import pytest

from spice.cli.parser import build_parser

PARSE_ERROR = 2


def test_root_parse_error_points_to_exact_contract(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit) as exc_info:
        parser.parse_args(["--definitely-wrong"])

    error = capsys.readouterr().err
    assert exc_info.value.code == PARSE_ERROR
    assert "Try `spice --help` for the exact contract." in error
    assert (
        "Choose one top-level command before passing command-specific flags." in error
    )
    assert "spice task status" in error


def test_task_subcommand_parse_error_prints_command_examples(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit) as exc_info:
        parser.parse_args(["task", "done", "TASK-1", "--bad-flag"])

    error = capsys.readouterr().err
    assert exc_info.value.code == PARSE_ERROR
    assert "Try `spice task done --help` for the exact contract." in error
    assert "Complete the current phase (advances or finishes)." in error
    assert (
        'spice task done TASK-20260609T203539640394Z --validation "tests passed"'
        in error
    )


def test_session_subcommand_parse_error_prints_filter_examples(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit) as exc_info:
        parser.parse_args(["session", "messages", "--bad-flag"])

    error = capsys.readouterr().err
    assert exc_info.value.code == PARSE_ERROR
    assert "Try `spice session messages --help` for the exact contract." in error
    assert (
        "Print individual user/assistant messages with phase and flavor filters."
        in error
    )
    assert "spice session messages --side assistant --limit 5" in error


def test_serve_subcommand_parse_error_prints_parent_option_example(capsys):
    parser = build_parser()

    with pytest.raises(SystemExit) as exc_info:
        parser.parse_args(["serve", "teams", "--bad-flag"])

    error = capsys.readouterr().err
    assert exc_info.value.code == PARSE_ERROR
    assert "Try `spice serve teams --help` for the exact contract." in error
    assert "Print serve team-store, routing, and task-drain diagnostics." in error
    assert "spice serve --task-backend /tmp/spice-smoke teams" in error
