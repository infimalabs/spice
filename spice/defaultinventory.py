"""Classification inventory for exported runtime defaults."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from typing import Callable

TOML_STATIC = "TOML-static"
PLATFORM_DERIVED = "platform-derived"
DRIVER_DERIVED = "driver-derived"
PROTOCOL_INVARIANT = "protocol-invariant"
CLASSIFICATIONS = frozenset(
    {TOML_STATIC, PLATFORM_DERIVED, DRIVER_DERIVED, PROTOCOL_INVARIANT}
)


def _normalize_path(value: object) -> object:
    return str(value)


def _normalize_hidden_project(value: object) -> object:
    return value.removeprefix(".") if isinstance(value, str) else value


def _normalize_prompt_template(value: object) -> object:
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return "\n".join(str(line) for line in value) + "\n"
    return value


def _normalize_task_reports(value: object) -> object:
    if not isinstance(value, Mapping):
        return value
    normalized = {}
    for name, raw in value.items():
        if isinstance(raw, Mapping):
            normalized[str(name)] = (
                raw.get("description"),
                raw.get("filter"),
                raw.get("sort"),
            )
        else:
            normalized[str(name)] = tuple(raw) if isinstance(raw, tuple) else raw
    return normalized


TOML_STATIC_NORMALIZERS: dict[str, Callable[[object], object]] = {
    "spice.agent.maxims.DEFAULT_PROMPT_TEMPLATE": _normalize_prompt_template,
    "spice.resourcelocks.LOCK_STATE_ROOT": _normalize_path,
    "spice.tasks.config.OOPS_PROJECT": _normalize_hidden_project,
    "spice.tasks.config.REPORTS": _normalize_task_reports,
}


# A dotted Python export maps to the packaged TOML leaf that owns its value.
TOML_STATIC_EXPORT_PATHS = {
    "spice.config.values.SAY_BACKEND_CHOICES": "say.backend_choices",
    "spice.config.values.DEFAULT_SAY_BACKEND": "say.backend",
    "spice.config.values.DEFAULT_EXTERNAL_SAY_CONTENT_TYPE": "say.content_type",
    "spice.config.values.DEFAULT_SAY_WORDS_PER_MINUTE": "say.words_per_minute",
    "spice.config.values.DEFAULT_SAY_TIMEOUT_SECONDS": "say.timeout_seconds",
    "spice.config.values.AGENT_PERSONALITY_CHOICES": "agent.personality_choices",
    "spice.config.values.DEFAULT_AGENT_PERSONALITY": "agent.personality",
    "spice.config.values.DEFAULT_JUDGE_BIN": "judge.bin",
    "spice.config.values.PORTABLE_JUDGE_BIN": "judge.portable_bin",
    "spice.config.values.DEFAULT_RTK_EXECUTABLE": "rtk.executable",
    "spice.policy.FILE_LOC_LIMIT": "policy.limits.file_loc",
    "spice.policy.FILE_BYTE_LIMIT": "policy.limits.file_bytes",
    "spice.policy.COMPLEXITY_MAX_CCN": "policy.limits.routine_ccn",
    "spice.policy.COMPLEXITY_MAX_LENGTH": "policy.limits.routine_length",
    "spice.policy.COMPLEXITY_HOTSPOT_LIMIT": "policy.complexity.hotspot_limit",
    "spice.policy.COMMIT_MESSAGE_WRAP_LIMIT": "policy.limits.commit_message_wrap",
    "spice.policy.TASTE_WORD_SUGGESTIONS": "policy.taste.words",
    "spice.policy.REPO_TRUTH_DOC_LIMIT": "policy.limits.repo_truth_doc_chars",
    "spice.policy.REPO_TRUTH_DOCS": "policy.repo_truth.docs",
    "spice.policy.MARKDOWN_DEPTH_DOC_EXTENSIONS": "policy.markdown_depth_budget.extensions",
    "spice.policy.MARKDOWN_DEPTH_BASE_CHAR_BUDGET": "policy.markdown_depth.base_chars",
    "spice.policy.MARKDOWN_DEPTH_MAX_BOUNDED_CHAR_BUDGET": "policy.markdown_depth.max_bounded_chars",
    "spice.policy.BOUNDARY_UNDERSCORE_PATTERN": "policy.package.boundary_underscore_pattern",
    "spice.policy.REACHABILITY_TEST_ONLY_LIMIT": "policy.debt.reachability_test_only",
    "spice.policy.ASSERTION_FREE_TEST_LIMIT": "policy.debt.assertion_free_tests",
    "spice.policy.MAGIC_BASELINE_REF": "policy.magic.baseline_ref",
    "spice.policy.MAGIC_EXAMINE_VALUE_THRESHOLD": "policy.magic.examine_threshold",
    "spice.policy.ENV_POLICY_ALLOW_MARKER": "policy.env.allow_marker",
    "spice.policy.ENV_POLICY_DEFAULT_NAME_PATTERNS": "policy.env.default_name_patterns",
    "spice.policy.ENV_POLICY_SELF_PATH_SUFFIX": "policy.env.self_path_suffix",
    "spice.policy.ENV_ACCESS_FAMILY_SUFFIXES": "policy.env_access.family_suffixes",
    "spice.policy.ENV_ACCESS_DEFAULT_PATTERNS": "policy.env_access.default_patterns",
    "spice.policy.ENV_ACCESS_FINDING_NAMES": "policy.env_access.finding_names",
    "spice.policy.C_GRAMMAR_SUFFIXES": "policy.languages.c_grammar",
    "spice.policy.COMPLEXITY_SUFFIXES": "policy.languages.complexity",
    "spice.policy.MAGIC_SUFFIXES": "policy.languages.magic",
    "spice.policy.ENV_SUFFIXES": "policy.languages.env",
    "spice.policy.FILE_SHAPE_GENERATED_LOCKFILE_SUFFIXES": "policy.lockfiles.suffixes",
    "spice.policy.FILE_SHAPE_GENERATED_LOCKFILE_NAMES": "policy.lockfiles.names",
    "spice.policy.FILE_SHAPE_SOURCE_SUFFIXES": "policy.file_shape.source_suffixes",
    "spice.policy.FILE_SHAPE_GENERATED_SOURCE_PATTERNS": "policy.file_shape.generated_patterns",
    "spice.policyconfig.FLEX_JITTER_PERCENT": "policy.flex.jitter_percent",
    "spice.tasks.config.DEFAULT_PROJECT_MIN_DEPTH": "tasks.project_min_depth",
    "spice.tasks.config.DEFAULT_PROJECT_MAX_DEPTH": "tasks.project_max_depth",
    "spice.tasks.config.BASE_APPROVED_STEMS": "tasks.base_stems",
    "spice.tasks.config.INTERNAL_STEMS": "tasks.internal_stems",
    "spice.tasks.config.MAXIM_PROPOSAL_HIDDEN_STEM": (
        "tasks.maxim_proposal_hidden_stem"
    ),
    "spice.tasks.config.BASE_HIDDEN_STEMS": "tasks.hidden_stems",
    "spice.tasks.config.APPROVED_PHASES": "tasks.approved_phases",
    "spice.tasks.config.PHASE_SLOT_COUNT": "tasks.phase_slot_count",
    "spice.tasks.config.DEFAULT_FLOW": "tasks.default_flow",
    "spice.tasks.config.PRIVATE_DEFAULT_FLOW": "tasks.private_default_flow",
    "spice.tasks.config.OOPS_DEFAULT_FLOW": "tasks.oops_default_flow",
    "spice.tasks.config.DEFAULT_PRIORITY": "tasks.default_priority",
    "spice.tasks.config.PRIORITY_MAP": "tasks.priority",
    "spice.tasks.config.PRIORITY_URGENCY": "tasks.priority_urgency",
    "spice.tasks.config.TASKWARRIOR_URGENCY": "tasks.taskwarrior_urgency",
    "spice.tasks.config.ALLOCATOR_BAND_WIDTH": "tasks.allocator_band_width",
    "spice.tasks.config.ALLOCATOR_ANTI_SELF_REVIEW": "tasks.allocator_anti_self_review",
    "spice.tasks.config.SEVERITY_PRIORITY": "tasks.severity_priority",
    "spice.tasks.config.SEVERITIES": "tasks.severities",
    "spice.tasks.config.SEVERITY_SHORTHANDS": "tasks.severity_shorthands",
    "spice.tasks.config.SLA_DUE_SECONDS": "tasks.sla_due_seconds",
    "spice.tasks.config.CLAIM_TTL_SECONDS": "tasks.claim_ttl_seconds",
    "spice.tasks.config.CLAIM_CONTEXT_SECONDS": "tasks.claim_context_seconds",
    "spice.tasks.config.DEFERRED_WAIT": "tasks.deferred_wait",
    "spice.tasks.config.OOPS_WAIT_SECONDS": "tasks.oops_wait_seconds",
    "spice.tasks.config.OOPS_PROJECT": "tasks.oops_hidden_stem",
    "spice.tasks.config.REPORTS": "tasks.reports",
    "spice.tasks.config.ANALYTICS_COMMANDS": "tasks.analytics.commands",
    "spice.agent.driver.PLAYWRIGHT_MCP_SERVER_NAME": "agent.playwright_mcp.server_name",
    "spice.agent.driver.PLAYWRIGHT_MCP_COMMAND": "agent.playwright_mcp.command",
    "spice.agent.driver.PLAYWRIGHT_MCP_ARGS": "agent.playwright_mcp.args",
    "spice.agent.driver.CLAUDE_DEFAULT_MODEL": "agent.claude.default_model",
    "spice.agent.driver.CLAUDE_AUTO_COMPACT_WINDOW_TOKENS": "agent.claude.auto_compact_window_tokens",
    "spice.agent.judgeadapter.DEFAULT_JUDGE_MODEL": "judge.model",
    "spice.agent.judgeadapter.DEFAULT_MODEL_COMMAND": "judge.model_command",
    "spice.agent.judgeadapter.DEFAULT_TIMEOUT_SECONDS": "judge.timeout_seconds",
    "spice.resourcelocks.DEFAULT_LOCK_CONTENTION_EXIT_CODE": "locks.lock_contention_exit_code",
    "spice.resourcelocks.DEFAULT_CHOSEN_SHARD_CONTENTION_EXIT_CODE": "locks.chosen_shard_contention_exit_code",
    "spice.resourcelocks.DEFAULT_POOL_EXHAUSTION_EXIT_CODE": "locks.pool_exhaustion_exit_code",
    "spice.resourcelocks.LOCK_STATE_ROOT": "locks.state_root",
    "spice.agent.maxims.DEFAULT_MAX_ATTEMPTS": "maxim.max_attempts",
    "spice.agent.maxims.PARALLEL_MAXIM_JUDGES": "maxim.parallel_judges",
    "spice.agent.maxims.MAXIM_PROPOSAL_MIN_RECURRENCE": "maxim.proposal_min_recurrence",
    "spice.agent.maxims.MAXIM_PROPOSAL_DRAFT_MAX_WORDS": "maxim.proposal_draft_max_words",
    "spice.agent.maxims.DEFAULT_PROMPT_LINES": "maxim.prompt_lines",
    "spice.agent.maxims.DEFAULT_PROMPT_TEMPLATE": "maxim.prompt_lines",
    "spice.serve.team.schema.DEFAULT_LIFETIME": "serve.default_lifetime",
    "spice.serve.web.DEFAULT_BRAND": "serve.brand",
    "spice.serve.web.DEFAULT_LIFETIME": "serve.default_lifetime",
    "spice.serve.web.VALID_LIFETIMES": "serve.valid_lifetimes",
    "spice.serve.app.DEFAULT_SERVE_HOST": "serve.host",
    "spice.serve.app.DEFAULT_SERVE_PORT": "serve.port",
}

EXPORTED_DEFAULT_CLASSIFICATION = {
    **{name: TOML_STATIC for name in TOML_STATIC_EXPORT_PATHS},
    "spice.config.values.default_judge_bin": PLATFORM_DERIVED,
    "spice.paths.runtime_spice_source": PLATFORM_DERIVED,
    "spice.agent.driver.BUILTIN_DRIVERS": DRIVER_DERIVED,
    "spice.agent.driver.DRIVER": DRIVER_DERIVED,
    "spice.agent.driver.CLAUDE_EFFORT_CHOICES": DRIVER_DERIVED,
    "spice.cli.parser.BUILTIN_COMMANDS": PROTOCOL_INVARIANT,
    "spice.tasks.config.PROJECT_DELIMITER": PROTOCOL_INVARIANT,
    "spice.tasks.config.SENTINEL_ACTOR": PROTOCOL_INVARIANT,
    "spice.resourcelocks.MAX_EXIT_CODE": PROTOCOL_INVARIANT,
    "spice.policy.COMMIT_MESSAGE_ALLOWED_TRAILER_KEYS": PROTOCOL_INVARIANT,
    "spice.policy.COMMIT_MESSAGE_BLOCKED_TRAILER_KEYS": PROTOCOL_INVARIANT,
    "spice.policy.LEGITIMATE_INTERNAL_COUPLINGS": PROTOCOL_INVARIANT,
    "spice.agent.maximcli.DEFAULT_OUTPUT_FORMAT": PROTOCOL_INVARIANT,
    "spice.process.git.DEFAULT_GIT_TIMEOUT_SECONDS": PROTOCOL_INVARIANT,
    "spice.serve.audio.DEFAULT_SAY_RATE_MULTIPLIER": PROTOCOL_INVARIANT,
    "spice.serve.livebus.DEFAULT_BUS_MESSAGE_LIMIT": PROTOCOL_INVARIANT,
    "spice.serve.messages.DEFAULT_MESSAGE_LIMIT": PROTOCOL_INVARIANT,
    "spice.serve.team.schema.DEFAULT_STUCK_THRESHOLD_SECONDS": PROTOCOL_INVARIANT,
    "spice.sessions.briefing.DEFAULT_BRIEFING_MAX_BYTES": PROTOCOL_INVARIANT,
    "spice.sessions.briefing.DEFAULT_BRIEFING_MAX_LINES": PROTOCOL_INVARIANT,
    "spice.sessions.briefing.DEFAULT_HORIZON_COMPACTIONS": PROTOCOL_INVARIANT,
    "spice.sessions.briefing.DEFAULT_RECENCY_MAX_SECONDS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_COMMANDS_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_COMMAND_TEXT_CHARS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_COMPACTIONS_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_COMMITS_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_MESSAGES_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_MESSAGE_TEXT_CHARS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_PHASE_EXAMPLES": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_PHASE_TEXT_CHARS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_SLICES_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_SLICE_TEXT_CHARS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_SUMMARY_RECENT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_SWEEP_WINDOWS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_TIMELINE_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_TIMELINE_TEXT_CHARS": PROTOCOL_INVARIANT,
    "spice.sessions.cli.DEFAULT_TURNS_LIMIT": PROTOCOL_INVARIANT,
    "spice.sessions.deadline.DEFAULT_REHYDRATION_DEADLINE_SECONDS": PROTOCOL_INVARIANT,
    "spice.studies.csharpmembers.DEFAULT_MEMBER_LIMIT": PROTOCOL_INVARIANT,
    "spice.studies.mutations.DEFAULT_MAX_MUTANTS_PER_MODULE": PROTOCOL_INVARIANT,
    "spice.studies.mutations.DEFAULT_MUTATION_TIMEOUT_SECONDS": PROTOCOL_INVARIANT,
    "spice.studies.shape.DEFAULT_NAME_CLUSTER_THRESHOLD": PROTOCOL_INVARIANT,
    "spice.tasks.artifacts.DEFAULT_RETENTION": PROTOCOL_INVARIANT,
    "spice.tasks.graphs.handout.DEFAULT_OUTPUT": PROTOCOL_INVARIANT,
}
