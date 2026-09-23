"""Mixed-generation markers across ACKs, inbox persistence, and task graphs."""

import subprocess

import pytest

from spice.mail.ackarchive import summarize_ack_archival, summarize_nack_archival
from spice.mail.ackgrammar import split_keyed_response
from spice.mail.ackstate import ack_state_records
from spice.mail.inbox import (
    collect_inbox_items,
    collect_pending_inbox_entries,
    mint_inbox_key,
    write_inbox_item,
)
from spice.paths import shared_state_path
from spice.tasks import create, graph, identity

LEGACY = "1dzGkSrJ"
CURRENT = "bnZCRxw3h"
EPOCH_MICROS = 1_700_000_000_000_000


@pytest.mark.parametrize("token", ["ACK", "NACK"])
@pytest.mark.parametrize("suffix", ["", "-2"])
def test_keyed_headers_and_task_origins_accept_both_widths(token, suffix):
    keys = [stamp + suffix for stamp in (LEGACY, CURRENT)]
    preamble, responses = split_keyed_response(f"{token} {' '.join(keys)}: handled.")
    assert preamble == ""
    assert [response.keys for response in responses] == [tuple(keys)]
    assert [response.content for response in responses] == ["handled."]
    for key in keys:
        assert create.validated_task_origin(f"ack:{key}") == f"ack:{key}"
        assert create.validated_task_origin(key) == f"ack:{key}"


@pytest.mark.parametrize("key", ["0000000", CURRENT + "0", CURRENT + "A", "00000000A"])
def test_ack_token_boundaries_do_not_accept_a_valid_prefix(key):
    text = f"ACK {key}: prose."
    assert split_keyed_response(text) == (text, [])


def test_mixed_width_inbox_orders_and_retires_original_keys(tmp_path):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    earlier = CURRENT
    later = "1dzGkSrK-2"
    write_inbox_item(tmp_path, f"{later}.txt", "later legacy steering")
    write_inbox_item(tmp_path, f"{earlier}.txt", "earlier current steering")
    expected = [f"{earlier}.txt", f"{later}.txt"]
    assert [item.name for item in collect_inbox_items(tmp_path)] == expected
    assert [item.name for item in collect_pending_inbox_entries(tmp_path)] == expected
    assert summarize_ack_archival(tmp_path, f"ACK {earlier}: completed.").archived == [
        earlier
    ]
    assert summarize_nack_archival(tmp_path, f"NACK {later}: declined.").refused == [
        later
    ]
    assert {record.key for record in ack_state_records(tmp_path)} == {earlier, later}
    assert collect_inbox_items(tmp_path) == []


@pytest.mark.parametrize("previous", [LEGACY, CURRENT])
def test_inbox_sequence_normalizes_previous_generation_and_clock_rollback(
    tmp_path, monkeypatch, previous
):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    sequence = shared_state_path(tmp_path, "inbox-key-sequence")
    sequence.parent.mkdir(parents=True, exist_ok=True)
    sequence.write_text(previous + "\n")
    monkeypatch.setattr(identity, "epoch_micros", lambda: EPOCH_MICROS - 1)
    assert mint_inbox_key(tmp_path) == "bnZCRxw3j"
    assert mint_inbox_key(tmp_path) == "bnZCRxw3k"
    assert sequence.read_text() == "bnZCRxw3k\n"


def test_inbox_clock_overflow_preserves_last_valid_sequence(tmp_path, monkeypatch):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    sequence = shared_state_path(tmp_path, "inbox-key-sequence")
    sequence.parent.mkdir(parents=True, exist_ok=True)
    sequence.write_text("zzzzzzzzz\n")
    monkeypatch.setattr(identity, "epoch_micros", lambda: EPOCH_MICROS)
    with pytest.raises(ValueError, match="does not fit"):
        mint_inbox_key(tmp_path)
    assert sequence.read_text() == "zzzzzzzzz\n"


@pytest.mark.parametrize("ceiling", [LEGACY, CURRENT])
def test_graph_ceiling_compares_instants_across_widths(ceiling):
    rows = [{"incepted": stamp} for stamp in [LEGACY, "1dzGkSrK", CURRENT, "bnZCRxw3j"]]
    assert [row["incepted"] for row in graph.live_rows(rows, ceiling=ceiling)] == [
        LEGACY,
        CURRENT,
    ]
