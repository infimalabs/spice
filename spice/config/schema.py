"""Structural key schema for every layered Spice configuration table."""

from __future__ import annotations

from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from spice.errors import SpiceError


@dataclass(frozen=True)
class _Missing:
    """Sentinel distinguishing a closed table from an open table of leaves."""


_MISSING = _Missing()


@dataclass(frozen=True)
class TableSchema:
    """Known children and, when present, one schema for arbitrary data keys."""

    children: Mapping[str, Schema]
    wildcard: Schema | _Missing = _MISSING


@dataclass(frozen=True)
class SequenceSchema:
    """Schema for table values contained in a TOML array."""

    item: Schema


type Schema = TableSchema | SequenceSchema | None


def _table(
    keys: Iterable[str] = (),
    *,
    children: Mapping[str, Schema] | None = None,
    wildcard: Schema | _Missing = _MISSING,
) -> TableSchema:
    values: dict[str, Schema] = {key: None for key in keys}
    if children is not None:
        values.update(children)
    return TableSchema(values, wildcard)


def _records(
    keys: Iterable[str] = (),
    *,
    children: Mapping[str, Schema] | None = None,
) -> SequenceSchema:
    return SequenceSchema(_table(keys, children=children))


_DRIVER_SCOPES = _table(("drivers",))
_PATH_SCOPES = _table(("paths",))
_POLICY_RULE_SCOPES = _table(("paths", "extensions"))
_PRE_COMMIT_SCOPES = _table(("paths", "drivers", "models", "phases"))

_WRAPPER_ROUTE = _table(
    ("head", "flags", "keep", "search_operands", "argv"),
    children={"scopes": _DRIVER_SCOPES},
)
_WRAPPER_ENTRY = _table(
    ("argv",),
    children={
        "scopes": _DRIVER_SCOPES,
        "match": SequenceSchema(_WRAPPER_ROUTE),
    },
)
_WRAPPER_GROUP = _table(
    children={"scopes": _DRIVER_SCOPES},
    wildcard=_WRAPPER_ENTRY,
)

_RULE_SETTINGS = _table(("multiplier", "min", "max", "unlimited", "flex"))
_POLICY_BOUND_KEYS = (
    "file_loc",
    "file_bytes",
    "routine_ccn",
    "routine_length",
    "commit_message_wrap",
    "repo_truth_doc_chars",
)
_POLICY_RULE = _table(
    ("multiplier", "min", "max", "unlimited", "flex"),
    children={
        "scopes": _POLICY_RULE_SCOPES,
        "magic": _table(("examine_threshold",)),
        **{key: _RULE_SETTINGS for key in _POLICY_BOUND_KEYS},
    },
)
_COMMAND_STEP = _table(
    ("label", "mount", "run", "argv", "formatter", "enabled"),
    children={"scopes": _PRE_COMMIT_SCOPES},
)
_PRE_COMMIT_BUILTIN_NAMES = (
    "merge-integrity",
    "plan-phase",
    "repo-shape",
    "staging",
    "repo-docs",
    "formatters",
    "local-paths",
    "taste",
    "serve-web-typecheck",
    "javascript-unused",
    "python-typecheck",
    "env-policy",
    "env-name-ledger",
    "file-shape",
    "complexity",
    "magic-numbers",
    "markdown-links",
    "reachability",
    "symbol-reachability",
    "python-unused",
    "assertion-free-tests",
    "private-internals",
)
_PRE_COMMIT_BUILTIN_KEYS = tuple(
    dict.fromkeys(
        form
        for key in _PRE_COMMIT_BUILTIN_NAMES
        for form in (key, key.replace("-", "_"), key.replace("-", " "))
    )
)
_PRE_COMMIT_BUILTINS = _table(
    children={key: _COMMAND_STEP for key in _PRE_COMMIT_BUILTIN_KEYS}
)

