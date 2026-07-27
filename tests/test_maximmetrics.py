"""Durable maxim metric event storage."""

from __future__ import annotations

import sqlite3

import pytest

from spice.agent import maximmetrics, watchdog
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
from spice.agent.identity import ambient_thread_id
from spice.agent.maximcli import (
    SCOPE_DECISION_EVIDENCE_ROW,
    render_maxim_report,
    run_maxim_report_cli,
)
from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MAXIM_EVENT_GATE_SUPPRESSED,
    MAXIM_EVENT_JUDGED_CONFIRMED,
    MAXIM_EVENT_JUDGED_REJECTED,
    MAXIM_EVENT_PUBLISHED,
    MAXIM_METRICS_SCHEMA_VERSION,
    MAXIM_METRICS_TABLE_SQL,
    MaximMetricCounts,
    MaximMetricEventWrite,
    MaximRecurrenceCounts,
    latest_fire_bag_name,
    maxim_metric_counts,
    maxim_metric_records,
    maxim_metrics_database_path,
    maxim_recurrence_counts,
    maxim_recurrence_inputs,
    record_maxim_metric_events,
)
from spice.agent.maxims import MaximVerdict
from spice.cli.parser import build_parser
from spice.config import edit, layers, values
from spice.errors import SpiceError
from spice.sqliteconnection import sqlite_connection
from tests.test_reposcaffolding import init_quiet_empty_repo as _init_repo


def _write_maxim_config(repo, *, name: str = "alpha") -> None:
    (repo / "pyproject.toml").write_text(
        f"""
[tool.spice.maxims.{name}]
words = ["{name}"]
message = "{name.upper()} reminder."
""".lstrip(),
        encoding="utf-8",
    )


def _enable_maxim_adjudication(repo) -> None:
    edit.set_scope_section(
        repo,
        layers.WORKTREE_SOURCE,
        values.JUDGE_KEY,
        {values.JUDGE_ENABLED_KEY: True},
    )


def _judge_verdict(*, agrees: bool) -> MaximVerdict:
    answer = "YES" if agrees else "NO"
    return MaximVerdict(
        maxim="",
        statement="",
        prompt="",
        answer=answer,
        attempts=(answer,),
    )


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
                reminder_key="1k9h3MxX",
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
        (count.bag_name, count.driver_name, count.thread_id): count
        for count in maxim_metric_counts(repo)
    }

    assert maxim_metrics_database_path(repo).is_file()
    assert counts[("polling", "codex", "thread-a")] == MaximMetricCounts(
        bag_name="polling",
        driver_name="codex",
        thread_id="thread-a",
        fire_count=2,
        judged_confirmed_count=1,
        judged_rejected_count=1,
        gate_suppressed_count=1,
        published_count=1,
    )
    assert counts[("fallbacks", "claude", "thread-b")] == MaximMetricCounts(
        bag_name="fallbacks",
        driver_name="claude",
        thread_id="thread-b",
        fire_count=1,
        judged_confirmed_count=0,
        judged_rejected_count=0,
        gate_suppressed_count=0,
        published_count=0,
    )
    with sqlite_connection(maxim_metrics_database_path(repo)) as connection:
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == (
            MAXIM_METRICS_SCHEMA_VERSION
        )


def test_maxim_store_stamps_the_exact_unversioned_source_without_losing_rows(
    tmp_path,
):
    repo = _init_repo(tmp_path / "repo")
    path = maxim_metrics_database_path(repo)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite_connection(path) as connection:
        connection.execute(MAXIM_METRICS_TABLE_SQL)
        connection.execute(maximmetrics.MAXIM_METRICS_EVENT_INDEX_SQL)
        connection.execute(maximmetrics.MAXIM_METRICS_RECURRENCE_INDEX_SQL)
        connection.execute(maximmetrics.MAXIM_METRICS_FIRE_RECENCY_INDEX_SQL)
        connection.execute(
            "INSERT INTO maxim_metric_events "
            "(occurred_at, event_type, bag_name, driver_name) VALUES (?, ?, ?, ?)",
            (1.0, MAXIM_EVENT_FIRE, "before", "codex"),
        )
        assert int(connection.execute("PRAGMA user_version").fetchone()[0]) == 0

    record_maxim_metric_events(
        repo,
        [MaximMetricEventWrite(MAXIM_EVENT_FIRE, "after", "codex")],
        now=2.0,
    )

    with sqlite_connection(path) as connection:
        version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        rows = connection.execute(
            "SELECT occurred_at, bag_name FROM maxim_metric_events ORDER BY id"
        ).fetchall()

    assert version == MAXIM_METRICS_SCHEMA_VERSION
    assert rows == [(1.0, "before"), (2.0, "after")]


