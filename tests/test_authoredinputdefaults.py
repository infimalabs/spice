"""Executable inventory for authored-input mutation defaults."""

from dataclasses import dataclass
from enum import StrEnum

import pytest

from spice.cli.entry import main
from spice.cli.parser import build_parser
from spice.cli.withdrawn import DRY_RUN_WITHDRAWAL_RELEASE
from spice.release import build_release_parser


class EffectRead(StrEnum):
    """Inputs whose semantics can determine a command's planned effects."""

    AUTHORED_DOCUMENT = "authored-document"
    AUTHORED_REPOSITORY = "authored-repository"
    AUTHORED_CONFIGURATION = "authored-configuration"
    OWNERSHIP_RECEIPT = "ownership-receipt"
    COMMAND_LINE = "command-line"
    TASK_BOARD = "task-board"


AUTHORED_READS = frozenset(
    {
        EffectRead.AUTHORED_DOCUMENT,
        EffectRead.AUTHORED_REPOSITORY,
        EffectRead.AUTHORED_CONFIGURATION,
        EffectRead.OWNERSHIP_RECEIPT,
    }
)


@dataclass(frozen=True)
class MutatingVerb:
    argv: tuple[str, ...]
    reads: tuple[EffectRead, ...]
    release: bool = False


MUTATING_VERBS = (
    MutatingVerb(
        ("init",),
        (EffectRead.AUTHORED_REPOSITORY, EffectRead.AUTHORED_CONFIGURATION),
    ),
    MutatingVerb(
        ("deinit",),
        (
            EffectRead.AUTHORED_REPOSITORY,
            EffectRead.AUTHORED_CONFIGURATION,
            EffectRead.OWNERSHIP_RECEIPT,
        ),
    ),
    MutatingVerb(
        ("task", "ingest", "plan.md", "--project", "task.plan"),
        (EffectRead.AUTHORED_DOCUMENT, EffectRead.TASK_BOARD),
    ),
    MutatingVerb(
        ("minor",),
        (EffectRead.AUTHORED_REPOSITORY,),
        release=True,
    ),
    MutatingVerb(
        ("patch",),
        (EffectRead.AUTHORED_REPOSITORY,),
        release=True,
    ),
    MutatingVerb(
        ("prepare", "minor"),
        (EffectRead.AUTHORED_REPOSITORY,),
        release=True,
    ),
    MutatingVerb(
        ("publish",),
        (EffectRead.AUTHORED_REPOSITORY,),
        release=True,
    ),
    MutatingVerb(
        ("github",),
        (EffectRead.AUTHORED_REPOSITORY,),
        release=True,
    ),
)

DRY_RUN_REPLACED_VERBS = (
    ("init",),
    ("task", "ingest", "plan.md"),
)


def _derived_default(reads: tuple[EffectRead, ...]) -> str:
    return "preview" if AUTHORED_READS.intersection(reads) else "apply"


@pytest.mark.parametrize("verb", MUTATING_VERBS, ids=lambda verb: " ".join(verb.argv))
def test_mutating_verb_default_is_derived_from_effect_driving_reads(
    verb: MutatingVerb,
) -> None:
    parser = build_release_parser() if verb.release else build_parser()

    bare = parser.parse_args(list(verb.argv))
    explicit = parser.parse_args([*verb.argv, "--apply"])
    observed_default = "apply" if bare.apply else "preview"

    assert _derived_default(verb.reads) == observed_default
    assert explicit.apply is True


@pytest.mark.parametrize(
    "verb", DRY_RUN_REPLACED_VERBS, ids=lambda argv: " ".join(argv)
)
@pytest.mark.parametrize(
    "options",
    (
        ("--dry-run",),
        ("--dry-run", "--apply"),
        ("--apply", "--dry-run"),
    ),
    ids=("withdrawn", "withdrawn-before-apply", "apply-before-withdrawn"),
)
def test_withdrawn_dry_run_refuses_with_release_and_replacement(
    verb: tuple[str, ...],
    options: tuple[str, ...],
    capsys: pytest.CaptureFixture[str],
) -> None:
    assert main([*verb, *options]) == 2

    captured = capsys.readouterr()
    assert captured.out == ""
    assert f"`--dry-run` was withdrawn in {DRY_RUN_WITHDRAWAL_RELEASE}" in captured.err
    assert "invoke the command without it to preview" in captured.err
    assert "use `--apply` to execute the plan" in captured.err
