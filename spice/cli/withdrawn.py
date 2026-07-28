"""Explicit refusals for command-line spellings removed from public contracts."""

from __future__ import annotations

import argparse
from typing import Any

from spice.errors import SpiceError

DRY_RUN_WITHDRAWAL_RELEASE = "v0.30.0"


class _WithdrawnDryRunAction(argparse.Action):
    def __call__(
        self,
        parser: argparse.ArgumentParser,
        namespace: argparse.Namespace,
        values: Any,
        option_string: str | None = None,
    ) -> None:
        del parser, namespace, values
        spelling = option_string or "--dry-run"
        raise SpiceError(
            f"`{spelling}` was withdrawn in {DRY_RUN_WITHDRAWAL_RELEASE} when "
            "authored-input commands became previews by default; invoke the "
            "command without it to preview, or use `--apply` to execute the plan"
        )


def add_withdrawn_dry_run_argument(parser: argparse.ArgumentParser) -> None:
    """Recognize the old spelling only far enough to return its migration."""
    parser.add_argument(
        "--dry-run",
        action=_WithdrawnDryRunAction,
        nargs=0,
        help=argparse.SUPPRESS,
    )
