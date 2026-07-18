"""Steering channel token: stable per worktree, surfaced + wrapped identically."""

import io
import subprocess
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import spice.mail.steeringkey as steeringkey
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


def test_steering_token_mint_never_queries_the_task_backend(tmp_path, monkeypatch):
    # The token is minted on the inbox-readout hot path, so it must not depend on
    # the task DB: a `tw.export()` there both couples a cosmetic recognition aid
    # to the backend and drags its failure surface into the readout. Make any
    # export blow up; minting must still yield a valid token.
    _init_git_repo(tmp_path)

    def _explode(*_args, **_kwargs):
        raise AssertionError("steering token mint must not call tw.export")

    monkeypatch.setattr("spice.tasks.tw.export", _explode)

    token = steering_token(tmp_path)
    assert token and all(ch in identity.ALPHABET for ch in token)


def test_steering_token_concurrent_callers_share_one_minted_value(
    tmp_path, monkeypatch
):
    _init_git_repo(tmp_path)
    caller_count = 6
    barrier = threading.Barrier(caller_count)
    minted: list[str] = []

    def mint() -> str:
        token = identity.ALPHABET[len(minted)] * 8
        minted.append(token)
        return token

    def read_token(_index: int) -> str:
        barrier.wait()
        return steering_token(tmp_path)

    monkeypatch.setattr(steeringkey, "_mint_token", mint)
    with ThreadPoolExecutor(max_workers=caller_count) as pool:
        observed = list(pool.map(read_token, range(caller_count)))

    first_token = identity.ALPHABET[0] * 8
    assert minted == [first_token]
    assert observed == [first_token] * caller_count
    assert steering_token(tmp_path) == first_token


def test_readout_wraps_the_block_in_the_token(tmp_path):
    _init_git_repo(tmp_path)
    token = steering_token(tmp_path)
    write_inbox_item(tmp_path, "1jNmXPHm.txt", "do the thing")
    buffer = io.StringIO()

    print_inbox_readout(tmp_path, displayed_keys=set(), file=buffer)

    lines = buffer.getvalue().splitlines()
    assert lines[0] == f"Inbox Steering  <{token}>"
    assert lines[-1].strip() == f"</{token}>"
