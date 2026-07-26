"""Launch-history replay through the public typed transcript reader."""

from __future__ import annotations

import json
from pathlib import Path

from spice.agent import launchhistory
from spice.agent.driver import CLAUDE_DRIVER
from spice.transcript.reader import TranscriptEventReader
from tests.test_transcriptparity import parity_corpus

RESET_EPOCH = 1_784_280_000
RECORDED_PROJECTIONS = {
    "claude/shapes": {"assistant_messages": 2, "tool_calls": 0},
    "claude/resumed": {"assistant_messages": 2, "tool_calls": 0},
    "claude/recorded": {"assistant_messages": 7, "tool_calls": 1},
    "codex/shapes": {"assistant_messages": 2, "tool_calls": 1},
    "codex/resumed": {"assistant_messages": 2, "tool_calls": 1},
    "codex/recorded": {"assistant_messages": 6, "tool_calls": 3},
}


def test_recorded_launch_projections_cross_the_public_bounded_reader(
    monkeypatch,
) -> None:
    cases = parity_corpus()
    reads: list[tuple[Path, str, int, int | None]] = []
    read_events = TranscriptEventReader.read

    def track_read(self, mode, **kwargs):
        reads.append(
            (
                self.path,
                mode,
                kwargs.get("start_offset", 0),
                kwargs.get("end_offset"),
            )
        )
        return read_events(self, mode, **kwargs)

    monkeypatch.setattr(TranscriptEventReader, "read", track_read)
    observed: dict[str, dict[str, object]] = {}
    for case in cases:
        with monkeypatch.context() as scoped:
            scoped.setattr(
                launchhistory,
                "driver_for",
                lambda _root, driver=case.driver: driver,
            )
            observed[case.label] = launchhistory.scan_launch_log(
                case.path.parent,
                case.path,
            )

    assert observed == RECORDED_PROJECTIONS
    assert [(mode, start) for _path, mode, start, _end in reads] == [
        ("bounded", 0)
    ] * len(cases)
    assert [end == path.stat().st_size for path, _mode, _start, end in reads] == [
        True
    ] * len(cases)


def test_partial_terminal_failure_record_keeps_structural_outcome(
    tmp_path,
    monkeypatch,
) -> None:
    log_path = tmp_path / "launch.log"
    log_path.write_text(
        json.dumps(
            {
                "type": "rate_limit_event",
                "rate_limit_info": {
                    "status": "rejected",
                    "resetsAt": RESET_EPOCH,
                },
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setattr(launchhistory, "driver_for", lambda _root: CLAUDE_DRIVER)

    assert launchhistory.scan_launch_log(tmp_path, log_path) == {
        "assistant_messages": 0,
        "tool_calls": 0,
        "kind": "out-of-credits",
        "reset_epoch": RESET_EPOCH,
    }