def _maxim_database_snapshot(path):
    with sqlite_connection(path) as connection:
        return (
            int(connection.execute("PRAGMA user_version").fetchone()[0]),
            str(connection.execute("PRAGMA journal_mode").fetchone()[0]),
            tuple(connection.iterdump()),
        )


def test_maxim_store_refuses_an_unknown_unversioned_shape_without_mutation(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    path = maxim_metrics_database_path(repo)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite_connection(path) as connection:
        connection.execute(
            "CREATE TABLE maxim_metric_events "
            "(id INTEGER PRIMARY KEY, occurred_at REAL NOT NULL, payload TEXT NOT NULL)"
        )
        connection.execute(
            "INSERT INTO maxim_metric_events (occurred_at, payload) VALUES (1.0, 'old')"
        )
    before = _maxim_database_snapshot(path)

    with pytest.raises(SpiceError, match="unsupported table shape"):
        record_maxim_metric_events(
            repo,
            [MaximMetricEventWrite(MAXIM_EVENT_FIRE, "new", "codex")],
        )

    assert _maxim_database_snapshot(path) == before


def test_maxim_store_refuses_a_newer_version_without_mutation(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    path = maxim_metrics_database_path(repo)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite_connection(path) as connection:
        connection.execute(MAXIM_METRICS_TABLE_SQL)
        connection.execute(f"PRAGMA user_version = {MAXIM_METRICS_SCHEMA_VERSION + 1}")
    before = _maxim_database_snapshot(path)

    with pytest.raises(SpiceError, match="newer schema version"):
        record_maxim_metric_events(
            repo,
            [MaximMetricEventWrite(MAXIM_EVENT_FIRE, "new", "codex")],
        )

    assert _maxim_database_snapshot(path) == before


def test_maxim_store_revalidates_a_cached_path_after_replacement(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    record_maxim_metric_events(
        repo,
        [MaximMetricEventWrite(MAXIM_EVENT_FIRE, "before", "codex")],
    )
    path = maxim_metrics_database_path(repo)
    replacement = path.with_name("replacement.sqlite3")
    with sqlite_connection(replacement) as connection:
        connection.execute(MAXIM_METRICS_TABLE_SQL)
        connection.execute(f"PRAGMA user_version = {MAXIM_METRICS_SCHEMA_VERSION + 1}")
    replacement.replace(path)
    before = _maxim_database_snapshot(path)

    with pytest.raises(SpiceError, match="changed to newer schema version"):
        record_maxim_metric_events(
            repo,
            [MaximMetricEventWrite(MAXIM_EVENT_FIRE, "after", "codex")],
        )

    assert _maxim_database_snapshot(path) == before


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
                reminder_key="1k9h3MxX",
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
            "1k9h3MxX",
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


def test_maxim_recurrence_counts_only_later_fires_inside_horizon(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    shared = {
        "bag_name": "polling",
        "driver_name": "codex",
        "thread_id": "thread-a",
        "trigger_family": "poll-loop",
    }

    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                statement="Before the reminder, I will poll.",
                **shared,
            )
        ],
        now=100.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_PUBLISHED,
                reminder_key="1k9h3MxX",
                reminder_body="[MAXIM] Use a watcher.",
                **shared,
            )
        ],
        now=110.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                statement="After the reminder, I still poll.",
                **shared,
            )
        ],
        now=120.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                statement="After the horizon, I poll again.",
                **shared,
            )
        ],
        now=200.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="polling",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="other-loop",
                statement="A different trigger family is separate.",
            )
        ],
        now=121.0,
    )

    assert maxim_recurrence_counts(repo, horizon_seconds=30.0) == [
        MaximRecurrenceCounts(
            bag_name="polling",
            driver_name="codex",
            thread_id="thread-a",
            trigger_family="poll-loop",
            recurrence_count=1,
        )
    ]