_POLICY = _table(
    (
        "package_roots",
        "name_cluster_threshold",
        "exclude",
        "generated_paths",
        "test_paths",
        "repo_truth_docs",
        "env_name_patterns",
        "env_names",
        "env_access_gate",
        "python_typecheck_interpreter",
        "assertion_helpers",
    ),
    children={
        "limits": _table(_POLICY_BOUND_KEYS),
        "flex": _table(("ratio", "jitter_percent", *_POLICY_BOUND_KEYS[:4])),
        "complexity": _table(("hotspot_limit",)),
        "taste": _table(children={"words": _table(wildcard=None)}),
        "repo_truth": _table(("docs",)),
        "markdown_depth_budget": _table(("extensions", "stem_pattern")),
        "markdown_depth": _table(("base_chars", "max_bounded_chars")),
        "package": _table(("boundary_underscore_pattern",)),
        "debt": _table(("reachability_test_only", "assertion_free_tests")),
        "magic": _table(("baseline_ref", "examine_threshold")),
        "env": _table(("allow_marker", "default_name_patterns", "self_path_suffix")),
        "env_access": _table(
            ("baseline",),
            children={
                "family_suffixes": _table(wildcard=None),
                "default_patterns": _table(wildcard=None),
                "finding_names": _table(wildcard=None),
            },
        ),
        "languages": _table(("c_grammar", "complexity", "magic", "env")),
        "lockfiles": _table(("suffixes", "names")),
        "file_shape": _table(("source_suffixes", "generated_patterns")),
        "commit_message": _table(("allowed_trailers", "blocked_trailers")),
        "rules": SequenceSchema(_POLICY_RULE),
        "suite_seam": _table(("paths", "run", "seconds")),
        "csharp_unused_retention": _table(
            ("base_types", "interfaces", "attribute_names")
        ),
        "internal_couplings": _records(("path", "test", "target")),
        "reachability_providers": _records(
            ("name", "run"),
            children={"scopes": _PATH_SCOPES},
        ),
        "pre_commit": SequenceSchema(_COMMAND_STEP),
        "pre_commit_success": SequenceSchema(_COMMAND_STEP),
        "pre_commit_builtins": _PRE_COMMIT_BUILTINS,
    },
)

_TASK_PHASE_MODEL = _table(("model", "effort"))
_TASK_REPORT = _table(("description", "filter", "sort"))
_TASKS = _table(
    (
        "base_stems",
        "stems",
        "internal_stems",
        "hidden_stems",
        "oops_hidden_stem",
        "maxim_proposal_hidden_stem",
        "approved_phases",
        "phase_slot_count",
        "default_flow",
        "private_default_flow",
        "oops_default_flow",
        "default_priority",
        "severities",
        "project_min_depth",
        "project_max_depth",
        "claim_ttl_seconds",
        "claim_context_seconds",
        "deferred_wait",
        "oops_wait_seconds",
        "allocator_band_width",
        "allocator_anti_self_review",
    ),
    children={
        "priority": _table(wildcard=None),
        "severity_priority": _table(wildcard=None),
        "severity_shorthands": _table(wildcard=None),
        "priority_urgency": _table(wildcard=None),
        "taskwarrior_urgency": _table(wildcard=None),
        "sla_due_seconds": _table(wildcard=None),
        "reports": _table(wildcard=_TASK_REPORT),
        "analytics": _table(("commands",)),
        "flows": _table(wildcard=None),
        "phase_models": _table(wildcard=_table(wildcard=_TASK_PHASE_MODEL)),
    },
)

_LOCKS = _table(
    (
        "lock_contention_exit_code",
        "chosen_shard_contention_exit_code",
        "pool_exhaustion_exit_code",
        "state_root",
    ),
    children={
        "named": _table(wildcard=_table(("path", "contention_exit_code"))),
        "pools": _table(
            wildcard=_table(
                (
                    "directory",
                    "shards",
                    "chosen_shard_contention_exit_code",
                    "pool_exhaustion_exit_code",
                )
            )
        ),
    },
)

