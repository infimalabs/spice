"""User-facing failure: printed as `spice: <message>`, exit code 2."""

from __future__ import annotations


class SpiceError(RuntimeError):
    pass
