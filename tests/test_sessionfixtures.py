"""Shared paths for tracked session transcript fixtures."""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from spice.agent.driver import CLAUDE_DRIVER
from spice.sessions import records

SESSION_FIXTURE_DIR = Path(__file__).with_name("fixtures") / "session"
CODEX_SUPERVISED = SESSION_FIXTURE_DIR / "supervised_codex.jsonl"
CLAUDE_SUPERVISED = SESSION_FIXTURE_DIR / "supervised_claude.jsonl"
SUPERVISED_FIXTURES = (CODEX_SUPERVISED, CLAUDE_SUPERVISED)


@contextmanager
def transcript_driver_for_fixture(monkeypatch: Any, path: Path) -> Iterator[None]:
    with monkeypatch.context() as scoped:
        if path == CLAUDE_SUPERVISED:
            scoped.setattr(
                records, "driver_for_transcript", lambda _path: CLAUDE_DRIVER
            )
        yield
