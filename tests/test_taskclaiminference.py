"""Handle inference from the active claim for done/review/unclaim (CLI-1kBd0cb2).

`spice task done|review|unclaim` accept an omitted handle and fill it from the
single claim the actor holds; zero or multiple active claims keep the handle
required so the target is never guessed.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from spice.agent.driver import DRIVER
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.tasks import claimstate, config, create, ops

ACTOR_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


# --- Parser contract: the handle positional is optional -------------------


@pytest.mark.parametrize("action", ["done", "review", "unclaim"])
def test_handle_is_optional_for_claim_scoped_actions(action):
    extra = ["--validation", "done"] if action == "done" else []
    args = build_parser().parse_args(["task", action, *extra])

    assert args.handle is None


@pytest.mark.parametrize("action", ["done", "review", "unclaim"])
def test_explicit_handle_still_parses(action):
    extra = ["--validation", "done"] if action == "done" else []
    args = build_parser().parse_args(["task", action, "CLI-1k4Q5gJw", *extra])

    assert args.handle == "CLI-1k4Q5gJw"


# --- Resolver: explicit -> sole claim -> fail loud ------------------------


def test_resolve_prefers_explicit_handle_over_the_claim(monkeypatch):
    explicit = {"uuid": "explicit"}
    monkeypatch.setattr(claimstate.identity, "resolve", lambda handle: explicit)
    monkeypatch.setattr(
        claimstate, "_active_claims_for", lambda _actor: [{"uuid": "claimed"}]
    )

    assert claimstate.resolve_claim_target("CLI-1k4Q5gJw", action="done") is explicit


def test_resolve_infers_the_sole_active_claim(monkeypatch):
    claim = {"uuid": "claimed"}
    monkeypatch.setattr(claimstate.tw, "current_actor", lambda: ACTOR_A)
    monkeypatch.setattr(claimstate, "_active_claims_for", lambda actor: [claim])

    assert claimstate.resolve_claim_target(None, action="done") is claim
    assert claimstate.resolve_claim_target("   ", action="done") is claim


def test_resolve_without_a_claim_requires_the_handle(monkeypatch):
    monkeypatch.setattr(claimstate.tw, "current_actor", lambda: ACTOR_A)
    monkeypatch.setattr(claimstate, "_active_claims_for", lambda _actor: [])

    with pytest.raises(SpiceError) as exc:
        claimstate.resolve_claim_target(None, action="done")

    assert "requires a handle" in str(exc.value)
    assert "no active claim" in str(exc.value)


def test_resolve_with_multiple_claims_requires_an_explicit_handle(monkeypatch):
    monkeypatch.setattr(claimstate.tw, "current_actor", lambda: ACTOR_A)
    monkeypatch.setattr(
        claimstate,
        "_active_claims_for",
        lambda _actor: [{"uuid": "one"}, {"uuid": "two"}],
    )
    monkeypatch.setattr(
        claimstate.identity,
        "render_handle",
        lambda row: "CLI-aaa" if row["uuid"] == "one" else "CLI-bbb",
    )

    with pytest.raises(SpiceError) as exc:
        claimstate.resolve_claim_target(None, action="unclaim")

    message = str(exc.value)
    assert "requires an explicit handle" in message
    assert "2 active claims" in message
    assert "CLI-aaa" in message and "CLI-bbb" in message


# --- End to end through the real backend ----------------------------------


def test_done_without_a_handle_completes_the_sole_claim(task_repo):
    handle = create.add(
        "Infer the handle from my claim",
        project="task.unit",
        origin="ack:20260101T000000000000Z",
        priority="medium",
        flow=["todo"],
        acceptance=["done with no handle completes the held claim"],
        claim=True,
    )

    result = ops.done(None, validation=["handle inferred from the active claim"])

    assert handle in result
    assert "completed" in result
    # The claim is gone, so a second bare done has nothing to infer.
    with pytest.raises(SpiceError) as exc:
        ops.done(None, validation=["nothing to complete"])
    assert "requires a handle" in str(exc.value)


def _init_repo(path: Path) -> Path:
    path.mkdir()
    _run(path, "git", "init", "-b", "main")
    _run(path, "git", "config", "user.email", "spice@example.test")
    _run(path, "git", "config", "user.name", "Spice Tests")
    (path / "README.md").write_text("initial\n", encoding="utf-8")
    _run(path, "git", "add", "README.md")
    _run(path, "git", "commit", "-m", "initial")
    return path


def _run(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True)


@pytest.fixture
def task_repo(tmp_path, monkeypatch):
    if shutil.which("task") is None:
        pytest.skip("Taskwarrior binary is required")
    repo = _init_repo(tmp_path / "repo")
    backend = tmp_path / "task-backend"
    monkeypatch.chdir(repo)
    monkeypatch.setenv(DRIVER.thread_id_env, ACTOR_A)
    monkeypatch.setenv("CODEX_TURN_ID", "turn-a")
    config.set_backend(str(backend))
    try:
        yield repo
    finally:
        config.set_backend(None)