def _record_metric_event(repo, event_type: str, *, now: float, **fields) -> None:
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                event_type,
                **fields,
            )
        ],
        now=now,
    )


def _write_report_metric_fixture(repo) -> None:
    shared = {
        "bag_name": "polling",
        "driver_name": "codex",
        "thread_id": "thread-a",
        "trigger_family": "poll-loop",
    }
    _record_metric_event(
        repo,
        MAXIM_EVENT_FIRE,
        statement="Before the reminder, I will poll.",
        now=100.0,
        **shared,
    )
    _record_metric_event(
        repo,
        MAXIM_EVENT_PUBLISHED,
        reminder_key="1k9h3MxX",
        reminder_body="[MAXIM] Use a watcher.",
        now=110.0,
        **shared,
    )
    _record_metric_event(
        repo,
        MAXIM_EVENT_FIRE,
        statement="After the reminder, I still poll.",
        now=120.0,
        **shared,
    )
    _record_metric_event(
        repo,
        MAXIM_EVENT_JUDGED_CONFIRMED,
        statement="The recurrence was a confirmed violation.",
        now=121.0,
        **shared,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE,
                bag_name="fallbacks",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="fallback-loop",
                statement="A different bag fired in the same driver/thread.",
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_JUDGED_REJECTED,
                bag_name="fallbacks",
                driver_name="codex",
                thread_id="thread-a",
                trigger_family="fallback-loop",
                statement="The fallback hit was compliant.",
            ),
        ],
        now=130.0,
    )


