"""Launch-log history: activity counts and structural failure classification."""

import json

from spice.agent import driver as agent_driver
from spice.agent import launchhistory


def test_launch_outcome_counts_prose_and_its_tool_call_on_one_line(
    tmp_path, monkeypatch
):
    # A supervised agent narrates and acts in the same assistant message, so one
    # stream line carries both blocks. The scan counts every typed fact on the
    # line rather than the single item a collapsed projection could carry.
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, "claude")
    log_path = tmp_path / "launch.log"
    line = {
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "reading the reader seam"},
                {
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "Bash",
                    "input": {"command": "ls"},
                },
            ],
        },
    }
    log_path.write_text(f"{json.dumps(line)}\n", encoding="utf-8")

    outcome = launchhistory.supervised_launch_outcome(
        tmp_path,
        thread_id="805282e9dafc40148523e6e7ae0a4144",
        log_path=log_path,
        started_at="2026-07-25T06:21:10.042183Z",
        lifetime_seconds=91.4,
        exit_code=0,
    )

    assert outcome["assistant_messages"] == 1
    assert outcome["tool_calls"] == 1


def test_absent_launch_log_scans_as_quiet_rather_than_erroring(tmp_path, monkeypatch):
    # A launch that died before its log existed still has to yield an outcome:
    # the reader reports the unreadable source and the scan stays empty, which
    # is the same shape a launch that simply said nothing produces.
    monkeypatch.setenv(agent_driver.SPICE_AGENT_DRIVER_ENV, "claude")

    scan = launchhistory.scan_launch_log(tmp_path, tmp_path / "never-written.log")

    assert scan == {"assistant_messages": 0, "tool_calls": 0}
