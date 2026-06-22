"""Per-directive send/ack accounting (acked is a subset of sends)."""

from __future__ import annotations

from spice.serve.directivestats import DirectiveTotals
from spice.serve.team.store import ServeTeamStore

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
        store.record_directive_sent(key, agent_id="agent-a", team_id="team-1")
    assert store.mark_directive_acked("k1") is True
    assert store.mark_directive_acked("k3") is True

    totals = store.directive_totals_for_agents(["agent-a"])
    assert totals == DirectiveTotals(sends=3, acked=2)
    assert totals.acked <= totals.sends


def test_resending_the_same_key_does_not_double_count(tmp_path):
    store = _store(tmp_path)
    store.record_directive_sent("k1", agent_id="agent-a", team_id="team-1")
    store.record_directive_sent("k1", agent_id="agent-a", team_id="team-1")

    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=1, acked=0
    )


def test_acking_is_idempotent_and_unknown_keys_are_noops(tmp_path):
    store = _store(tmp_path)
    store.record_directive_sent("k1", agent_id="agent-a", team_id="team-1")

    assert store.mark_directive_acked("k1") is True
    assert store.mark_directive_acked("k1") is False  # already acked
    assert store.mark_directive_acked("nope") is False  # never sent

    totals = store.directive_totals_for_agents(["agent-a"])
    assert totals == DirectiveTotals(sends=1, acked=1)
    assert totals.acked <= totals.sends


def test_totals_sum_across_agents_and_capture_team(tmp_path):
    store = _store(tmp_path)
    # agent-a sent two directives while on different teams (team-at-capture is
    # recorded per row); agent-b one. Per-agent totals sum across teams.
    store.record_directive_sent("a1", agent_id="agent-a", team_id="team-1")
    store.record_directive_sent("a2", agent_id="agent-a", team_id="team-2")
    store.record_directive_sent("b1", agent_id="agent-b", team_id="team-1")
    store.mark_directive_acked("a1")
    store.mark_directive_acked("b1")

    assert store.directive_totals_for_agents(["agent-a"]) == DirectiveTotals(
        sends=2, acked=1
    )
    assert store.directive_totals_for_agents(["agent-a", "agent-b"]) == DirectiveTotals(
        sends=3, acked=2
    )
    assert store.directive_totals_for_agents([]) == DirectiveTotals(sends=0, acked=0)


def test_directive_rows_are_the_stable_series_with_team_at_capture(tmp_path):
    store = _store(tmp_path)
    store.record_directive_sent(
        "a1", agent_id="agent-a", team_id="team-1", sent_at=DIRECTIVE_SENT_AT
    )
    store.mark_directive_acked("a1", acked_at=DIRECTIVE_ACKED_AT)

    with store.connect() as connection:
        row = connection.execute(
            "SELECT agent_id, team_id, sent_at, acked, acked_at "
            "FROM directives WHERE directive_key = ?",
            ("a1",),
        ).fetchone()

    assert (row["agent_id"], row["team_id"]) == ("agent-a", "team-1")
    assert row["sent_at"] == DIRECTIVE_SENT_AT
    assert int(row["acked"]) == 1
    assert row["acked_at"] == DIRECTIVE_ACKED_AT
