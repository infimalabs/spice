"""Fault injection for the supervisor's never-die boundary handlers.

The handlers whose entire job is keeping the fleet alive when a supervised
side task fails were previously marked ``# pragma: no cover``. These tests
inject a deterministic fault into each side task and assert the supervisor
(a) surfaces the failure and (b) does not raise — so the coverage exclusion is
no longer needed for the paths exercised here.
"""

from __future__ import annotations

import io
from types import SimpleNamespace

import pytest

from spice.agent import sidechannel, watchdog, wrap


@pytest.fixture
def empty_ack_summary(monkeypatch):
    monkeypatch.setattr(
        watchdog,
        "summarize_nack_archival",
        lambda _repo, _text: SimpleNamespace(
            refused=[],
            already_refused=[],
            already_acked=[],
            unmatched=[],
            reasonless=[],
        ),
    )
    monkeypatch.setattr(
        watchdog,
        "summarize_ack_archival",
        lambda _repo, _text: SimpleNamespace(
            archived=[], already_acked=[], unmatched=[], noop=False
        ),
    )


def test_inline_task_failure_is_surfaced_and_supervisor_survives(
    tmp_path, monkeypatch, empty_ack_summary
):
    feedback: list[tuple[str, dict]] = []
    monkeypatch.setattr(
        watchdog,
        "publish_side_channel_feedback",
        lambda _repo, kind, **fields: feedback.append((kind, fields)),
    )

    def boom(_repo, _text, _log):
        raise RuntimeError("inline-boom")

    monkeypatch.setattr(watchdog, "create_inline_tasks", boom)
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog, "publish_maxim_hits_as_inbox", lambda _repo, _text, **_kw: []
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        tmp_path, "plain message", log, watchdog.MaximReminderGate()
    )

    assert "spice inline task supervisor error: inline-boom" in log.getvalue()
    assert any(kind == "task.error" for kind, _fields in feedback)


def test_metric_failure_is_surfaced_and_supervisor_survives(
    tmp_path, monkeypatch, empty_ack_summary
):
    monkeypatch.setattr(watchdog, "create_inline_tasks", lambda _repo, _text, _log: [])
    monkeypatch.setattr(
        watchdog, "publish_maxim_hits_as_inbox", lambda _repo, _text, **_kw: []
    )

    def boom(_repo):
        raise RuntimeError("metric-boom")

    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", boom)
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        tmp_path, "plain message", log, watchdog.MaximReminderGate()
    )

    assert "spice metrics supervisor error: metric-boom" in log.getvalue()


def test_maxim_failure_is_surfaced_and_supervisor_survives(
    tmp_path, monkeypatch, empty_ack_summary
):
    monkeypatch.setattr(watchdog, "create_inline_tasks", lambda _repo, _text, _log: [])
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)

    def boom(_repo, _text, **_kw):
        raise RuntimeError("maxim-boom")

    monkeypatch.setattr(watchdog, "publish_maxim_hits_as_inbox", boom)
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        tmp_path, "plain message", log, watchdog.MaximReminderGate()
    )

    assert "spice maxim supervisor error: maxim-boom" in log.getvalue()


def test_nack_archival_failure_is_surfaced_and_supervisor_survives(
    tmp_path, monkeypatch
):
    def boom(_repo, _text):
        raise RuntimeError("nack-boom")

    monkeypatch.setattr(watchdog, "summarize_nack_archival", boom)
    monkeypatch.setattr(
        watchdog,
        "summarize_ack_archival",
        lambda _repo, _text: SimpleNamespace(
            archived=[], already_acked=[], unmatched=[], noop=False
        ),
    )
    monkeypatch.setattr(watchdog, "create_inline_tasks", lambda _repo, _text, _log: [])
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog, "publish_maxim_hits_as_inbox", lambda _repo, _text, **_kw: []
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        tmp_path, "plain message", log, watchdog.MaximReminderGate()
    )

    assert "spice nack archival supervisor error: nack-boom" in log.getvalue()


def test_ack_archival_failure_is_surfaced_and_supervisor_survives(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        watchdog,
        "summarize_nack_archival",
        lambda _repo, _text: SimpleNamespace(
            refused=[],
            already_refused=[],
            already_acked=[],
            unmatched=[],
            reasonless=[],
        ),
    )

    def boom(_repo, _text):
        raise RuntimeError("ack-boom")

    monkeypatch.setattr(watchdog, "summarize_ack_archival", boom)
    monkeypatch.setattr(watchdog, "create_inline_tasks", lambda _repo, _text, _log: [])
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog, "publish_maxim_hits_as_inbox", lambda _repo, _text, **_kw: []
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        tmp_path, "plain message", log, watchdog.MaximReminderGate()
    )

    assert "spice ack archival supervisor error: ack-boom" in log.getvalue()


def test_side_channel_feedback_failure_is_swallowed(tmp_path, monkeypatch):
    def boom(_repo, _kind, **_fields):
        raise RuntimeError("feedback-boom")

    monkeypatch.setattr(watchdog, "publish_side_channel_feedback", boom)
    log = io.StringIO()

    watchdog.publish_supervisor_feedback(tmp_path, log, "task.created", handles=["h"])

    assert "spice side-channel feedback error: feedback-boom" in log.getvalue()


def test_initial_side_channel_payload_render_failure_is_surfaced(tmp_path, monkeypatch):
    def boom(_repo):
        raise RuntimeError("render-boom")

    monkeypatch.setattr(sidechannel, "render_side_channel_payload", boom)
    stderr = io.StringIO()

    wrap.emit_initial_side_channel_payload(tmp_path, stderr=stderr)

    assert "spice side-channel unavailable: render-boom" in stderr.getvalue()
