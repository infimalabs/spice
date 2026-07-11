"""`spice demo` seeds an ephemeral lane that serve renders from a canned transcript."""

from __future__ import annotations

import argparse
import os
import shlex
import subprocess

import pytest

from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.serve import demo
from spice.serve.app import ServeState
from spice.serve.demo import CANNED_TRANSCRIPT, seed_demo_environment
from spice.serve.messages import assistant_messages_for_thread_id
from spice.serve.payload.message import messages_payload_for_worktree
from spice.tasks import config as task_config


def _isolate_driver_homes(env, tmp_path, monkeypatch) -> None:
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(env.driver_home))
    monkeypatch.setenv("CODEX_HOME", str(tmp_path / "codex-empty"))


def _demo_subparser(parser: argparse.ArgumentParser) -> argparse.ArgumentParser:
    for action in parser._actions:
        choices = getattr(action, "choices", None)
        if choices and "demo" in choices:
            return choices["demo"]
    raise AssertionError("demo subparser is registered")


def test_seeded_demo_lane_renders_canned_transcript(tmp_path, monkeypatch):
    env = seed_demo_environment(root=tmp_path / "demo")
    _isolate_driver_homes(env, tmp_path, monkeypatch)

    read = assistant_messages_for_thread_id(env.thread_id, repo_root=env.repo_root)

    assert read.error is None
    assert read.transcript is not None
    # serve's transcript reader surfaces every canned turn as the lane's rendered
    # conversation, newest-first, from static fixture data with no model call.
    assert [item.text for item in read.items] == [
        text for _ts, text in reversed(CANNED_TRANSCRIPT)
    ]


def test_demo_lane_serve_payload_renders_fixture_turns(tmp_path, monkeypatch):
    env = seed_demo_environment(root=tmp_path / "demo")
    _isolate_driver_homes(env, tmp_path, monkeypatch)
    # Resolve serve's team store under the demo's own ephemeral backend so the
    # real operator board is never opened; set_backend is serve's documented
    # backend selector (spice serve --task-backend).
    task_config.set_backend(str(env.task_backend))
    try:
        state = ServeState(anchor_root=env.repo_root)
        targets = state.worktree_targets()
        assert targets  # serve discovers the seeded repo as a lane
        payload = messages_payload_for_worktree(state, targets[0], limit=200)
    finally:
        task_config.set_backend(None)

    rendered = [message["text"] for message in payload["messages"] if message["text"]]
    # the demo lane's actual serve payload -- not just the reader primitive --
    # carries every fixture turn as a completed conversation, newest-first, from
    # static fixture data with no live agent or model call.
    assert rendered == [text for _ts, text in reversed(CANNED_TRANSCRIPT)]


def test_demo_seed_writes_only_under_its_ephemeral_root(tmp_path):
    demo_root = tmp_path / "demo"
    env = seed_demo_environment(root=demo_root)

    assert env.root == demo_root.resolve()
    assert env.repo_root == demo_root.resolve()
    # every artifact the seed produced lives under the throwaway root, so a real
    # worktree or task board is never touched.
    assert env.transcript_path.is_file()
    assert env.transcript_path.is_relative_to(env.root)
    assert env.driver_home.is_relative_to(env.root)
    assert env.task_backend.is_relative_to(env.root)
    assert (env.repo_root / ".git").is_dir()


def test_demo_command_is_registered_with_zero_setup_help():
    parser = build_parser()
    args = parser.parse_args(["demo", "--seed-only", "--root", "/tmp/spice-demo-x"])

    assert args.command == "demo"
    assert args.func is demo.run_demo
    assert args.seed_only is True

    description = _demo_subparser(parser).description or ""
    for phrase in (
        "zero-setup",
        "serve UI",
        "canned transcript",
        "no agent subscription",
        "no model calls",
    ):
        assert phrase in description


def test_demo_root_gate_preserves_existing_content(tmp_path):
    nonempty = tmp_path / "operator-worktree"
    nonempty.mkdir()
    sentinel = nonempty / "sentinel.txt"
    sentinel.write_text("operator data\n", encoding="utf-8")
    git_dir = nonempty / ".git"
    git_dir.mkdir()
    root_file = tmp_path / "not-a-directory"
    root_file.write_text("operator file\n", encoding="utf-8")

    for protected in (nonempty, root_file):
        with pytest.raises(SpiceError):
            seed_demo_environment(root=protected)

    assert sentinel.read_text(encoding="utf-8") == "operator data\n"
    assert git_dir.is_dir()
    assert root_file.read_text(encoding="utf-8") == "operator file\n"


def test_seed_only_command_is_quoted_and_anchors_the_demo_repo(
    tmp_path, monkeypatch, capsys
):
    demo_root = tmp_path / "demo root"
    fake_bin = tmp_path / "fake bin"
    fake_bin.mkdir()
    probe = tmp_path / "serve probe.json"
    fake_spice = fake_bin / "spice"
    fake_spice.write_text(
        "#!/bin/sh\n"
        'printf \'%s\\n\' "$PWD" "$CLAUDE_CONFIG_DIR" "$@" > '
        f"{shlex.quote(str(probe))}\n",
        encoding="utf-8",
    )
    fake_spice.chmod(0o755)
    monkeypatch.setenv("PATH", str(fake_bin), prepend=os.pathsep)

    args = argparse.Namespace(
        root=demo_root,
        host="127.0.0.1",
        port=8765,
        seed_only=True,
    )
    assert demo.run_demo(args) == 0
    output = capsys.readouterr().out
    command = output.split("spice demo: start the server with:\n  ", 1)[1].strip()
    caller = tmp_path / "caller cwd"
    caller.mkdir()

    subprocess.run(command, shell=True, cwd=caller, check=True)

    lines = probe.read_text(encoding="utf-8").splitlines()
    assert lines[0] == str(demo_root.resolve())
    assert lines[1] == str(demo_root.resolve() / "driver-home")
    assert lines[2:] == [
        "serve",
        "--task-backend",
        str(demo_root.resolve() / "task-backend"),
        "--host",
        "127.0.0.1",
        "--port",
        "8765",
    ]
