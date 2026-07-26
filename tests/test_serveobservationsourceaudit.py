"""Static guardrails for the terminal Serve observation architecture."""

from __future__ import annotations

import re
from pathlib import Path

SERVE_ROOT = Path(__file__).parents[1] / "spice" / "serve"


def _source(relative: str) -> str:
    return (SERVE_ROOT / relative).read_text(encoding="utf-8")


def _serve_python() -> str:
    return "\n".join(
        path.read_text(encoding="utf-8") for path in sorted(SERVE_ROOT.rglob("*.py"))
    )


def test_retired_mixed_store_shapes_have_no_production_path():
    source = _serve_python()

    assert "task_events" not in source
    assert "LEGACY_TEAM_SCHEMA_FINGERPRINT" not in source
    assert "migrate_serve_directive_history" not in source
    assert not re.search(
        r"(?:INSERT|UPDATE|DELETE|DROP)[^\n]*directive_totals",
        source,
        flags=re.IGNORECASE,
    )


def test_renewal_history_is_append_only_and_authority_has_no_drop_fallback():
    renewals = _source("team/renewals.py")
    authority = _source("team/store.py")

    assert not re.search(
        r"(?:UPDATE|DELETE)\s+(?:FROM\s+)?events",
        renewals,
        flags=re.IGNORECASE,
    )
    assert "DROP TABLE" not in authority
    assert "_drop_all" not in authority


def test_serve_activity_attribution_uses_typed_events_and_family_reads():
    ingestion = _source("metrics.py")
    metric_queries = _source("team/metrics.py")

    assert "TranscriptEventReader" in ingestion
    assert "json.loads" not in ingestion
    assert "projections.connect()" not in ingestion
    assert "projections.connect()" not in metric_queries
    assert "projections.read(AGENT_ACTIVITY)" in metric_queries
    assert "projections.write(AGENT_ACTIVITY)" in metric_queries


def test_task_lifecycle_query_has_no_projection_or_duplicate_store_read():
    metric_queries = _source("team/metrics.py")
    lifecycle = metric_queries.split("    def task_lifecycle_series(", 1)[1].split(
        "    def task_distribution_series(", 1
    )[0]

    assert "team_task_transitions" in lifecycle
    assert "self.projections" not in lifecycle
    assert "AGENT_ACTIVITY" not in lifecycle
    assert "directive_state_path" not in lifecycle
