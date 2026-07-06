"""Steering channel token: stable per worktree, surfaced + wrapped identically."""

import io
import subprocess
from pathlib import Path

from spice.mail.inbox import write_inbox_item
from spice.mail.readout import print_inbox_readout
from spice.mail.steeringkey import steering_token
from spice.tasks import identity


def _init_git_repo(path: Path) -> None:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


def test_steering_token_is_stable_base52_and_minted_once(tmp_path):
    _init_git_repo(tmp_path)
    token = steering_token(tmp_path)
    assert token and all(ch in identity.ALPHABET for ch in token)
    assert steering_token(tmp_path) == token  # reused, not re-minted


def test_steering_token_is_empty_without_a_repo():
    assert steering_token(None) == ""


def test_readout_wraps_the_block_in_the_token(tmp_path):
    _init_git_repo(tmp_path)
    token = steering_token(tmp_path)
    write_inbox_item(tmp_path, "20260104T000000000004Z.txt", "do the thing")
    buffer = io.StringIO()

    print_inbox_readout(tmp_path, displayed_keys=set(), file=buffer)

    lines = buffer.getvalue().splitlines()
    assert lines[0] == f"Inbox Steering  <{token}>"
    assert lines[-1].strip() == f"</{token}>"
