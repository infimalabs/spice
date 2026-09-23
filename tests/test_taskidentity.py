"""Rust-compatible moment stamps and retained Python task identities."""

from datetime import UTC, datetime, timedelta, timezone

import pytest

from spice.errors import SpiceError
from spice.tasks import identity

EPOCH_MICROS = 1_700_000_000_000_000
STAMP_SPACE = 2_779_905_883_635_712
LEGACY = "1dzGkSrJ"
CURRENT = "bnZCRxw3h"


@pytest.mark.parametrize(
    ("micros", "stamp"),
    [
        (0, "000000000"),
        (51, "00000000z"),
        (52, "000000010"),
        (EPOCH_MICROS, CURRENT),
        (STAMP_SPACE - 1, "zzzzzzzzz"),
    ],
)
def test_current_codec_matches_rust_vectors(micros, stamp):
    assert identity.encode_width(micros) == stamp
    assert identity.incepted_micros(stamp) == micros
    assert identity.epoch_micros(identity.incepted_datetime(stamp)) == micros


def test_every_base52_digit_keeps_its_rust_position():
    alphabet = "0123456789BCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz"
    for position in range(9):
        for digit, character in enumerate(alphabet):
            expected = "0" * (8 - position) + character + "0" * position
            assert identity.encode_width(digit * 52**position) == expected


def test_both_widths_decode_the_same_instant_without_rewriting_identity(monkeypatch):
    instant = datetime(2023, 11, 14, 22, 13, 20, tzinfo=UTC)
    rows = [
        {"incepted": stamp, "project": "task.identity", "description": "Retained"}
        for stamp in (LEGACY, CURRENT)
    ]
    monkeypatch.setattr(identity.tw, "export", lambda: rows)
    for row in rows:
        stamp = row["incepted"]
        handle = f"IDENTIT-{stamp}"
        assert identity.incepted_datetime(stamp) == instant
        assert identity.incepted_micros(stamp) == EPOCH_MICROS
        assert identity.incepted_of_handle(handle) == stamp
        assert identity.render_handle(row) == handle
        assert identity.resolve(handle) is row
        assert identity.resolve(stamp) is row


@pytest.mark.parametrize(
    "stamp", ["", "0000000", "0000000000", "0000000A", "00000000A", "bnZC\nRxw3h"]
)
def test_stamp_readers_reject_noncanonical_spelling(stamp):
    with pytest.raises(ValueError, match="invalid inception stamp"):
        identity.incepted_micros(stamp)
    with pytest.raises(SpiceError, match="not a valid task handle"):
        identity.incepted_of_handle(f"IDENTIT-{stamp}")
    assert identity.INCEPTED_RE.fullmatch(stamp) is None


def test_integer_clock_preserves_microseconds_and_timezone_offsets():
    # Floating-point timestamp multiplication loses the third microsecond here.
    utc = datetime(2040, 1, 1, tzinfo=UTC)
    offset = timezone(timedelta(hours=-6))
    for microsecond in range(1000):
        instant = utc + timedelta(microseconds=microsecond)
        expected = 2_208_988_800_000_000 + microsecond
        assert identity.epoch_micros(instant) == expected
        assert identity.epoch_micros(instant.astimezone(offset)) == expected
        assert identity.incepted_datetime(identity.encode_width(expected)) == instant


def test_task_collisions_advance_exactly_one_microsecond(monkeypatch):
    monkeypatch.setattr(identity, "epoch_micros", lambda: EPOCH_MICROS)
    occupied = {LEGACY, CURRENT, "bnZCRxw3j"}
    assert identity.mint_incepted(occupied) == "bnZCRxw3k"
    assert identity.mint_incepted({LEGACY}) == CURRENT


@pytest.mark.parametrize("micros", [-1, STAMP_SPACE])
def test_mint_refuses_out_of_range_clock_without_wrapping(monkeypatch, micros):
    monkeypatch.setattr(identity, "epoch_micros", lambda: micros)
    with pytest.raises(ValueError):
        identity.mint_incepted(set())


def test_final_representable_instant_and_collision_overflow(monkeypatch):
    assert identity.incepted_datetime("zzzzzzzzz") == datetime(
        2058, 2, 2, 20, 4, 43, 635711, tzinfo=UTC
    )
    monkeypatch.setattr(identity, "epoch_micros", lambda: STAMP_SPACE - 1)
    with pytest.raises(ValueError, match="does not fit"):
        identity.mint_incepted({"zzzzzzzzz"})
