"""A shell that reports `mode=native` does not rewrite the commands it runs.

The failure these cover is not a crash. An RTK whose rewrite changes a search's
answer used to keep rewriting and only stop being advertised, so activation said
`native` while the shell substituted a basic-dialect search for an extended one
and reported its zero as a result. Nothing in that sequence looks wrong from the
caller's side, which is why every test here compares against the command as
written and executes both sides rather than reading a decision back.
"""

from __future__ import annotations

import io
import json
import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent import rtkhealth, shellhook, wrap
from spice.config import values

ALTERNATION_PATTERN = "alpha|beta"
SUBJECT_LINES = "alpha\nbeta\ngamma\n"
NARROWED_REWRITE = "grep --count 'alpha|beta'"
UNFAITHFUL_DETAIL = "rewriting rg changed its answer: written 2, rewritten 0"


# The shell stage is isolated by replacing `subprocess.run`, and `wrap.subprocess`
# is that same module object, so a child launched afterwards would reach the fake
# rewriter instead of the program named on its argv. Both real executions in this
# file are bound to the launcher as it was before any test replaced it.
_LAUNCH = subprocess.run


class _ExecutedProcess:
    """A child that really runs, so the comparison is of answers and not plans."""

    def __init__(self, command: list[str]) -> None:
        self.command = command
        completed = _LAUNCH(command, capture_output=True, text=True)
        self.stdout = completed.stdout
        self.returncode = completed.returncode
        self.pid = 0

    def wait(self) -> int:
        return self.returncode


def _declare_health(monkeypatch: pytest.MonkeyPatch, state: str) -> None:
    """State the verdict this shell was activated under."""
    rtkhealth.AGENT_RTK_HEALTH.clear()
    monkeypatch.setattr(
        rtkhealth,
        "probe_rtk_health",
        lambda repo_root, **_kwargs: rtkhealth.RtkHealth(
            values.configured_rtk_executable(repo_root),
            state,
            UNFAITHFUL_DETAIL,
            rtkhealth.RTK_MINIMUM_VERSION_TEXT,
        ),
    )


def _isolate_shell_stage(monkeypatch: pytest.MonkeyPatch, rewrites: list[list[str]]):
    """Silence the side channel and record every rewrite the stage asks for."""
    wrap._rtk_warned_keys.clear()

    def run_rtk(command: list[str], **_kwargs: object) -> object:
        rewrites.append(list(command))
        subject = command[-1]
        return subprocess.CompletedProcess(
            [], 3, stdout=f"{NARROWED_REWRITE} {subject}\n", stderr=""
        )

    monkeypatch.setattr(wrap.subprocess, "run", run_rtk)
    monkeypatch.setattr(
        wrap, "bind_ambient_thread_for_shell_stage", lambda *_a, **_k: None
    )
    monkeypatch.setattr(wrap, "emit_initial_side_channel_payload", lambda *_a, **_k: ())
    monkeypatch.setattr(wrap, "start_agent_side_channel_watch", lambda *_a, **_k: None)


def _alternation_subject(tmp_path: Path) -> Path:
    subject = tmp_path / "subject.txt"
    subject.write_text(SUBJECT_LINES)
    return subject


def _run_alternation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, state: str
) -> tuple[_ExecutedProcess, list[list[str]], str]:
    """Run the written alternation through a shell activated under `state`."""
    subject = _alternation_subject(tmp_path)
    rewrites: list[list[str]] = []
    _declare_health(monkeypatch, state)
    _isolate_shell_stage(monkeypatch, rewrites)
    executed: list[_ExecutedProcess] = []

    def launch(argv: list[str], **_kwargs: object) -> _ExecutedProcess:
        executed.append(_ExecutedProcess(argv))
        return executed[-1]

    stderr = io.StringIO()
    wrap.run_agent_command(
        tmp_path,
        ["rg", "--count", ALTERNATION_PATTERN, str(subject)],
        popen_factory=launch,
        stderr=stderr,
    )
    return executed[-1], rewrites, stderr.getvalue()