CONFIG_SCHEMA = _table(
    children={
        "say": _table(
            (
                "backend",
                "backend_choices",
                "command",
                "content_type",
                "external_content_type",
                "voice",
                "words_per_minute",
                "timeout_seconds",
            )
        ),
        "agent": _table(
            (
                "personality",
                "personality_choices",
                "model",
                "effort",
                "driver",
                "wrappers",
            ),
            children={
                "playwright_mcp": _table(("server_name", "command", "args")),
                "claude": _table(("default_model", "auto_compact_window_tokens")),
            },
        ),
        "judge": _table(
            (
                "bin",
                "enabled",
                "portable_bin",
                "model",
                "model_command",
                "timeout_seconds",
            )
        ),
        "rtk": _table(("executable",)),
        "wrappers": _table(wildcard=_WRAPPER_GROUP),
        "policy": _POLICY,
        "tasks": _TASKS,
        "locks": _LOCKS,
        "commands": _table(wildcard=None),
        "maxim": _table(
            (
                "max_attempts",
                "parallel_judges",
                "proposal_min_recurrence",
                "proposal_draft_max_words",
                "prompt_lines",
            )
        ),
        "maxims": _table(
            wildcard=_table(
                ("words", "message"),
                children={"scopes": _DRIVER_SCOPES},
            )
        ),
        "serve": _table(
            ("brand", "default_lifetime", "valid_lifetimes", "host", "port")
        ),
        "inventory": _table(
            (
                "toml_static",
                "platform_derived",
                "driver_derived",
                "protocol_invariant",
            )
        ),
    }
)


def validate_config_keys(
    values: Mapping[str, Any],
    *,
    source_name: str,
    source_path: Path,
) -> None:
    """Refuse the first structurally unknown key in one source layer."""
    _validate_mapping(
        values,
        CONFIG_SCHEMA,
        source_name=source_name,
        source_path=source_path,
        prefix=(),
    )


def _validate_mapping(
    values: Mapping[str, Any],
    schema: TableSchema,
    *,
    source_name: str,
    source_path: Path,
    prefix: tuple[str, ...],
) -> None:
    for raw_key, value in values.items():
        key = str(raw_key)
        child: Schema | _Missing = schema.children.get(key, _MISSING)
        if isinstance(child, _Missing):
            child = schema.wildcard
        if isinstance(child, _Missing):
            raise _unknown_key_error(
                key,
                prefix=prefix,
                choices=schema.children,
                source_name=source_name,
                source_path=source_path,
            )
        _validate_value(
            value,
            child,
            source_name=source_name,
            source_path=source_path,
            prefix=(*prefix, key),
        )


def _validate_value(
    value: Any,
    schema: Schema,
    *,
    source_name: str,
    source_path: Path,
    prefix: tuple[str, ...],
) -> None:
    if isinstance(schema, TableSchema) and isinstance(value, Mapping):
        _validate_mapping(
            value,
            schema,
            source_name=source_name,
            source_path=source_path,
            prefix=prefix,
        )
        return
    if not isinstance(schema, SequenceSchema) or not isinstance(value, Sequence):
        return
    if isinstance(value, (str, bytes)):
        return
    for index, item in enumerate(value):
        if isinstance(schema.item, TableSchema) and isinstance(item, Mapping):
            item_prefix = (*prefix[:-1], f"{prefix[-1]}[{index}]") if prefix else prefix
            _validate_mapping(
                item,
                schema.item,
                source_name=source_name,
                source_path=source_path,
                prefix=item_prefix,
            )


def _unknown_key_error(
    key: str,
    *,
    prefix: tuple[str, ...],
    choices: Mapping[str, Schema],
    source_name: str,
    source_path: Path,
) -> SpiceError:
    dotted = ".".join((*prefix, key))
    message = (
        f"unknown configuration key {dotted} (source={source_name} path={source_path})"
    )
    suggestion = _nearest_key(key, choices)
    if suggestion is not None:
        suggested_path = ".".join((*prefix, suggestion))
        message += f"; did you mean {suggested_path}?"
    return SpiceError(message)


def _nearest_key(key: str, choices: Mapping[str, Schema]) -> str | None:
    candidates = tuple(candidate for candidate in choices if candidate)
    if not key or not candidates:
        return None
    ranked = sorted(
        (_edit_distance(key.casefold(), candidate.casefold()), candidate)
        for candidate in candidates
    )
    distance, candidate = ranked[0]
    threshold = 1 if min(len(key), len(candidate)) < 4 else 2
    return candidate if distance <= threshold else None


def _edit_distance(left: str, right: str) -> int:
    previous = list(range(len(right) + 1))
    for left_index, left_character in enumerate(left, start=1):
        current = [left_index]
        for right_index, right_character in enumerate(right, start=1):
            current.append(
                min(
                    current[-1] + 1,
                    previous[right_index] + 1,
                    previous[right_index - 1] + (left_character != right_character),
                )
            )
        previous = current
    return previous[-1]
