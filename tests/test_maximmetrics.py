"""Durable maxim metric event storage."""

from __future__ import annotations

import subprocess

from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MAXIM_EVENT_GATE_SUPPRESSED,
    MAXIM_EVENT_JUDGED_CONFIRMED,
    MAXIM_EVENT_JUDGED_REJECTED,
    MAXIM_EVENT_PUBLISHED,
    MaximMetricCounts,
    MaximMetricEventWrite,
    maxim_metric_counts,
    maxim_metric_records,
    maxim_metrics_database_path,
    maxim_recurrence_inputs,
    record_maxim_metric_events,
)


def _init_repo(path):
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)
    return path


def test_maxim_metric_store_persists_aggregate_counts_after_reload(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="polling",
                statement="I will sleep and poll.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_JUDGED_CONFIRMED,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="polling",
                statement="I will sleep and poll.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_JUDGED_REJECTED,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="polling",
                statement="I will use a watcher.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_GATE_SUPPRESSED,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="polling",
                statement="I will sleep and poll again.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_PUBLISHED,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="polling",
                reminder_key="20260703T010000000000Z",
                reminder_body="[MAXIM] Use a watcher.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="fallbacks",
                driver_name="claude",
                thread_id="thread-b",
                trigger_family="fallbacks",
                statement="I will fall back quietly.",
            ),
        ],
        now=1000.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="polling",
                statement="I will poll once more.",
            )
        ],
        now=1001.0,
    )

    counts = {
        (count.bag_name, count.driver_name): count
        for count in maxim_metric_counts(repo)
    }

    assert maxim_metrics_database_path(repo).is_file()
    assert counts[("polling", "codex")] == MaximMetricCounts(
        bag_name="polling",
        driver_name="codex",
        fire_count=2,
        judged_confirmed_count=1,
        judged_rejected_count=1,
        gate_suppressed_count=1,
        published_count=1,
    )
    assert counts[("fallbacks", "claude")] == MaximMetricCounts(
        bag_name="fallbacks",
        driver_name="claude",
        fire_count=1,
        judged_confirmed_count=0,
        judged_rejected_count=0,
        gate_suppressed_count=0,
        published_count=0,
    )


def test_maxim_metric_recurrence_inputs_keep_trigger_and_reminder_context(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="poll-loop",
                statement="I will poll for the file.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_PUBLISHED,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="poll-loop",
                reminder_key="20260703T010000000000Z",
                reminder_body="[MAXIM] Use a watcher.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="poll-loop",
                statement="Later prose still says poll.",
            ),
        ],
        now=2000.0,
    )

    records = maxim_metric_records(repo)
    recurrence_inputs = maxim_recurrence_inputs(repo)

    assert [record.event_type for record in records] == [
        MAXIM_EVENT_FIRE,
        MAXIM_EVENT_PUBLISHED,
        MAXIM_EVENT_FIRE,
    ]
    assert [
        (
            item.event_type,
            item.bag_name,
            item.driver_name,
            item.thread_id,
            item.trigger_family,
            item.statement,
            item.reminder_key,
            item.reminder_body,
        )
        for item in recurrence_inputs
    ] == [
        (
            MAXIM_EVENT_FIRE,
            "polling",
            "codex",
            "thread-a",
            "poll-loop",
            "I will poll for the file.",
            "",
            "",
        ),
        (
            MAXIM_EVENT_PUBLISHED,
            "polling",
            "codex",
            "thread-a",
            "poll-loop",
            "",
            "20260703T010000000000Z",
            "[MAXIM] Use a watcher.",
        ),
        (
            MAXIM_EVENT_FIRE,
            "polling",
            "codex",
            "thread-a",
            "poll-loop",
            "Later prose still says poll.",
            "",
            "",
        ),
    ]
