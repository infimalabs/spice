"""Portable in-box maxim judge adapter (`spice-judge`)."""

from __future__ import annotations

import io
import shlex
import sys
from pathlib import Path

import pytest

from spice.agent import judgeadapter

EXPLICIT_TIMEOUT_SECONDS = 12.5


@pytest.mark.parametrize(
    "text,expected",
    [
        ("YES", "YES"),
        ("The answer is YES.", "YES"),
        ("no", "NO"),
        ("definitely NO", "NO"),
        ("YES or NO?", None),
        ("maybe", None),
        ("", None),
    ],
)
def test_extract_verdict(text, expected):
    assert judgeadapter.extract_verdict(text) == expected


def test_resolve_model_command_defaults_to_documented_runner(monkeypatch):
    monkeypatch.delenv(judgeadapter.JUDGE_MODEL_COMMAND_ENV, raising=False)
    assert judgeadapter.resolve_model_command() == list(
        judgeadapter.DEFAULT_MODEL_COMMAND
    )


def test_resolve_model_command_honors_override(monkeypatch):
    monkeypatch.setenv(judgeadapter.JUDGE_MODEL_COMMAND_ENV, "ollama run mistral")
    assert judgeadapter.resolve_model_command() == ["ollama", "run", "mistral"]


def test_resolve_timeout_default_disable_and_value(monkeypatch):
    monkeypatch.delenv(judgeadapter.JUDGE_TIMEOUT_ENV, raising=False)
    assert judgeadapter.resolve_timeout() == judgeadapter.DEFAULT_TIMEOUT_SECONDS

    monkeypatch.setenv(judgeadapter.JUDGE_TIMEOUT_ENV, "0")
    assert judgeadapter.resolve_timeout() is None

    monkeypatch.setenv(judgeadapter.JUDGE_TIMEOUT_ENV, str(EXPLICIT_TIMEOUT_SECONDS))
    assert judgeadapter.resolve_timeout() == EXPLICIT_TIMEOUT_SECONDS


def test_resolve_timeout_rejects_non_numeric(monkeypatch):
    monkeypatch.setenv(judgeadapter.JUDGE_TIMEOUT_ENV, "soon")
    with pytest.raises(ValueError):
        judgeadapter.resolve_timeout()


def _fake_model(tmp_path: Path, name: str, body: str) -> str:
    script = tmp_path / name
    script.write_text(
        "\n".join([f"#!{sys.executable}", "import sys", body, ""]),
        encoding="utf-8",
    )
    script.chmod(0o755)
    return shlex.join([str(script)])


def test_main_emits_verdict_extracted_from_model(tmp_path, monkeypatch, capsys):
    command = _fake_model(
        tmp_path, "model-yes", "sys.stdin.read()\nprint('The answer is YES.')"
    )
    monkeypatch.setenv(judgeadapter.JUDGE_MODEL_COMMAND_ENV, command)
    monkeypatch.setattr("sys.stdin", io.StringIO("prompt"))

    code = judgeadapter.main([])

    assert code == judgeadapter.EXIT_SUCCESS
    assert capsys.readouterr().out.strip() == "YES"


def test_main_passes_ambiguous_reply_through_for_retry(tmp_path, monkeypatch, capsys):
    command = _fake_model(
        tmp_path, "model-vague", "sys.stdin.read()\nprint('maybe YES or NO')"
    )
    monkeypatch.setenv(judgeadapter.JUDGE_MODEL_COMMAND_ENV, command)
    monkeypatch.setattr("sys.stdin", io.StringIO("prompt"))

    code = judgeadapter.main([])

    assert code == judgeadapter.EXIT_SUCCESS
    assert "maybe YES or NO" in capsys.readouterr().out


def test_main_fails_loudly_when_model_missing(monkeypatch, capsys):
    monkeypatch.setenv(
        judgeadapter.JUDGE_MODEL_COMMAND_ENV, "definitely-not-a-real-judge-xyz"
    )
    monkeypatch.setattr("sys.stdin", io.StringIO("prompt"))

    code = judgeadapter.main([])

    assert code == judgeadapter.EXIT_FAILURE
    assert "not found" in capsys.readouterr().err


def test_main_fails_loudly_when_model_errors(tmp_path, monkeypatch, capsys):
    command = _fake_model(
        tmp_path, "model-boom", "sys.stderr.write('kaboom')\nraise SystemExit(3)"
    )
    monkeypatch.setenv(judgeadapter.JUDGE_MODEL_COMMAND_ENV, command)
    monkeypatch.setattr("sys.stdin", io.StringIO("prompt"))

    code = judgeadapter.main([])

    err = capsys.readouterr().err
    assert code == judgeadapter.EXIT_FAILURE
    assert "exited 3" in err
    assert "kaboom" in err
