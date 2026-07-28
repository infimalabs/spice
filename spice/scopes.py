"""Universal applicability selectors for layered configuration entries.

``scopes = { ... }`` is one inline selector leaf.  Values inside an axis are
alternatives (OR); different axes are simultaneous requirements (AND); an
absent axis is unconstrained.  The consumer declaration is part of parsing so
one diagnostic owns malformed, unknown, and unsupported axes.

The initial axes come from live entry-applicability consumers:

* paths: policy rules, study providers, and pre-commit command steps;
* drivers: wrappers, wrapper routes, maxim bags, and pre-commit command steps;
* models: pre-commit command steps selected for the effective agent model;
* phases: pre-commit command steps before or after the built-in gate;
* extensions: policy rules that apply only to selected file suffixes.

Command heads and flags remain wrapper-routing payload.  Language families and
test/generated roles remain classification datasets.  Task phases remain live
routing state, and system/repository/worktree names remain layering
metadata.  None of those concepts is a selector axis.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from enum import StrEnum
from pathlib import PurePath, PurePosixPath
from types import MappingProxyType

from spice.errors import SpiceError
from spice.pathmatch import (
    PathSpecificity,
    matches_repo_path,
    normalize_repo_path,
    path_specificity,
)

SCOPES_KEY = "scopes"


class ScopeAxis(StrEnum):
    PATHS = "paths"
    DRIVERS = "drivers"
    MODELS = "models"
    PHASES = "phases"
    EXTENSIONS = "extensions"


SCOPE_AXIS_ORDER = (
    ScopeAxis.PATHS,
    ScopeAxis.EXTENSIONS,
    ScopeAxis.DRIVERS,
    ScopeAxis.MODELS,
    ScopeAxis.PHASES,
)
PRE_COMMIT_SCOPE_PHASES = ("pre-commit", "pre-commit-success")


@dataclass(frozen=True)
class ScopeConsumer:
    """One configurable entry family and the axes it can evaluate."""

    name: str
    supported_axes: frozenset[ScopeAxis]

    @property
    def supported_axis_names(self) -> tuple[str, ...]:
        return tuple(
            axis.value for axis in SCOPE_AXIS_ORDER if axis in self.supported_axes
        )

    def parse(self, raw: object) -> ScopeSelector:
        """Parse one ``scopes`` inline table for this consumer."""
        return _parse_scope_selector(raw, consumer=self)

    def normalize(self, selector: ScopeSelector) -> ScopeSelector:
        """Return the canonical validated form for this consumer."""
        return _normalize_scope_selector(selector, consumer=self)


POLICY_RULE_SCOPES = ScopeConsumer(
    "policy-rule", frozenset({ScopeAxis.PATHS, ScopeAxis.EXTENSIONS})
)
STUDY_PROVIDER_SCOPES = ScopeConsumer("study-provider", frozenset({ScopeAxis.PATHS}))
PRE_COMMIT_STEP_SCOPES = ScopeConsumer(
    "pre-commit-step",
    frozenset(
        {
            ScopeAxis.PATHS,
            ScopeAxis.DRIVERS,
            ScopeAxis.MODELS,
            ScopeAxis.PHASES,
        }
    ),
)
WRAPPER_SCOPES = ScopeConsumer("wrapper", frozenset({ScopeAxis.DRIVERS}))
WRAPPER_ROUTE_SCOPES = ScopeConsumer("wrapper-route", frozenset({ScopeAxis.DRIVERS}))
MAXIM_SCOPES = ScopeConsumer("maxim", frozenset({ScopeAxis.DRIVERS}))

SCOPE_CONSUMERS: Mapping[str, ScopeConsumer] = MappingProxyType(
    {
        consumer.name: consumer
        for consumer in (
            POLICY_RULE_SCOPES,
            STUDY_PROVIDER_SCOPES,
            PRE_COMMIT_STEP_SCOPES,
            WRAPPER_SCOPES,
            WRAPPER_ROUTE_SCOPES,
            MAXIM_SCOPES,
        )
    }
)
SCOPE_AXIS_CONSUMERS: Mapping[ScopeAxis, tuple[str, ...]] = MappingProxyType(
    {
        axis: tuple(
            consumer.name
            for consumer in SCOPE_CONSUMERS.values()
            if axis in consumer.supported_axes
        )
        for axis in SCOPE_AXIS_ORDER
    }
)

# Inventory decisions that deliberately remain outside ``scopes``.  Keeping
# them named prevents a future parser from admitting a lookalike axis merely
# because the word already appears in configuration.
NON_SELECTOR_CONCEPTS: Mapping[str, str] = MappingProxyType(
    {
        "commands": "wrapper command heads and flags are routing payload",
        "languages": "language families are classification datasets",
        "roles": "test and generated roles are classification datasets",
        "task-phases": "task phases are live allocator routing state",
        "configuration-layers": (
            "system, repository, and worktree are precedence metadata"
        ),
    }
)


@dataclass(frozen=True)
class ScopeSelector:
    paths: tuple[str, ...] = ()
    drivers: tuple[str, ...] = ()
    models: tuple[str, ...] = ()
    phases: tuple[str, ...] = ()
    extensions: tuple[str, ...] = ()

    def values_for(self, axis: ScopeAxis) -> tuple[str, ...]:
        return getattr(self, axis.value)

    @property
    def constrained_axes(self) -> tuple[ScopeAxis, ...]:
        return tuple(axis for axis in SCOPE_AXIS_ORDER if self.values_for(axis))

    def evaluate(self, context: ScopeContext) -> ScopeEvaluation:
        return _evaluate_scope_selector(self, context)

    def matches(self, context: ScopeContext) -> bool:
        return self.evaluate(context).matched

    def specificity(self, context: ScopeContext | None = None) -> ScopeSpecificity:
        return _scope_selector_specificity(self, context)

    def explain(self, context: ScopeContext) -> str:
        return self.evaluate(context).explanation


@dataclass(frozen=True)
class ScopeContext:
    path: str | PurePath | None = None
    driver: str | None = None
    model: str | None = None
    phase: str | None = None
    extension: str | None = None


@dataclass(frozen=True, order=True)
class ScopeSpecificity:
    """More axes, then fixed axis order, exactness, and fewer alternatives."""

    constrained_axes: int
    path: tuple[bool, PathSpecificity, int]
    extension: tuple[bool, int]
    driver: tuple[bool, int]
    model: tuple[bool, int]
    phase: tuple[bool, int]
    normalized_values: tuple[tuple[str, ...], ...]


@dataclass(frozen=True)
class ScopeEvaluation:
    matched: bool
    specificity: ScopeSpecificity
    explanation: str


def _parse_scope_selector(
    raw: object,
    *,
    consumer: ScopeConsumer,
) -> ScopeSelector:
    """Parse one ``scopes`` inline table for its declared consumer."""
    if raw is None:
        return ScopeSelector()
    if not isinstance(raw, Mapping):
        raise _scope_error(consumer, "must be an inline table")

    names = tuple(sorted(str(key) for key in raw))
    known_names = {axis.value for axis in ScopeAxis}
    unsupported = tuple(
        name
        for name in names
        if name not in known_names or ScopeAxis(name) not in consumer.supported_axes
    )
    if unsupported:
        raise _scope_error(
            consumer,
            f"unsupported axes: {', '.join(unsupported)}",
        )

    selector = ScopeSelector(
        **{
            axis.value: _raw_axis_values(raw[axis.value], axis, consumer)
            for axis in SCOPE_AXIS_ORDER
            if axis.value in raw
        }
    )
    return consumer.normalize(selector)


def _normalize_scope_selector(
    selector: ScopeSelector,
    *,
    consumer: ScopeConsumer,
) -> ScopeSelector:
    """Return the canonical, validated ordering for one typed selector."""
    unsupported = tuple(
        axis.value
        for axis in selector.constrained_axes
        if axis not in consumer.supported_axes
    )
    if unsupported:
        raise _scope_error(
            consumer,
            f"unsupported axes: {', '.join(unsupported)}",
        )
    return ScopeSelector(
        paths=_normalize_paths(selector.paths, consumer),
        drivers=_normalize_drivers(selector.drivers, consumer),
        models=_normalize_models(selector.models),
        phases=_normalize_phases(selector.phases, consumer),
        extensions=_normalize_extensions(selector.extensions, consumer),
    )


def _evaluate_scope_selector(
    selector: ScopeSelector, context: ScopeContext
) -> ScopeEvaluation:
    normalized_context = _normalize_context(context)
    outcomes = tuple(
        (
            axis,
            _axis_matches(selector.values_for(axis), axis, normalized_context),
        )
        for axis in selector.constrained_axes
    )
    matched = all(outcome for _axis, outcome in outcomes)
    return ScopeEvaluation(
        matched=matched,
        specificity=_scope_selector_specificity(selector, normalized_context),
        explanation=_render_scope_evaluation(selector, normalized_context, outcomes),
    )


def _scope_selector_specificity(
    selector: ScopeSelector, context: ScopeContext | None = None
) -> ScopeSpecificity:
    normalized_context = _normalize_context(context or ScopeContext())
    path_patterns = selector.paths
    matching_paths = tuple(
        pattern
        for pattern in path_patterns
        if normalized_context.path
        and matches_repo_path(normalized_context.path, pattern)
    )
    scored_paths = matching_paths or path_patterns
    path_score = max(
        (path_specificity(pattern) for pattern in scored_paths),
        default=PathSpecificity(0, False, 0, 0, 0),
    )
    return ScopeSpecificity(
        constrained_axes=len(selector.constrained_axes),
        path=(
            bool(path_patterns),
            path_score,
            -len(path_patterns) if path_patterns else 0,
        ),
        extension=_axis_narrowness(selector.extensions),
        driver=_axis_narrowness(selector.drivers),
        model=_axis_narrowness(selector.models),
        phase=_axis_narrowness(selector.phases),
        normalized_values=tuple(selector.values_for(axis) for axis in SCOPE_AXIS_ORDER),
    )


def _raw_axis_values(
    raw: object, axis: ScopeAxis, consumer: ScopeConsumer
) -> tuple[str, ...]:
    if (
        not isinstance(raw, Sequence)
        or isinstance(raw, (str, bytes))
        or not raw
        or any(not isinstance(item, str) or not item.strip() for item in raw)
    ):
        raise _scope_error(
            consumer,
            f"axis {axis.value!r} must be a non-empty list of non-empty strings",
        )
    return tuple(raw)


def _normalize_paths(values: Sequence[str], consumer: ScopeConsumer) -> tuple[str, ...]:
    normalized: list[str] = []
    for raw in values:
        value = normalize_repo_path(raw)
        parts = PurePosixPath(value).parts
        drive_qualified = bool(parts and len(parts[0]) == 2 and parts[0].endswith(":"))
        if (
            value in {"", "."}
            or value.startswith("/")
            or drive_qualified
            or ".." in parts
        ):
            raise _scope_error(
                consumer,
                f"axis 'paths' contains a non-repository-relative selector: {raw!r}",
            )
        normalized.append(value)
    return tuple(sorted(set(normalized)))


def _normalize_drivers(
    values: Sequence[str], consumer: ScopeConsumer
) -> tuple[str, ...]:
    if not values:
        return ()
    from spice.agent.driver import driver_scope_choices

    known = frozenset(driver_scope_choices())
    normalized = tuple(sorted({_normalize_identity(value) for value in values}))
    unknown = tuple(value for value in normalized if value not in known)
    if unknown:
        raise _scope_error(
            consumer,
            "axis 'drivers' has unknown values: "
            f"{', '.join(unknown)}; expected one of: {', '.join(sorted(known))}",
        )
    return normalized


def _normalize_models(values: Sequence[str]) -> tuple[str, ...]:
    return tuple(sorted({_normalize_identity(value) for value in values}))


def _normalize_phases(
    values: Sequence[str], consumer: ScopeConsumer
) -> tuple[str, ...]:
    if not values:
        return ()
    normalized = tuple(
        sorted({value.strip().casefold().replace("_", "-") for value in values})
    )
    unknown = tuple(
        value for value in normalized if value not in PRE_COMMIT_SCOPE_PHASES
    )
    if unknown:
        raise _scope_error(
            consumer,
            "axis 'phases' has unknown values: "
            f"{', '.join(unknown)}; expected one of: "
            f"{', '.join(PRE_COMMIT_SCOPE_PHASES)}",
        )
    return normalized


def _normalize_extensions(
    values: Sequence[str], consumer: ScopeConsumer
) -> tuple[str, ...]:
    normalized: list[str] = []
    for raw in values:
        value = raw.strip().casefold()
        if (
            len(value) < 2
            or not value.startswith(".")
            or PurePosixPath(f"file{value}").suffix != value
            or "/" in value
            or "\\" in value
        ):
            raise _scope_error(
                consumer,
                f"axis 'extensions' contains an invalid suffix: {raw!r}",
            )
        normalized.append(value)
    return tuple(sorted(set(normalized)))


def _normalize_context(context: ScopeContext) -> ScopeContext:
    path = normalize_repo_path(context.path) if context.path is not None else ""
    extension = (context.extension or "").strip().casefold()
    if not extension and path:
        extension = PurePosixPath(path).suffix.casefold()
    return ScopeContext(
        path=path,
        driver=_normalize_identity(context.driver or ""),
        model=_normalize_identity(context.model or ""),
        phase=(context.phase or "").strip().casefold().replace("_", "-"),
        extension=extension,
    )


def _normalize_identity(value: str) -> str:
    """Normalize configured and runtime driver/model identities identically."""
    return value.strip().casefold()


def _axis_matches(
    values: tuple[str, ...], axis: ScopeAxis, context: ScopeContext
) -> bool:
    if axis is ScopeAxis.PATHS:
        return bool(context.path) and any(
            matches_repo_path(context.path, pattern) for pattern in values
        )
    actual = str(getattr(context, axis.value.removesuffix("s")) or "")
    return actual in values


def _axis_narrowness(values: tuple[str, ...]) -> tuple[bool, int]:
    return (bool(values), -len(values) if values else 0)


def _render_scope_evaluation(
    selector: ScopeSelector,
    context: ScopeContext,
    outcomes: tuple[tuple[ScopeAxis, bool], ...],
) -> str:
    if not outcomes:
        return "scopes match=true: unconstrained"
    details = []
    for axis, matched in outcomes:
        values = ", ".join(selector.values_for(axis))
        if axis is ScopeAxis.PATHS:
            actual = str(context.path or "<absent>")
        else:
            actual = str(getattr(context, axis.value.removesuffix("s")) or "<absent>")
        details.append(
            f"{axis.value} any-of [{values}] actual={actual} match="
            f"{str(matched).lower()}"
        )
    overall = str(all(outcome for _axis, outcome in outcomes)).lower()
    return f"scopes match={overall}: " + "; ".join(details)


def _scope_error(consumer: ScopeConsumer, detail: str) -> SpiceError:
    supported = ", ".join(consumer.supported_axis_names) or "none"
    return SpiceError(
        f"scopes for consumer {consumer.name!r} (supported axes: {supported}): {detail}"
    )
