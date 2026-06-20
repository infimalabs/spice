"""Argparse recovery metadata for command-specific parse errors."""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from typing import Any, Iterable, NoReturn, TypeVar, overload

_NamespaceT = TypeVar("_NamespaceT")


@dataclass(frozen=True)
class RecoveryMetadata:
    hints: tuple[str, ...] = ()
    examples: tuple[str, ...] = ()


class RecoveringArgumentParser(argparse.ArgumentParser):
    @overload
    def parse_args(
        self,
        args: Iterable[str] | None = None,
        namespace: None = None,
    ) -> argparse.Namespace: ...

    @overload
    def parse_args(
        self,
        args: Iterable[str] | None = None,
        *,
        namespace: _NamespaceT,
    ) -> _NamespaceT: ...

    def parse_args(
        self,
        args: Iterable[str] | None = None,
        namespace: Any = None,
    ) -> Any:
        parsed, extras = self.parse_known_args(args, namespace)
        if extras:
            parser = getattr(parsed, "_spice_error_parser", self)
            parser.error("unrecognized arguments: " + " ".join(extras))
        assert parsed is not None
        return parsed

    def add_subparsers(self, **kwargs: Any) -> argparse._SubParsersAction:
        kwargs.setdefault("parser_class", type(self))
        kwargs.setdefault("action", RecoveringSubParsersAction)
        return super().add_subparsers(**kwargs)

    def error(self, message: str) -> NoReturn:
        self.print_usage(sys.stderr)
        self.exit(2, f"{self.prog}: error: {message}\n{render_recovery(self)}\n")


class RecoveringSubParsersAction(argparse._SubParsersAction):
    def add_parser(self, name: str, **kwargs: Any) -> argparse.ArgumentParser:
        hints = tuple(_clean(kwargs.pop("recovery_hints", ())))
        examples = tuple(_clean(kwargs.pop("recovery_examples", ())))
        help_text = kwargs.get("help")
        parser = super().add_parser(name, **kwargs)
        if not hints and isinstance(help_text, str) and help_text.strip():
            hints = (help_text.strip(),)
        if not examples:
            examples = (f"{parser.prog} --help",)
        set_recovery(parser, hints=hints, examples=examples)
        parser.set_defaults(_spice_error_parser=parser)
        return parser


def set_recovery(
    parser: argparse.ArgumentParser,
    *,
    hints: Iterable[str] = (),
    examples: Iterable[str] = (),
) -> argparse.ArgumentParser:
    parser.set_defaults(
        _spice_recovery=RecoveryMetadata(
            hints=tuple(_clean(hints)),
            examples=tuple(_clean(examples)),
        )
    )
    parser._spice_recovery = RecoveryMetadata(  # type: ignore[attr-defined]
        hints=tuple(_clean(hints)),
        examples=tuple(_clean(examples)),
    )
    return parser


def render_recovery(parser: argparse.ArgumentParser) -> str:
    metadata = getattr(parser, "_spice_recovery", RecoveryMetadata())
    lines = [
        f"Try `{parser.prog} --help` for the exact contract.",
    ]
    if metadata.hints:
        lines.append("Hints:")
        lines.extend(f"  - {hint}" for hint in metadata.hints)
    if metadata.examples:
        lines.append("Examples:")
        lines.extend(f"  {example}" for example in metadata.examples)
    return "\n".join(lines)


def _clean(values: Iterable[str]) -> list[str]:
    return [str(value).strip() for value in values if str(value).strip()]