def test_maxim_report_uses_recurrence_counts(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    _write_report_metric_fixture(repo)

    lines = render_maxim_report(repo).splitlines()
    rows = {line.split()[0]: line.split() for line in lines[2:-1]}

    assert lines[1].split() == [
        "bag",
        "driver",
        "thread",
        "fire_rate",
        "confirm_rate",
        "recurrence",
        "fire",
        "confirmed",
        "rejected",
        "suppressed",
        "published",
        "recur",
    ]
    assert rows["fallbacks"] == [
        "fallbacks",
        "codex",
        "thread-a",
        "33%",
        "0%",
        "-",
        "1",
        "0",
        "1",
        "0",
        "0",
        "0",
    ]
    assert rows["polling"] == [
        "polling",
        "codex",
        "thread-a",
        "67%",
        "100%",
        "100%",
        "2",
        "1",
        "0",
        "0",
        "1",
        "1",
    ]
    assert lines[-1] == SCOPE_DECISION_EVIDENCE_ROW


def test_maxim_report_empty_history_points_to_scope_evidence(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    lines = render_maxim_report(repo).splitlines()

    assert lines == [
        "maxim metric events: 0",
        SCOPE_DECISION_EVIDENCE_ROW,
    ]


def test_maxim_report_parser_wires_report_action():
    args = build_parser().parse_args(["maxim", "report"])

    assert args.maxim_action == "report"
    assert args.func is run_maxim_report_cli


def test_watchdog_records_published_violation_metrics(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    _write_maxim_config(repo)
    _enable_maxim_adjudication(repo)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(
        watchdog,
        "evaluate_maxim_any_violation",
        lambda _maxim, _statement: _judge_verdict(agrees=False),
    )

    watchdog.publish_maxim_hits_as_inbox(
        repo,
        "alpha appears in this assistant message",
        reminder_gate=watchdog.MaximReminderGate(),
    )
    count = maxim_metric_counts(repo)[0]

    assert count == MaximMetricCounts(
        bag_name="alpha",
        driver_name="codex",
        thread_id=ambient_thread_id() or "",
        fire_count=1,
        judged_confirmed_count=1,
        judged_rejected_count=0,
        gate_suppressed_count=0,
        published_count=1,
    )


def test_watchdog_records_judged_rejection_metrics(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    _write_maxim_config(repo)
    _enable_maxim_adjudication(repo)
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(
        watchdog,
        "evaluate_maxim_any_violation",
        lambda _maxim, _statement: _judge_verdict(agrees=True),
    )

    watchdog.publish_maxim_hits_as_inbox(
        repo,
        "alpha appears in compliant assistant prose",
        reminder_gate=watchdog.MaximReminderGate(),
    )
    count = maxim_metric_counts(repo)[0]

    assert count == MaximMetricCounts(
        bag_name="alpha",
        driver_name="codex",
        thread_id=ambient_thread_id() or "",
        fire_count=1,
        judged_confirmed_count=0,
        judged_rejected_count=1,
        gate_suppressed_count=0,
        published_count=0,
    )


def test_watchdog_records_gate_suppressed_metrics(tmp_path, monkeypatch):
    repo = _init_repo(tmp_path / "repo")
    _write_maxim_config(repo)
    _enable_maxim_adjudication(repo)
    gate = watchdog.MaximReminderGate()
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)
    monkeypatch.setattr(
        watchdog,
        "evaluate_maxim_any_violation",
        lambda _maxim, _statement: _judge_verdict(agrees=False),
    )

    watchdog.publish_maxim_hits_as_inbox(
        repo,
        "alpha appears once",
        reminder_gate=gate,
    )
    watchdog.publish_maxim_hits_as_inbox(
        repo,
        "alpha appears again before compaction",
        reminder_gate=gate,
    )
    count = maxim_metric_counts(repo)[0]

    assert count == MaximMetricCounts(
        bag_name="alpha",
        driver_name="codex",
        thread_id=ambient_thread_id() or "",
        fire_count=2,
        judged_confirmed_count=1,
        judged_rejected_count=0,
        gate_suppressed_count=1,
        published_count=1,
    )


def test_latest_fire_bag_name_returns_most_recent_fire_from_seeded_store(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE, bag_name="polling", driver_name="codex"
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE, bag_name="fallbacks", driver_name="claude"
            ),
        ],
        now=100.0,
    )
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE, bag_name="stalls", driver_name="codex"
            )
        ],
        now=150.0,
    )
    # A newer non-fire event must not win: only fires are considered.
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_PUBLISHED,
                bag_name="polling",
                driver_name="codex",
                reminder_key="1k9h3MxX",
                reminder_body="[MAXIM] Use a watcher.",
            )
        ],
        now=200.0,
    )

    assert latest_fire_bag_name(repo) == "stalls"


def test_latest_fire_bag_name_breaks_ties_by_insert_order(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE, bag_name="earlier", driver_name="codex"
            ),
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE, bag_name="later", driver_name="codex"
            ),
        ],
        now=100.0,
    )

    # Same occurred_at; the row inserted later (higher id) is the most recent,
    # matching the reversed (occurred_at ASC, id ASC) scan this replaces.
    assert latest_fire_bag_name(repo) == "later"


def test_latest_fire_bag_name_is_empty_without_fires(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    # No store file yet.
    assert latest_fire_bag_name(repo) == ""

    # A store with only non-fire events still yields no fire bag.
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_PUBLISHED,
                bag_name="polling",
                driver_name="codex",
                reminder_key="1k9h3MxX",
                reminder_body="[MAXIM] Use a watcher.",
            )
        ],
        now=100.0,
    )

    assert latest_fire_bag_name(repo) == ""


def test_maxim_store_opens_write_and_read_connections_in_wal(tmp_path):
    repo = _init_repo(tmp_path / "repo")
    record_maxim_metric_events(
        repo,
        [
            MaximMetricEventWrite(
                MAXIM_EVENT_FIRE, bag_name="polling", driver_name="codex"
            )
        ],
        now=100.0,
    )
    path = maxim_metrics_database_path(repo)

    with sqlite_connection(path) as connection:
        mode = connection.execute("PRAGMA journal_mode").fetchone()[0]

    assert mode.lower() == "wal"
    # The read path returns the persisted fire while operating over the WAL store.
    assert latest_fire_bag_name(repo) == "polling"


