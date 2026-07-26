"""Canonical steering/ACK facts projected into Serve directive metrics."""

from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.mail.ackstate import (
    directive_history_records_from_database,
)
from spice.serve.directivestats import DirectiveTotals
from spice.serve.team.store import ServeTeamStore
from tests.test_directivefacthelpers import (
    complete_directive_fact,
    publish_directive_fact,
)

DIRECTIVE_SENT_AT = 100.0
DIRECTIVE_ACKED_AT = 140.0


def _store(tmp_path):
    return ServeTeamStore(path=tmp_path / "teams.sqlite3")


def test_each_directive_counts_once_and_ack_is_a_subset_of_send(tmp_path):
    store = _store(tmp_path)
    # Three directives sent to one agent on one team; two acknowledged. It does
    # not matter that an agent might ack several keys in one message — each key
    # was individually sent, so each is its own send, and each ack flips one.
    for key in ("k1", "k2", "k3"):
        publish_directive_fact(
            store.directive_state_path, key, agent_id="agent-a", team_id="team-1"
        )
    assert complete_directive_fact(store.directive_state_path, "k1") is True
    assert complete_directive_fact(store.directive_state_path, "k3") is True

    totals = store.directive_totals_for_agents(["agent-a"])
    assert totals == DirectiveTotals(sends=3, acked=2)
    assert totals.acked <= totals.sends


def test_resending_the_same_key_does_not_double_count(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path,
        "k1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=100,
    )
    publish_directive_fact(
        store.directive_state_path,
        "k1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=100,
    )

    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=1, acked=0
    )


def test_acking_is_idempotent_and_unknown_keys_are_noops(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path, "k1", agent_id="agent-a", team_id="team-1"
    )

    assert complete_directive_fact(store.directive_state_path, "k1") is True
    assert complete_directive_fact(store.directive_state_path, "k1") is False
    assert complete_directive_fact(store.directive_state_path, "nope") is False

    totals = store.directive_totals_for_agents(["agent-a"])
    assert totals == DirectiveTotals(sends=1, acked=1)
    assert totals.acked <= totals.sends


def test_totals_sum_across_agents_and_capture_team(tmp_path):
    store = _store(tmp_path)
    # agent-a sent two directives while on different teams (team-at-capture is
    # recorded per row); agent-b one. Per-agent totals sum across teams.
    for key, agent_id, team_id in (
        ("a1", "agent-a", "team-1"),
        ("a2", "agent-a", "team-2"),
        ("b1", "agent-b", "team-1"),
    ):
        publish_directive_fact(
            store.directive_state_path,
            key,
            agent_id=agent_id,
            team_id=team_id,
        )
    complete_directive_fact(store.directive_state_path, "a1")
    complete_directive_fact(store.directive_state_path, "b1")

    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=2, acked=1
    )
    assert store.directive_totals_for_agents(["agent-a", "agent-b"]) == DirectiveTotals(
        sends=3, acked=2
    )
    assert store.directive_totals_for_agents([]) == DirectiveTotals(sends=0, acked=0)


def test_directive_rows_are_the_stable_series_with_team_at_capture(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path,
        "a1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=DIRECTIVE_SENT_AT,
    )
    complete_directive_fact(
        store.directive_state_path, "a1", acked_at=DIRECTIVE_ACKED_AT
    )

    row = directive_history_records_from_database(store.directive_state_path)[0]

    assert (row.target_actor, row.team_id) == ("agent-a", "team-1")
    assert row.sent_at == DIRECTIVE_SENT_AT
    assert row.disposition == "acked"
    assert row.acknowledged_at == DIRECTIVE_ACKED_AT


def test_same_key_with_changed_publication_provenance_fails_actionably(tmp_path):
    store = _store(tmp_path)
    publish_directive_fact(
        store.directive_state_path,
        "k1",
        agent_id="agent-a",
        team_id="team-1",
        sent_at=100,
    )

    with pytest.raises(SpiceError, match="immutable publication provenance differs"):
        publish_directive_fact(
            store.directive_state_path,
            "k1",
            agent_id="agent-b",
            team_id="team-2",
            sent_at=101,
        )


def test_pending_refused_ack_latency_and_restart_share_one_history(tmp_path):
    store = _store(tmp_path)
    for key, sent_at in (("pending", 100), ("acked", 110), ("refused", 120)):
        publish_directive_fact(
            store.directive_state_path,
            key,
            agent_id="agent-a",
            team_id="team-a",
            sent_at=sent_at,
        )
    complete_directive_fact(store.directive_state_path, "acked", acked_at=150)
    complete_directive_fact(
        store.directive_state_path,
        "refused",
        acked_at=170,
        disposition="refused",
        ack_text="NACK refused: conflicts with policy",
        ack_content="conflicts with policy",
    )
    assert (
        complete_directive_fact(store.directive_state_path, "acked", acked_at=180)
        is False
    )

    restarted = _store(tmp_path)
    summary = restarted.directive_lifecycle_summary_for_agents(["agent-a"])
    assert (
        summary.sends,
        summary.acked,
        summary.refused,
        summary.pending,
        summary.minimum_latency_seconds,
        summary.maximum_latency_seconds,
    ) == (3, 1, 1, 1, 40.0, 50.0)
