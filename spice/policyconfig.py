"""Resolved tracked policy overlay.

`spice.policy` remains the built-in constitution. This module is the single
project-config seam: tracked `[tool.spice.policy]` values override defaults,
and malformed configuration fails loudly with the offending key.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import cast

from spice import policy
from spice.errors import SpiceError
from spice.repocfg import policy_table


@dataclass(frozen=True)
class PolicyLimits:
    file_loc: int
    file_bytes: int
    routine_ccn: int
    routine_length: int
    commit_message_wrap: int
    repo_truth_doc_chars: int


@dataclass(frozen=True)
class PolicyFlex:
    ratio: float
    file_loc: int
    file_bytes: int
    routine_ccn: int
    routine_length: int


@dataclass(frozen=True)
class PolicyMagic:
    examine_threshold: int
    baseline_ref: str


@dataclass(frozen=True)
class PolicyDebt:
    reachability_test_only: int
    assertion_free_tests: int


@dataclass(frozen=True)
class PolicyLanguages:
    complexity: tuple[str, ...]
    magic: tuple[str, ...]
    env: tuple[str, ...]
    c_grammar: tuple[str, ...]


@dataclass(frozen=True)
class PolicyLockfiles:
    suffixes: tuple[str, ...]
    names: tuple[str, ...]


@dataclass(frozen=True)
class PolicyEnvAccess:
    family_suffixes: Mapping[str, tuple[str, ...]]
    default_patterns: Mapping[str, tuple[str, ...]]


@dataclass(frozen=True)
class FileShapePolicy:
    line_limit: int
    line_flex_limit: int
    byte_limit: int
    byte_flex_limit: int


@dataclass(frozen=True)
class ComplexityPolicy:
    max_ccn: int
    ccn_flex_limit: int
    max_length: int
    length_flex_limit: int


@dataclass(frozen=True)
class ResolvedPolicy:
    limits: PolicyLimits
    flex: PolicyFlex
    magic: PolicyMagic
    debt: PolicyDebt
    languages: PolicyLanguages
    lockfiles: PolicyLockfiles
    env_access: PolicyEnvAccess

    @property
    def file_shape(self) -> FileShapePolicy:
        return FileShapePolicy(
            line_limit=self.limits.file_loc,
            line_flex_limit=self.flex.file_loc,
            byte_limit=self.limits.file_bytes,
            byte_flex_limit=self.flex.file_bytes,
        )

    @property
    def complexity(self) -> ComplexityPolicy:
        return ComplexityPolicy(
            max_ccn=self.limits.routine_ccn,
            ccn_flex_limit=self.flex.routine_ccn,
            max_length=self.limits.routine_length,
            length_flex_limit=self.flex.routine_length,
        )


def resolve_policy(repo_root: Path) -> ResolvedPolicy:
    raw_policy = policy_table(repo_root)
    limits_table = _subtable(raw_policy, "limits")
    limits = PolicyLimits(
        file_loc=_positive_int(
            limits_table,
            "file_loc",
            policy.FILE_LOC_LIMIT,
            "[tool.spice.policy.limits]",
        ),
        file_bytes=_positive_int(
            limits_table,
            "file_bytes",
            policy.FILE_BYTE_LIMIT,
            "[tool.spice.policy.limits]",
        ),
        routine_ccn=_positive_int(
            limits_table,
            "routine_ccn",
            policy.COMPLEXITY_MAX_CCN,
            "[tool.spice.policy.limits]",
        ),
        routine_length=_positive_int(
            limits_table,
            "routine_length",
            policy.COMPLEXITY_MAX_LENGTH,
            "[tool.spice.policy.limits]",
        ),
        commit_message_wrap=_positive_int(
            limits_table,
            "commit_message_wrap",
            policy.COMMIT_MESSAGE_WRAP_LIMIT,
            "[tool.spice.policy.limits]",
        ),
        repo_truth_doc_chars=_positive_int(
            limits_table,
            "repo_truth_doc_chars",
            policy.REPO_TRUTH_DOC_LIMIT,
            "[tool.spice.policy.limits]",
        ),
    )
    flex_table = _subtable(raw_policy, "flex")
    ratio = _ratio(flex_table)
    flex = PolicyFlex(
        ratio=ratio,
        file_loc=_flex_value(flex_table, "file_loc", limits.file_loc, ratio),
        file_bytes=_flex_value(flex_table, "file_bytes", limits.file_bytes, ratio),
        routine_ccn=_flex_value(flex_table, "routine_ccn", limits.routine_ccn, ratio),
        routine_length=_flex_value(
            flex_table, "routine_length", limits.routine_length, ratio
        ),
    )
    magic_table = _subtable(raw_policy, "magic")
    debt_table = _subtable(raw_policy, "debt")
    return ResolvedPolicy(
        limits=limits,
        flex=flex,
        magic=PolicyMagic(
            examine_threshold=_positive_int(
                magic_table,
                "examine_threshold",
                policy.MAGIC_EXAMINE_VALUE_THRESHOLD,
                "[tool.spice.policy.magic]",
            ),
            baseline_ref=_non_empty_string(
                magic_table,
                "baseline_ref",
                policy.MAGIC_BASELINE_REF,
                "[tool.spice.policy.magic]",
            ),
        ),
        debt=PolicyDebt(
            reachability_test_only=_non_negative_int(
                debt_table,
                "reachability_test_only",
                policy.REACHABILITY_TEST_ONLY_LIMIT,
                "[tool.spice.policy.debt]",
            ),
            assertion_free_tests=_non_negative_int(
                debt_table,
                "assertion_free_tests",
                policy.ASSERTION_FREE_TEST_LIMIT,
                "[tool.spice.policy.debt]",
            ),
        ),
        languages=_languages(raw_policy),
        lockfiles=_lockfiles(raw_policy),
        env_access=_env_access(raw_policy),
    )


def _languages(raw_policy: Mapping[str, object]) -> PolicyLanguages:
    table = _subtable(raw_policy, "languages")
    return PolicyLanguages(
        complexity=_string_tuple(
            table,
            "complexity",
            policy.COMPLEXITY_SUFFIXES,
            "[tool.spice.policy.languages]",
            suffixes=True,
        ),
        magic=_string_tuple(
            table,
            "magic",
            policy.MAGIC_SUFFIXES,
            "[tool.spice.policy.languages]",
            suffixes=True,
        ),
        env=_string_tuple(
            table,
            "env",
            policy.ENV_SUFFIXES,
            "[tool.spice.policy.languages]",
            suffixes=True,
        ),
        c_grammar=_string_tuple(
            table,
            "c_grammar",
            policy.C_GRAMMAR_SUFFIXES,
            "[tool.spice.policy.languages]",
            suffixes=True,
        ),
    )


def _lockfiles(raw_policy: Mapping[str, object]) -> PolicyLockfiles:
    table = _subtable(raw_policy, "lockfiles")
    return PolicyLockfiles(
        suffixes=_string_tuple(
            table,
            "suffixes",
            policy.FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES,
            "[tool.spice.policy.lockfiles]",
            suffixes=True,
        ),
        names=_string_tuple(
            table,
            "names",
            policy.FILE_SHAPE_GENERATED_LOCKFILE_NAMES,
            "[tool.spice.policy.lockfiles]",
        ),
    )


def _env_access(raw_policy: Mapping[str, object]) -> PolicyEnvAccess:
    table = _subtable(raw_policy, "env_access")
    return PolicyEnvAccess(
        family_suffixes=_string_tuple_map(
            _nested_subtable(
                table, "family_suffixes", "[tool.spice.policy.env_access]"
            ),
            policy.ENV_ACCESS_FAMILY_SUFFIXES,
            "[tool.spice.policy.env_access.family_suffixes]",
            suffixes=True,
        ),
        default_patterns=_string_tuple_map(
            _nested_subtable(
                table, "default_patterns", "[tool.spice.policy.env_access]"
            ),
            policy.ENV_ACCESS_DEFAULT_PATTERNS,
            "[tool.spice.policy.env_access.default_patterns]",
        ),
    )


def _subtable(raw_policy: Mapping[str, object], key: str) -> Mapping[str, object]:
    raw = raw_policy.get(key)
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise SpiceError(f"[tool.spice.policy.{key}] must be a table")
    return cast(Mapping[str, object], raw)


def _nested_subtable(
    table: Mapping[str, object], key: str, context: str
) -> Mapping[str, object]:
    raw = table.get(key)
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise SpiceError(f"{context} {key} must be a table")
    return cast(Mapping[str, object], raw)


def _positive_int(
    table: Mapping[str, object], key: str, default: int, context: str
) -> int:
    raw = table.get(key, default)
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise SpiceError(f"{context} {key} must be a positive integer")
    if raw <= 0:
        raise SpiceError(f"{context} {key} must be a positive integer")
    return raw


def _non_negative_int(
    table: Mapping[str, object], key: str, default: int, context: str
) -> int:
    raw = table.get(key, default)
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise SpiceError(f"{context} {key} must be a non-negative integer")
    if raw < 0:
        raise SpiceError(f"{context} {key} must be a non-negative integer")
    return raw


def _ratio(table: Mapping[str, object]) -> float:
    raw = table.get("ratio", policy.FLEX_NUMERATOR / policy.FLEX_DENOMINATOR)
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        raise SpiceError("[tool.spice.policy.flex] ratio must be a number >= 1.0")
    value = float(raw)
    if value < 1.0:
        raise SpiceError("[tool.spice.policy.flex] ratio must be a number >= 1.0")
    return value


def _flex_value(table: Mapping[str, object], key: str, base: int, ratio: float) -> int:
    raw = table.get(key)
    if raw is None:
        return int(base * ratio)
    value = _positive_int(table, key, base, "[tool.spice.policy.flex]")
    if value < base:
        raise SpiceError(
            f"[tool.spice.policy.flex] {key} must be >= "
            f"[tool.spice.policy.limits] {key}"
        )
    return value


def _non_empty_string(
    table: Mapping[str, object], key: str, default: str, context: str
) -> str:
    raw = table.get(key, default)
    if not isinstance(raw, str) or not raw.strip():
        raise SpiceError(f"{context} {key} must be a non-empty string")
    return raw.strip()


def _string_tuple(
    table: Mapping[str, object],
    key: str,
    default: tuple[str, ...],
    context: str,
    *,
    suffixes: bool = False,
) -> tuple[str, ...]:
    raw = table.get(key)
    if raw is None:
        return tuple(default)
    if not isinstance(raw, list):
        raise SpiceError(f"{context} {key} must be a list of strings")
    values: list[str] = []
    for item in raw:
        if not isinstance(item, str) or not item.strip():
            raise SpiceError(f"{context} {key} must be a list of strings")
        value = item.strip()
        if suffixes and not value.startswith("."):
            raise SpiceError(f"{context} {key} entries must be file suffixes")
        if value not in values:
            values.append(value)
    return tuple(values)


def _string_tuple_map(
    table: Mapping[str, object],
    default: Mapping[str, tuple[str, ...]],
    context: str,
    *,
    suffixes: bool = False,
) -> Mapping[str, tuple[str, ...]]:
    resolved = dict(default)
    for key in table:
        resolved[key] = _string_tuple(table, key, (), context, suffixes=suffixes)
    return resolved
