"""Shared markers for tests that require real POSIX mode-bit denial."""

from __future__ import annotations

import os

import pytest

MODE_BIT_DENIAL_SKIP_REASON = (
    "requires POSIX mode bits to deny access; uid 0 bypasses that denial"
)
REQUIRES_MODE_BIT_DENIAL = pytest.mark.skipif(
    getattr(os, "geteuid", lambda: -1)() == 0,
    reason=MODE_BIT_DENIAL_SKIP_REASON,
)


def test_mode_bit_denial_skip_reason_names_privileged_bypass() -> None:
    assert MODE_BIT_DENIAL_SKIP_REASON == (
        "requires POSIX mode bits to deny access; uid 0 bypasses that denial"
    )
