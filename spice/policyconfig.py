"""Resolved tracked policy overlay.

`spice.policy` remains the built-in constitution. This module is the single
project-config seam: tracked `[tool.spice.policy]` values override defaults,
and malformed configuration fails loudly with the offending key.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path
from typing import cast

from spice import policy
from spice.errors import SpiceError
from spice.repocfg import policy_table

_COMMIT_TRAILER_KEY_RE = re.compile(r"^[A-Za-z0-9-]+$")
_FORBIDDEN_COMMIT_TRAILER_KEYS = frozenset({"co-authored-by"})


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
class PolicyMarkdownDepthBudget:
    extensions: tuple[str, ...]
    stem_pattern: re.Pattern[str] | None = None


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
class PolicyFileShapePaths:
    source_suffixes: tuple[str, ...]
    generated_patterns: tuple[str, ...]


@dataclass(frozen=True)
class PolicyEnvAccess:
    family_suffixes: Mapping[str, tuple[str, ...]]
    default_patterns: Mapping[str, tuple[str, ...]]


@dataclass(frozen=True)
class ScopeSettings:
    multiplier: float = 1.0
    minimum: int | None = None
    maximum: int | None = None
    unlimited: bool = False
    flex_ratio: float | None = None


@dataclass(frozen=True)
class ScopeMagic:
    examine_threshold: int


@dataclass(frozen=True)
class PolicyScope:
    matcher: str
    settings_by_bound: Mapping[str, ScopeSettings]
    specificity: tuple[int, int, int, int, int]
    magic: ScopeMagic | None = None
    extensions: tuple[str, ...] = ()
    stem_pattern: re.Pattern[str] | None = None
    skip_single_letter_stems: bool = False


@dataclass(frozen=True)
class ScopedBound:
    limit: int
    flex_limit: int
    unlimited: bool = False


@dataclass(frozen=True)
class PolicyCommitMessage:
    wrap_limit: int
    allowed_trailers: frozenset[str] | None


@dataclass(frozen=True)
class FileShapePolicy:
    line_limit: int
    line_flex_limit: int
    byte_limit: int
    byte_flex_limit: int
    line_unlimited: bool = False
    byte_unlimited: bool = False

    @property
    def unlimited(self) -> bool:
        return self.line_unlimited and self.byte_unlimited


@dataclass(frozen=True)
class ComplexityPolicy:
    max_ccn: int
    ccn_flex_limit: int
    max_length: int
    length_flex_limit: int
    hotspot_limit: int
    ccn_unlimited: bool = False
    length_unlimited: bool = False

    @property
    def unlimited(self) -> bool:
        return self.ccn_unlimited and self.length_unlimited


@dataclass(frozen=True)
class ResolvedPolicy:
    limits: PolicyLimits
    flex: PolicyFlex
    complexity_hotspot_limit: int
    magic: PolicyMagic
    debt: PolicyDebt
    markdown_depth_budget: PolicyMarkdownDepthBudget
    languages: PolicyLanguages
    lockfiles: PolicyLockfiles
    file_shape_paths: PolicyFileShapePaths
    env_access: PolicyEnvAccess
    commit_message: PolicyCommitMessage
    scopes: tuple[PolicyScope, ...] = ()

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
            hotspot_limit=self.complexity_hotspot_limit,
        )

    def bound_for_path(self, bound: str, base: int, path: Path) -> ScopedBound:
        scope = self._scope_for_bound(bound, path)
        if scope is None:
            return ScopedBound(
                limit=base,
                flex_limit=_global_flex_for_bound(self.flex, bound, base),
            )
        settings = scope.settings_by_bound[bound]
        if settings.unlimited or settings.multiplier == 0:
            return ScopedBound(limit=base, flex_limit=base, unlimited=True)
        limit = max(1, int(base * settings.multiplier))
        if settings.minimum is not None:
            limit = max(limit, settings.minimum)
        if settings.maximum is not None:
            limit = min(limit, settings.maximum)
        flex_ratio = (
            settings.flex_ratio if settings.flex_ratio is not None else self.flex.ratio
        )
        flex_limit = max(limit, int(limit * flex_ratio))
        return ScopedBound(limit=limit, flex_limit=flex_limit)

    def file_shape_for_path(self, path: Path) -> FileShapePolicy:
        line = self.bound_for_path("file_loc", self.limits.file_loc, path)
        byte = self.bound_for_path("file_bytes", self.limits.file_bytes, path)
        return FileShapePolicy(
            line_limit=line.limit,
            line_flex_limit=line.flex_limit,
            byte_limit=byte.limit,
            byte_flex_limit=byte.flex_limit,
            line_unlimited=line.unlimited,
            byte_unlimited=byte.unlimited,
        )

    def complexity_for_path(self, path: Path) -> ComplexityPolicy:
        ccn = self.bound_for_path("routine_ccn", self.limits.routine_ccn, path)
        length = self.bound_for_path("routine_length", self.limits.routine_length, path)
        return ComplexityPolicy(
            max_ccn=ccn.limit,
            ccn_flex_limit=ccn.flex_limit,
            max_length=length.limit,
            length_flex_limit=length.flex_limit,
            hotspot_limit=self.complexity_hotspot_limit,
            ccn_unlimited=ccn.unlimited,
            length_unlimited=length.unlimited,
        )

    def markdown_depth_budget_applies_to_path(self, path: Path) -> bool:
        return _markdown_selector_matches(path, self.markdown_depth_budget)

    def magic_for_path(self, path: Path) -> PolicyMagic:
        scope = self._scope_for_magic(path)
        if scope is None or scope.magic is None:
            return self.magic
        return PolicyMagic(
            examine_threshold=scope.magic.examine_threshold,
            baseline_ref=self.magic.baseline_ref,
        )

    def magic_examine_threshold_for_path(self, path: Path) -> int:
        return self.magic_for_path(path).examine_threshold

    def _scope_for_bound(self, bound: str, path: Path) -> PolicyScope | None:
        matches = [
            scope
            for scope in self.scopes
            if bound in scope.settings_by_bound and _scope_matches(scope, path)
        ]
        if not matches:
            return None
        return max(matches, key=lambda scope: (scope.specificity, scope.matcher))

    def _scope_for_magic(self, path: Path) -> PolicyScope | None:
        matches = [
            scope
            for scope in self.scopes
            if scope.magic is not None and _scope_matches(scope, path)
        ]
        if not matches:
            return None
        return max(matches, key=lambda scope: (scope.specificity, scope.matcher))


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
    complexity_table = _subtable(raw_policy, "complexity")
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
    markdown_depth_budget = _markdown_depth_budget(raw_policy)
    magic_table = _subtable(raw_policy, "magic")
    debt_table = _subtable(raw_policy, "debt")
    return ResolvedPolicy(
        limits=limits,
        flex=flex,
        complexity_hotspot_limit=_positive_int(
            complexity_table,
            "hotspot_limit",
            policy.COMPLEXITY_HOTSPOT_LIMIT,
            "[tool.spice.policy.complexity]",
        ),
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
        markdown_depth_budget=markdown_depth_budget,
        languages=_languages(raw_policy),
        lockfiles=_lockfiles(raw_policy),
        file_shape_paths=_file_shape_paths(raw_policy),
        env_access=_env_access(raw_policy),
        commit_message=_commit_message(raw_policy, limits),
        scopes=_scopes(raw_policy, markdown_depth_budget),
    )


def _commit_message(
    raw_policy: Mapping[str, object], limits: PolicyLimits
) -> PolicyCommitMessage:
    table = _subtable(raw_policy, "commit_message")
    return PolicyCommitMessage(
        wrap_limit=limits.commit_message_wrap,
        allowed_trailers=_optional_trailer_key_set(
            table,
            "allowed_trailers",
            policy.COMMIT_MESSAGE_ALLOWED_TRAILER_KEYS,
            "[tool.spice.policy.commit_message]",
        ),
    )


SCOPED_BOUND_KEYS = (
    "file_loc",
    "file_bytes",
    "routine_ccn",
    "routine_length",
    "commit_message_wrap",
    "repo_truth_doc_chars",
)
SCOPE_SETTING_KEYS = ("multiplier", "min", "max", "unlimited", "flex")
MARKDOWN_DEPTH_BUDGET_KEYS = ("extensions", "stem_pattern")
SCOPE_NESTED_KEYS = ("magic",)
SCOPE_MAGIC_KEYS = ("examine_threshold",)


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


def _file_shape_paths(raw_policy: Mapping[str, object]) -> PolicyFileShapePaths:
    table = _subtable(raw_policy, "file_shape")
    return PolicyFileShapePaths(
        source_suffixes=_string_tuple(
            table,
            "source_suffixes",
            policy.FILE_SHAPE_SOURCE_SUFFIXES,
            "[tool.spice.policy.file_shape]",
            suffixes=True,
        ),
        generated_patterns=_string_tuple(
            table,
            "generated_patterns",
            policy.FILE_SHAPE_GENERATED_SOURCE_PATTERNS,
            "[tool.spice.policy.file_shape]",
        ),
    )


def _env_access(raw_policy: Mapping[str, object]) -> PolicyEnvAccess:
    table = _subtable(raw_policy, "env_access")
    family_suffixes = _string_tuple_map(
        _nested_subtable(table, "family_suffixes", "[tool.spice.policy.env_access]"),
        policy.ENV_ACCESS_FAMILY_SUFFIXES,
        "[tool.spice.policy.env_access.family_suffixes]",
        suffixes=True,
    )
    default_patterns = _string_tuple_map(
        _nested_subtable(table, "default_patterns", "[tool.spice.policy.env_access]"),
        policy.ENV_ACCESS_DEFAULT_PATTERNS,
        "[tool.spice.policy.env_access.default_patterns]",
    )
    unknown_pattern_families = sorted(set(default_patterns) - set(family_suffixes))
    if unknown_pattern_families:
        listed = ", ".join(unknown_pattern_families)
        raise SpiceError(
            "[tool.spice.policy.env_access.default_patterns] unknown family "
            f"{listed}; declare suffixes in "
            "[tool.spice.policy.env_access.family_suffixes]"
        )
    return PolicyEnvAccess(
        family_suffixes=family_suffixes,
        default_patterns=default_patterns,
    )


def _markdown_depth_budget(
    raw_policy: Mapping[str, object],
) -> PolicyMarkdownDepthBudget:
    table = _subtable(raw_policy, "markdown_depth_budget")
    unknown = sorted(key for key in table if key not in MARKDOWN_DEPTH_BUDGET_KEYS)
    if unknown:
        listed = ", ".join(unknown)
        expected = ", ".join(MARKDOWN_DEPTH_BUDGET_KEYS)
        raise SpiceError(
            "[tool.spice.policy.markdown_depth_budget] unknown key(s): "
            f"{listed}; expected {expected}"
        )
    return PolicyMarkdownDepthBudget(
        extensions=_string_tuple(
            table,
            "extensions",
            policy.MARKDOWN_DEPTH_DOC_EXTENSIONS,
            "[tool.spice.policy.markdown_depth_budget]",
            suffixes=True,
        ),
        stem_pattern=_optional_regex(
            table,
            "stem_pattern",
            "[tool.spice.policy.markdown_depth_budget]",
        ),
    )


def _scopes(
    raw_policy: Mapping[str, object],
    markdown_depth_budget: PolicyMarkdownDepthBudget,
) -> tuple[PolicyScope, ...]:
    table = _subtable(raw_policy, "scopes")
    scopes: list[PolicyScope] = list(_markdown_depth_scopes(markdown_depth_budget))
    for raw_matcher, raw_scope in table.items():
        matcher = str(raw_matcher).strip().replace("\\", "/").removeprefix("./")
        if not matcher:
            raise SpiceError("[tool.spice.policy.scopes] scope keys must be non-empty")
        context = _scope_context(matcher)
        if not isinstance(raw_scope, dict):
            raise SpiceError(f"{context} must be a table")
        scope_table = cast(Mapping[str, object], raw_scope)
        settings_by_bound = _scope_settings_by_bound(scope_table, context)
        scopes.append(
            PolicyScope(
                matcher=matcher,
                settings_by_bound=settings_by_bound,
                specificity=_scope_specificity(matcher, priority=1),
                magic=_scope_magic(scope_table, context),
            )
        )
    return tuple(scopes)


def _markdown_depth_scopes(
    selector: PolicyMarkdownDepthBudget,
) -> tuple[PolicyScope, ...]:
    scopes: list[PolicyScope] = []
    if not selector.extensions:
        return ()
    bounded_depth_count = (
        policy.MARKDOWN_DEPTH_MAX_BOUNDED_CHAR_BUDGET
        // policy.MARKDOWN_DEPTH_BASE_CHAR_BUDGET
    )
    for extension in selector.extensions:
        for depth in range(bounded_depth_count):
            budget = policy.MARKDOWN_DEPTH_BASE_CHAR_BUDGET * (depth + 1)
            matcher = _markdown_depth_matcher(depth, extension)
            scopes.append(
                _markdown_depth_scope(
                    matcher,
                    selector,
                    _fixed_scope_settings(budget),
                )
            )
        scopes.append(
            _markdown_depth_scope(
                _markdown_unbounded_depth_matcher(bounded_depth_count, extension),
                selector,
                ScopeSettings(unlimited=True),
            )
        )
    return tuple(scopes)


def _fixed_scope_settings(limit: int) -> ScopeSettings:
    return ScopeSettings(
        multiplier=1.0,
        minimum=limit,
        maximum=limit,
    )


def _markdown_depth_scope(
    matcher: str,
    selector: PolicyMarkdownDepthBudget,
    settings: ScopeSettings,
) -> PolicyScope:
    return PolicyScope(
        matcher=matcher,
        settings_by_bound={"repo_truth_doc_chars": settings},
        specificity=_scope_specificity(matcher, priority=0),
        extensions=selector.extensions,
        stem_pattern=selector.stem_pattern,
        skip_single_letter_stems=True,
    )


def _markdown_depth_matcher(depth: int, extension: str) -> str:
    name = f"*{extension}"
    if depth == 0:
        return name
    return f"{'/'.join('*' for _ in range(depth))}/{name}"


def _markdown_unbounded_depth_matcher(depth: int, extension: str) -> str:
    return f"{'/'.join('*' for _ in range(depth))}/**/*{extension}"


def _scope_settings_by_bound(
    table: Mapping[str, object], context: str
) -> Mapping[str, ScopeSettings]:
    unknown = sorted(
        key
        for key in table
        if key not in SCOPE_SETTING_KEYS
        and key not in SCOPED_BOUND_KEYS
        and key not in SCOPE_NESTED_KEYS
    )
    if unknown:
        expected = ", ".join(
            (*SCOPE_SETTING_KEYS, *SCOPED_BOUND_KEYS, *SCOPE_NESTED_KEYS)
        )
        listed = ", ".join(unknown)
        raise SpiceError(f"{context} unknown key(s): {listed}; expected {expected}")

    settings_by_bound: dict[str, ScopeSettings] = {}
    flat_keys_present = any(key in table for key in SCOPE_SETTING_KEYS)
    if flat_keys_present:
        flat_settings = _scope_settings(
            {key: table[key] for key in SCOPE_SETTING_KEYS if key in table},
            context,
        )
        settings_by_bound.update({bound: flat_settings for bound in SCOPED_BOUND_KEYS})

    for bound in SCOPED_BOUND_KEYS:
        raw = table.get(bound)
        if raw is None:
            continue
        bound_context = f"{context} {bound}"
        if not isinstance(raw, dict):
            raise SpiceError(f"{bound_context} must be a table")
        settings_by_bound[bound] = _scope_settings(
            cast(Mapping[str, object], raw), bound_context
        )
    return settings_by_bound


def _scope_magic(table: Mapping[str, object], context: str) -> ScopeMagic | None:
    raw = table.get("magic")
    if raw is None:
        return None
    magic_context = f"{context} magic"
    if not isinstance(raw, dict):
        raise SpiceError(f"{magic_context} must be a table")
    magic_table = cast(Mapping[str, object], raw)
    unknown = sorted(key for key in magic_table if key not in SCOPE_MAGIC_KEYS)
    if unknown:
        listed = ", ".join(unknown)
        expected = ", ".join(SCOPE_MAGIC_KEYS)
        raise SpiceError(
            f"{magic_context} unknown key(s): {listed}; expected {expected}"
        )
    return ScopeMagic(
        examine_threshold=_required_positive_int(
            magic_table, "examine_threshold", magic_context
        )
    )


def _scope_settings(table: Mapping[str, object], context: str) -> ScopeSettings:
    unknown = sorted(key for key in table if key not in SCOPE_SETTING_KEYS)
    if unknown:
        listed = ", ".join(unknown)
        expected = ", ".join(SCOPE_SETTING_KEYS)
        raise SpiceError(f"{context} unknown key(s): {listed}; expected {expected}")
    minimum = _optional_positive_int(table, "min", context)
    maximum = _optional_positive_int(table, "max", context)
    if minimum is not None and maximum is not None and minimum > maximum:
        raise SpiceError(f"{context} min must be <= max")
    return ScopeSettings(
        multiplier=_scope_multiplier(table, context),
        minimum=minimum,
        maximum=maximum,
        unlimited=_scope_unlimited(table, context),
        flex_ratio=_optional_ratio(table, "flex", context),
    )


def _optional_positive_int(
    table: Mapping[str, object], key: str, context: str
) -> int | None:
    raw = table.get(key)
    if raw is None:
        return None
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise SpiceError(f"{context} {key} must be a positive integer")
    if raw <= 0:
        raise SpiceError(f"{context} {key} must be a positive integer")
    return raw


def _required_positive_int(table: Mapping[str, object], key: str, context: str) -> int:
    return _positive_int(table, key, 0, context)


def _scope_multiplier(table: Mapping[str, object], context: str) -> float:
    raw = table.get("multiplier", 1.0)
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        raise SpiceError(f"{context} multiplier must be a number >= 0.0")
    value = float(raw)
    if value < 0.0:
        raise SpiceError(f"{context} multiplier must be a number >= 0.0")
    return value


def _scope_unlimited(table: Mapping[str, object], context: str) -> bool:
    raw = table.get("unlimited", False)
    if not isinstance(raw, bool):
        raise SpiceError(f"{context} unlimited must be true or false")
    return raw


def _optional_ratio(
    table: Mapping[str, object], key: str, context: str
) -> float | None:
    raw = table.get(key)
    if raw is None:
        return None
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        raise SpiceError(f"{context} {key} must be a number >= 1.0")
    value = float(raw)
    if value < 1.0:
        raise SpiceError(f"{context} {key} must be a number >= 1.0")
    return value


def _global_flex_for_bound(flex: PolicyFlex, bound: str, base: int) -> int:
    match bound:
        case "file_loc":
            return flex.file_loc
        case "file_bytes":
            return flex.file_bytes
        case "routine_ccn":
            return flex.routine_ccn
        case "routine_length":
            return flex.routine_length
        case _:
            return int(base * flex.ratio)


def _scope_matches(scope: PolicyScope, path: Path) -> bool:
    return _matcher_matches(scope.matcher, path) and _scope_selector_matches(
        scope, path
    )


def _matcher_matches(matcher: str, path: Path) -> bool:
    normalized_path = path.as_posix().replace("\\", "/").removeprefix("./")
    if _has_glob(matcher):
        return any(
            fnmatchcase(normalized_path, variant)
            for variant in _glob_zero_directory_variants(matcher)
        )
    return normalized_path == matcher or normalized_path.startswith(matcher + "/")


def _scope_selector_matches(scope: PolicyScope, path: Path) -> bool:
    if scope.extensions and path.suffix not in scope.extensions:
        return False
    if scope.skip_single_letter_stems and len(path.stem) <= 1:
        return False
    if (
        scope.stem_pattern is not None
        and scope.stem_pattern.fullmatch(path.stem) is None
    ):
        return False
    return True


def _markdown_selector_matches(path: Path, selector: PolicyMarkdownDepthBudget) -> bool:
    if path.suffix not in selector.extensions:
        return False
    if len(path.stem) <= 1:
        return False
    if selector.stem_pattern is not None:
        return selector.stem_pattern.fullmatch(path.stem) is not None
    return True


def _glob_zero_directory_variants(matcher: str) -> tuple[str, ...]:
    variants = [matcher]
    index = 0
    while index < len(variants):
        current = variants[index]
        index += 1
        marker = "/**/"
        if marker not in current:
            continue
        shortened = current.replace(marker, "/", 1)
        if shortened not in variants:
            variants.append(shortened)
    return tuple(variants)


def _scope_specificity(
    matcher: str, *, priority: int
) -> tuple[int, int, int, int, int]:
    exactish = 0 if _has_glob(matcher) else 1
    literal_chars = sum(1 for char in matcher if char not in "*?[]!")
    segments = len([segment for segment in matcher.split("/") if segment])
    return (priority, exactish, literal_chars, segments, len(matcher))


def _has_glob(value: str) -> bool:
    return any(char in value for char in "*?[")


def _scope_context(matcher: str) -> str:
    return f'[tool.spice.policy.scopes."{matcher}"]'


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


def _optional_regex(
    table: Mapping[str, object], key: str, context: str
) -> re.Pattern[str] | None:
    raw = table.get(key)
    if raw is None:
        return None
    if not isinstance(raw, str) or not raw.strip():
        raise SpiceError(f"{context} {key} must be a non-empty regex string")
    try:
        return re.compile(raw.strip())
    except re.error as exc:
        raise SpiceError(f"{context} {key} is not a valid regex: {exc}") from exc


def _optional_trailer_key_set(
    table: Mapping[str, object],
    key: str,
    default: tuple[str, ...] | None,
    context: str,
) -> frozenset[str] | None:
    raw = table.get(key)
    if raw is None:
        if default is None:
            return None
        raw = list(default)
    if not isinstance(raw, list):
        raise SpiceError(f"{context} {key} must be a list of commit trailer keys")
    values: list[str] = []
    for item in raw:
        if not isinstance(item, str) or not item.strip():
            raise SpiceError(f"{context} {key} must be a list of commit trailer keys")
        value = item.strip().lower()
        if _COMMIT_TRAILER_KEY_RE.fullmatch(value) is None:
            raise SpiceError(f"{context} {key} entries must be commit trailer keys")
        if value in _FORBIDDEN_COMMIT_TRAILER_KEYS:
            raise SpiceError(f"{context} {key} must not include Co-Authored-By")
        if value not in values:
            values.append(value)
    return frozenset(values)


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
        values = list(resolved.get(key, ()))
        for value in _string_tuple(table, key, (), context, suffixes=suffixes):
            if value not in values:
                values.append(value)
        resolved[key] = tuple(values)
    return resolved
