"""`spice demo` seeds an ephemeral lane that serve renders from a canned transcript."""

from __future__ import annotations

import argparse

from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
from spice.cli.parser import build_parser
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