def test_maxim_schema_initializes_once_per_path_across_repeated_writes(
    tmp_path, monkeypatch
):
    repo = _init_repo(tmp_path / "repo")
    real_ensure_schema = maximmetrics._ensure_schema
    ddl_runs: list[int] = []

    def counting_ensure_schema(connection):
        ddl_runs.append(1)
        return real_ensure_schema(connection)

    monkeypatch.setattr(maximmetrics, "_ensure_schema", counting_ensure_schema)

    for index in range(3):
        record_maxim_metric_events(
            repo,
            [
                MaximMetricEventWrite(
                    MAXIM_EVENT_FIRE, bag_name=f"bag{index}", driver_name="codex"
                )
            ],
            now=100.0 + index,
        )

    # The DDL sweep runs once for the path, not on every write.
    assert len(ddl_runs) == 1


def _maxim_write_outcome_with_reader_open(db_path, *, do_write):
    """Hold a reader's read transaction open, attempt a write, report outcome.

    In the default rollback journal the reader's SHARED lock and the writer's
    commit-time promotion to EXCLUSIVE are mutually exclusive on a BUSY that
    `busy_timeout` cannot retry away, so the write raises "database is locked".
    In WAL the reader's snapshot and the single writer coexist and the write
    commits.
    """
    reader = sqlite3.connect(db_path)
    reader.isolation_level = None
    try:
        reader.execute("PRAGMA busy_timeout = 200")
        reader.execute("BEGIN")
        reader.execute("SELECT count(*) FROM maxim_metric_events").fetchall()
        try:
            do_write()
            return "committed"
        except sqlite3.OperationalError as exc:
            return f"locked:{exc}"
    finally:
        reader.rollback()
        reader.close()


def test_wal_maxim_write_commits_while_a_reader_is_open_unlike_rollback(tmp_path):
    repo = _init_repo(tmp_path / "repo")

    # Pre-fix shape: rollback journal with the schema DDL re-run on every write.
    rollback_db = maxim_metrics_database_path(repo).parent / "rollback.sqlite3"
    rollback_db.parent.mkdir(parents=True, exist_ok=True)
    with sqlite_connection(rollback_db, wal=False) as connection:
        connection.execute(MAXIM_METRICS_TABLE_SQL)

    def rollback_write():
        with sqlite_connection(
            rollback_db, busy_timeout_ms=200, wal=False
        ) as connection:
            connection.execute(MAXIM_METRICS_TABLE_SQL)
            connection.execute(
                "INSERT INTO maxim_metric_events "
                "(occurred_at, event_type, bag_name, driver_name) VALUES (?, ?, ?, ?)",
                (1.0, MAXIM_EVENT_FIRE, "rollback", "codex"),
            )

    rollback_outcome = _maxim_write_outcome_with_reader_open(
        rollback_db, do_write=rollback_write
    )

    # Fixed shape: WAL store initialized once, exercised through the real API.
    record_maxim_metric_events(
        repo,
        [MaximMetricEventWrite(MAXIM_EVENT_FIRE, bag_name="warm", driver_name="codex")],
        now=100.0,
    )
    wal_db = maxim_metrics_database_path(repo)

    def wal_write():
        record_maxim_metric_events(
            repo,
            [
                MaximMetricEventWrite(
                    MAXIM_EVENT_FIRE, bag_name="live", driver_name="codex"
                )
            ],
            now=101.0,
        )

    wal_outcome = _maxim_write_outcome_with_reader_open(wal_db, do_write=wal_write)

    assert wal_outcome == "committed"
    assert rollback_outcome.startswith("locked")
    # The WAL write committed while the reader was open and is the newest fire.
    assert latest_fire_bag_name(repo) == "live"
