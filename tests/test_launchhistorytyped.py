"""Launch-history replay through the public typed transcript reader."""

from __future__ import annotations

import json
from pathlib import Path

from spice.agent import launchhistory
from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER
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
    observed = {
        case.label: launchhistory.scan_transcript_activity(case.driver, case.path)
        for case in cases
    }

    assert observed == RECORDED_PROJECTIONS
    assert [(mode, start) for _path, mode, start, _end in reads] == [
        ("bounded", 0)
    ] * len(cases)
    assert [end == path.stat().st_size for path, _mode, _start, end in reads] == [
        True
    ] * len(cases)


def _marker_launch_log(path: Path) -> Path:
    """Write a launch log in the shape a marker-format driver prints."""
    path.write_text(
        "\n".join(
            [
                "OpenAI Codex v0.145.0",
                "user",
                "bootstrap",
                CODEX_DRIVER.stdout_assistant_marker,
                "ACK 1kJ3qGy6: reading the board.",
                "exec",
                "spice task next",
                "exec",
                "spice task show",
                CODEX_DRIVER.stdout_assistant_marker,
                "Board is empty; exiting.",
                "tokens used",
            ]
        ),
        encoding="utf-8",
    )
    return path


def test_marker_format_launch_log_records_the_work_it_shows(tmp_path, monkeypatch):
    """A human-readable launch log still has to report the activity it printed.

    Marker stdout holds no JSON records, so counting it as a transcript reports
    zero for every launch — a long working session and a crash on boot become
    indistinguishable, and the rapid-death guard reads both as bare starts.
    """
    log_path = _marker_launch_log(tmp_path / "launch.log")
    monkeypatch.setattr(launchhistory, "driver_for", lambda _root: CODEX_DRIVER)

    assert launchhistory.scan_launch_log(tmp_path, log_path) == {
        "assistant_messages": 2,
        "tool_calls": 2,
    }


def test_launch_log_dialect_follows_the_driver_not_the_bytes(tmp_path, monkeypatch):
    """The same bytes read differently under each driver's declared format.

    Both scans see one identical file, so the differing counts can only come
    from the dialect its driver names — the deciding input is `stdout_format`,
    never anything sniffed from the content.
    """
    log_path = _marker_launch_log(tmp_path / "launch.log")

    with monkeypatch.context() as scoped:
        scoped.setattr(launchhistory, "driver_for", lambda _root: CODEX_DRIVER)
        marker_scan = launchhistory.scan_launch_log(tmp_path, log_path)
    with monkeypatch.context() as scoped:
        scoped.setattr(launchhistory, "driver_for", lambda _root: CLAUDE_DRIVER)
        json_scan = launchhistory.scan_launch_log(tmp_path, log_path)

    assert marker_scan != json_scan
    assert marker_scan["assistant_messages"] > 0
    assert json_scan == {"assistant_messages": 0, "tool_calls": 0}


def test_marker_counts_cover_only_the_launch_that_printed_them(tmp_path, monkeypatch):
    """Counts stay scoped to one launch rather than the thread behind it.

    Two launches on one thread write separate logs. Reading the driver's whole
    thread transcript would let the second inherit the first's work and clear a
    guard it never earned, so each log answers only for itself.
    """
    monkeypatch.setattr(launchhistory, "driver_for", lambda _root: CODEX_DRIVER)
    worked = _marker_launch_log(tmp_path / "first.log")
    idle = tmp_path / "second.log"
    idle.write_text("OpenAI Codex v0.145.0\nuser\nbootstrap\n", encoding="utf-8")

    assert launchhistory.scan_launch_log(tmp_path, worked)["assistant_messages"] == 2
    assert launchhistory.scan_launch_log(tmp_path, idle) == {
        "assistant_messages": 0,
        "tool_calls": 0,
    }


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
