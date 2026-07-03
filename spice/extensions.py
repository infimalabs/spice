"""Declared third-party extension entry-point discovery."""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from importlib import metadata
from typing import TypeVar

from spice.errors import SpiceError

SPICE_DRIVER_ENTRY_POINT_GROUP = "spice.drivers"
SPICE_STUDY_ENTRY_POINT_GROUP = "spice.studies"
SPICE_WRAPPER_ENTRY_POINT_GROUP = "spice.wrappers"
SPICE_EXTENSION_ENTRY_POINT_GROUPS = (
    SPICE_DRIVER_ENTRY_POINT_GROUP,
    SPICE_STUDY_ENTRY_POINT_GROUP,
    SPICE_WRAPPER_ENTRY_POINT_GROUP,
)

T = TypeVar("T")


@dataclass(frozen=True)
class SpiceExtensionEntryPoint:
    group: str
    name: str
    value: str
    distribution: str
    entry_point: metadata.EntryPoint

    def load(self) -> object:
        return self.entry_point.load()


def extension_entry_points(
    group: str,
    *,
    built_in_names: Iterable[str] = (),
    distributions: Iterable[metadata.Distribution] | None = None,
) -> tuple[SpiceExtensionEntryPoint, ...]:
    _require_known_group(group)
    reserved = frozenset(str(name) for name in built_in_names)
    entries = tuple(
        sorted(
            _iter_group_entry_points(group, distributions=distributions),
            key=_entry_point_sort_key,
        )
    )
    _raise_for_built_in_shadows(group, entries, reserved)
    _raise_for_duplicate_extension_names(group, entries)
    return entries


def merge_builtin_and_extension_entry_points(
    group: str,
    built_ins: Mapping[str, T],
    *,
    distributions: Iterable[metadata.Distribution] | None = None,
) -> dict[str, T | SpiceExtensionEntryPoint]:
    merged: dict[str, T | SpiceExtensionEntryPoint] = dict(built_ins)
    for entry in extension_entry_points(
        group,
        built_in_names=built_ins,
        distributions=distributions,
    ):
        merged[entry.name] = entry
    return merged


def _iter_group_entry_points(
    group: str,
    *,
    distributions: Iterable[metadata.Distribution] | None,
) -> Iterable[SpiceExtensionEntryPoint]:
    source = distributions if distributions is not None else metadata.distributions()
    for distribution in source:
        distribution_name = _distribution_name(distribution)
        for entry_point in distribution.entry_points:
            if entry_point.group == group:
                yield SpiceExtensionEntryPoint(
                    group=group,
                    name=entry_point.name,
                    value=entry_point.value,
                    distribution=distribution_name,
                    entry_point=entry_point,
                )


def _require_known_group(group: str) -> None:
    if group in SPICE_EXTENSION_ENTRY_POINT_GROUPS:
        return
    expected = ", ".join(SPICE_EXTENSION_ENTRY_POINT_GROUPS)
    raise SpiceError(
        f"unknown spice extension entry point group {group!r}; expected {expected}"
    )


def _entry_point_sort_key(
    entry_point: SpiceExtensionEntryPoint,
) -> tuple[str, str, str]:
    return (entry_point.name, entry_point.distribution, entry_point.value)


def _raise_for_built_in_shadows(
    group: str,
    entries: tuple[SpiceExtensionEntryPoint, ...],
    built_in_names: frozenset[str],
) -> None:
    for entry in entries:
        if entry.name in built_in_names:
            raise SpiceError(
                f"extension entry point group {group!r} entry {entry.name!r} "
                f"from {entry.distribution!r} shadows built-in {entry.name!r}; "
                "pick another name"
            )


def _raise_for_duplicate_extension_names(
    group: str, entries: tuple[SpiceExtensionEntryPoint, ...]
) -> None:
    by_name: dict[str, list[SpiceExtensionEntryPoint]] = {}
    for entry in entries:
        by_name.setdefault(entry.name, []).append(entry)
    for name in sorted(by_name):
        duplicates = by_name[name]
        if len(duplicates) < 2:
            continue
        providers = ", ".join(
            f"{entry.distribution}:{entry.value}" for entry in duplicates
        )
        raise SpiceError(
            f"duplicate extension entry point group {group!r} entry {name!r}; "
            f"providers: {providers}"
        )


def _distribution_name(distribution: metadata.Distribution) -> str:
    return str(distribution.metadata.get("Name") or "<unknown>")