def test_unfaithful_shell_answers_an_alternation_exactly_as_absolute_rg(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The declared-unfaithful shell returns the written search's own answer.

    Both sides are executed and their counts and exit status compared, because
    the failure being excluded produces a well-formed count and a plausible exit
    status -- a basic-dialect search reading `|` as a literal reports zero
    matches and exits 1, which is exactly what a genuine miss looks like.
    """
    rg = shutil.which("rg") or ""
    assert Path(rg).is_file(), "the comparison is against an absolute-path rg"
    subject = tmp_path / "subject.txt"
    executed, rewrites, _stderr = _run_alternation(
        tmp_path, monkeypatch, "rewrite-unfaithful"
    )
    written = _LAUNCH(
        [rg, "--count", ALTERNATION_PATTERN, str(subject)],
        capture_output=True,
        text=True,
    )

    assert {
        "stdout": executed.stdout,
        "returncode": executed.returncode,
        "command": executed.command,
        "rewrites_requested": rewrites,
    } == {
        "stdout": written.stdout,
        "returncode": written.returncode,
        "command": ["rg", "--count", ALTERNATION_PATTERN, str(subject)],
        "rewrites_requested": [],
    }


def test_unfaithful_shell_says_aloud_that_it_stopped_rewriting(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Refusing to rewrite is announced, naming RTK and the state that decided it.

    A rewrite that stops silently cannot be told apart from one that had nothing
    to say: both leave the command running natively with no diagnostic, and the
    agent keeps reading answers under a model of the shell that is no longer
    true. The announcement is what makes the two distinguishable.
    """
    _executed, rewrites, stderr = _run_alternation(
        tmp_path, monkeypatch, "rewrite-unfaithful"
    )

    assert {"stderr": stderr, "rewrites_requested": rewrites} == {
        "stderr": (
            "spice agent run: RTK rewrite degraded to native "
            "executable='rtk' failure=rewrite-not-permitted\n"
        ),
        "rewrites_requested": [],
    }


def test_a_healthy_shell_still_asks_rtk_to_rewrite(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The refusal is decided by the verdict, not by switching rewriting off.

    Without this the previous two tests are satisfied by an RTK that never
    rewrites anything, which is a different program rather than a fixed one.
    """
    subject = tmp_path / "subject.txt"
    _executed, rewrites, _stderr = _run_alternation(tmp_path, monkeypatch, "active")

    assert rewrites == [
        ["rtk", "rewrite", "--", "rg", "--count", ALTERNATION_PATTERN, str(subject)]
    ]


@pytest.mark.parametrize(
    ("state", "expected_mode", "expected_wrappers"),
    [
        # The advertised mode is read out of the same verdict that decides the
        # wrappers, so each row pairs what activation prints with what the shell
        # is actually carrying when it prints it.
        ("active", "active", ["rtk", "grep"]),
        ("rewrite-unfaithful", "native", ["grep"]),
        ("protocol-invalid", "native", ["grep"]),
        ("obsolete", "native", ["grep"]),
        ("missing", "native", ["grep"]),
    ],
)
def test_advertised_mode_matches_the_wrappers_the_shell_carries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    state: str,
    expected_mode: str,
    expected_wrappers: list[str],
) -> None:
    """What activation reports is a statement about the installed wrappers.

    A wrapper that expands a word into an RTK invocation is a rewrite the shell
    performs itself, so a shell reporting `native` while carrying one reports a
    mode it does not have. Both readings come from one verdict here, which is
    what makes the report checkable rather than merely printed.
    """
    _declare_health(monkeypatch, state)
    health = rtkhealth.agent_rtk_health(tmp_path)
    rendered = shellhook.render_agent_wrapper_lines(tmp_path)
    installed = [line[: -len("() {")] for line in rendered if line.endswith("() {")]
    advertised = json.loads(health.activation_status_line().split("=", 1)[1])

    assert {"mode": advertised["mode"], "wrappers": installed} == {
        "mode": expected_mode,
        "wrappers": expected_wrappers,
    }
