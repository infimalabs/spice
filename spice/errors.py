"""User-facing failure: printed as `spice: <message>`, exit code 2.

Library seam: target-repo tools may raise `SpiceError` for user-facing
failures; the class name and RuntimeError base are source-stable.
"""

from __future__ import annotations


class SpiceError(RuntimeError):
    pass
