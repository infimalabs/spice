"""The type vocabulary every serve wire declaration is written in.

Split out of wire.py so the declarations in wireschema.py and the validator and
JSDoc renderer in wire.py share one algebra without importing each other.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
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

# A field one arm of a union declares only to say it never carries it. Optional
# and typed `undefined`, it costs nothing on the wire -- the producer omits it,
# and validation rejects it if present -- while giving the browser the property
# it needs to narrow on. Without it a reader cannot even mention the field: a
# name missing from one arm is an error on the union rather than a question the
# reader is allowed to ask.
ABSENT = _primitive("undefined")


def absent(fields: Iterable[str]) -> dict[str, WireType]:
    """Deny every one of ``fields``, so an arm is written from the other's list.

    Passing the sibling's own field map is what keeps the two in step: a field
    added to one arm is denied by the other in the same edit, rather than
    quietly becoming readable on both.
    """
    return dict.fromkeys(fields, ABSENT)


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
