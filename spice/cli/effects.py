"""Live parser metadata for authored-input mutation decisions."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from enum import StrEnum


class EffectRead(StrEnum):
    """Inputs whose semantics can determine a command's planned effects."""

    AUTHORED_DOCUMENT = "authored-document"
    AUTHORED_REPOSITORY = "authored-repository"
    AUTHORED_CONFIGURATION = "authored-configuration"
    OWNERSHIP_RECEIPT = "ownership-receipt"
    COMMAND_LINE = "command-line"
    TASK_BOARD = "task-board"


AUTHORED_EFFECT_READS = frozenset(
    {
        EffectRead.AUTHORED_DOCUMENT,
        EffectRead.AUTHORED_REPOSITORY,
        EffectRead.AUTHORED_CONFIGURATION,
        EffectRead.OWNERSHIP_RECEIPT,
    }
)


class MutationDecision(StrEnum):
    """How an authored-input invocation receives mutation authority."""

    PREVIEW_APPLY = "preview-apply"
    EXPLICIT_OPTION = "explicit-option"
    HOOK_BACKEND = "hook-backend"


@dataclass(frozen=True)
class AuthoredInputInvocation:
    """One mutating invocation attached to its live leaf parser.

    ``sample_suffix`` supplies required positionals or selectors after the
    command path. ``mutation_args`` is either ``--apply`` or the existing
    explicit mutation option. Hook backends receive their authority from the
    generated Git hook invocation and therefore have no mutation argument.
    """

    reads: tuple[EffectRead, ...]
    decision: MutationDecision
    sample_suffix: tuple[str, ...] = ()
    mutation_args: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        if not AUTHORED_EFFECT_READS.intersection(self.reads):
            raise ValueError(
                "authored-input invocation must include an authored effect read"
            )
        if self.decision is MutationDecision.PREVIEW_APPLY:
            if self.mutation_args != ("--apply",):
                raise ValueError("preview/apply invocation must use `--apply`")
            return
        if self.decision is MutationDecision.EXPLICIT_OPTION:
            if not self.mutation_args or not self.mutation_args[0].startswith("--"):
                raise ValueError("explicit-option invocation requires an option")
            return
        if self.mutation_args:
            raise ValueError("hook backend invocation cannot take mutation arguments")


_PARSER_DEFAULT = "authored_input_invocations"


def mark_authored_input(
    parser: argparse.ArgumentParser,
    *invocations: AuthoredInputInvocation,
) -> None:
    """Attach exhaustive authored-input mutation metadata to a leaf parser."""
    if not invocations:
        raise ValueError("at least one authored-input invocation is required")
    parser.set_defaults(
        **{
            _PARSER_DEFAULT: (
                *authored_input_invocations(parser),
                *invocations,
            )
        }
    )


def authored_input_invocations(
    parser: argparse.ArgumentParser,
) -> tuple[AuthoredInputInvocation, ...]:
    """Read the invocation contracts attached to one live parser."""
    value = parser.get_default(_PARSER_DEFAULT)
    if value is None:
        return ()
    return tuple(value)
