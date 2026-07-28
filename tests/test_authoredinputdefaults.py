"""Executable inventory for authored-input mutation defaults."""

import argparse
from dataclasses import dataclass

import pytest

from spice.cli.effects import (
    AUTHORED_EFFECT_READS,
    AuthoredInputInvocation,
    MutationDecision,
    authored_input_invocations,
)
from spice.cli.entry import main
from spice.cli.parser import build_parser
from spice.cli.withdrawn import DRY_RUN_WITHDRAWAL_RELEASE
from spice.release import build_release_parser


@dataclass(frozen=True)
class LiveAuthoredInputInvocation:
    command_path: tuple[str, ...]
    parse_path: tuple[str, ...]
    root_parser: argparse.ArgumentParser
    leaf_parser: argparse.ArgumentParser
    contract: AuthoredInputInvocation

    @property
    def invocation_argv(self) -> tuple[str, ...]:
        return (*self.parse_path, *self.contract.sample_suffix)

    @property
    def display(self) -> str:
        suffix = " ".join((*self.contract.sample_suffix, *self.contract.mutation_args))
        command = " ".join(self.command_path)
        return f"{command} {suffix}".strip()


DRY_RUN_REPLACED_VERBS = (
    ("init",),
    ("task", "ingest", "plan.md"),
)


def _command_parsers(
    parser: argparse.ArgumentParser,
    prefix: tuple[str, ...] = (),
) -> tuple[tuple[tuple[str, ...], argparse.ArgumentParser], ...]:
    commands: list[tuple[tuple[str, ...], argparse.ArgumentParser]] = []
    for action in parser._actions:
        if not isinstance(action, argparse._SubParsersAction):
            continue
        for name, child in action.choices.items():
            path = (*prefix, name)
            commands.append((path, child))
            commands.extend(_command_parsers(child, path))
    return tuple(commands)


def _live_authored_input_inventory() -> tuple[LiveAuthoredInputInvocation, ...]:
    inventory: list[LiveAuthoredInputInvocation] = []
    roots = (
        ((), build_parser(include_mounted_epilog=False)),
        (("release",), build_release_parser()),
    )
    for display_prefix, root_parser in roots:
        for parse_path, leaf_parser in _command_parsers(root_parser):
            for contract in authored_input_invocations(leaf_parser):
                inventory.append(
                    LiveAuthoredInputInvocation(
                        command_path=(*display_prefix, *parse_path),
                        parse_path=parse_path,
                        root_parser=root_parser,
                        leaf_parser=leaf_parser,
                        contract=contract,
                    )
                )
    return tuple(inventory)


LIVE_AUTHORED_INPUT_INVENTORY = _live_authored_input_inventory()


@pytest.mark.parametrize(
    "live",
    LIVE_AUTHORED_INPUT_INVENTORY,
    ids=lambda live: live.display,
)
def test_live_authored_input_mutation_decision_matches_effect_driving_reads(
    live: LiveAuthoredInputInvocation,
) -> None:
    contract = live.contract
    assert AUTHORED_EFFECT_READS.intersection(contract.reads)
    bare = live.root_parser.parse_args(list(live.invocation_argv))

    if contract.decision is MutationDecision.PREVIEW_APPLY:
        explicit = live.root_parser.parse_args(
            [*live.invocation_argv, *contract.mutation_args]
        )
        assert bare.apply is False
        assert explicit.apply is True
        return

    assert not hasattr(bare, "apply")
    if contract.decision is MutationDecision.EXPLICIT_OPTION:
        explicit = live.root_parser.parse_args(
            [*live.invocation_argv, *contract.mutation_args]
        )
        destination = _option_destination(live.leaf_parser, contract.mutation_args[0])
        assert getattr(bare, destination) in (None, False)
        assert getattr(explicit, destination) not in (None, False)
        return

    assert contract.decision is MutationDecision.HOOK_BACKEND
    assert contract.mutation_args == ()


def test_live_authored_input_inventory_is_unique_and_exercises_every_decision() -> None:
    identities = [
        (
            live.command_path,
            live.contract.sample_suffix,
            live.contract.mutation_args,
        )
        for live in LIVE_AUTHORED_INPUT_INVENTORY
    ]

    assert len(identities) == len(set(identities))
    assert {live.contract.decision for live in LIVE_AUTHORED_INPUT_INVENTORY} == set(
        MutationDecision
    )


def test_every_live_explicit_mutation_option_has_an_authored_input_contract() -> None:
    classified = {
        (live.command_path, live.contract.mutation_args[0])
        for live in LIVE_AUTHORED_INPUT_INVENTORY
        if live.contract.decision is MutationDecision.EXPLICIT_OPTION
    }
    live_options: set[tuple[tuple[str, ...], str]] = set()
    for display_prefix, root_parser in (
        ((), build_parser(include_mounted_epilog=False)),
        (("release",), build_release_parser()),
    ):
        for parse_path, leaf_parser in _command_parsers(root_parser):
            for action in leaf_parser._actions:
                for option in action.option_strings:
                    if (
                        option == "--fix"
                        or option == "--create-tasks"
                        or option.startswith("--write")
                    ):
                        live_options.add(((*display_prefix, *parse_path), option))

    assert classified == live_options


def _option_destination(parser: argparse.ArgumentParser, option: str) -> str:
    matches = [
        action.dest for action in parser._actions if option in action.option_strings
    ]
    assert len(matches) == 1
    return matches[0]


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


def test_receipt_writers_and_unapply_verbs_are_the_same_live_parser_set() -> None:
    parsers = dict(
        (
            *_command_parsers(build_parser()),
            *(
                (("release", *path), parser)
                for path, parser in _command_parsers(build_release_parser())
            ),
        )
    )
    receipt_writers = {
        path
        for path, parser in parsers.items()
        if parser.get_default("writes_receipt") is True
    }
    unapply_verbs = {
        path
        for path, parser in parsers.items()
        if any("--unapply" in action.option_strings for action in parser._actions)
    }

    assert (receipt_writers, unapply_verbs) == ({("init",)}, {("init",)})
