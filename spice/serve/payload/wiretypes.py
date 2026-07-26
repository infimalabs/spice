"""The type vocabulary every serve wire declaration is written in.

Split out of wire.py so the declarations in wireschema.py and the validator and
JSDoc renderer in wire.py share one algebra without importing each other.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class WireType:
    kind: str
    name: str = ""
    items: tuple["WireType", ...] = ()
    literal: Any = None


@dataclass(frozen=True)
class WireField:
    name: str
    value_type: WireType
    optional: bool = False


@dataclass(frozen=True)
class WireObject:
    name: str
    fields: tuple[WireField, ...]


def _primitive(name: str) -> WireType:
    return WireType(name)


STRING = _primitive("string")
INTEGER = _primitive("integer")
NUMBER = _primitive("number")
BOOLEAN = _primitive("boolean")
JSON_VALUE = _primitive("json")


def ref(name: str) -> WireType:
    return WireType("reference", name=name)


def array(item: WireType) -> WireType:
    return WireType("array", items=(item,))


def record(item: WireType) -> WireType:
    return WireType("record", items=(item,))


def union(*items: WireType) -> WireType:
    return WireType("union", items=items)


def literal(value: Any) -> WireType:
    return WireType("literal", literal=value)


def wire_field(name: str, value_type: WireType, *, optional: bool = False) -> WireField:
    return WireField(name, value_type, optional)


def wire_object(
    name: str,
    required: Mapping[str, WireType] | None = None,
    optional: Mapping[str, WireType] | None = None,
) -> WireObject:
    return WireObject(
        name,
        tuple(wire_field(key, value) for key, value in (required or {}).items())
        + tuple(
            wire_field(key, value, optional=True)
            for key, value in (optional or {}).items()
        ),
    )


STRINGS = array(STRING)
NUMBERS = array(NUMBER)
