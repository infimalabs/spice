"""Codex CLI's own reactive-compaction threshold, kept configurable.

Split out of spice.agent.driver to keep that module under its file-length
budget, alongside its sibling spice.agent.claudeautocompact (Claude's
parallel CLAUDE_AUTO_COMPACT_WINDOW_TOKENS / claude_auto_compact_environment,
split out for the same reason).
"""

from __future__ import annotations

from pathlib import Path

from spice import defaults

# Codex CLI has no env-var seam for this the way Claude does; its own
# documented lever is the `model_auto_compact_token_limit` config.toml key
# (confirmed against the installed binary with `--strict-config`, and
# documented at https://learn.chatgpt.com/docs/config-file/config-reference),
# read via a `-c` override baked into CodexDriver.build_exec_command by
# codex_auto_compact_config_overrides() below. Deliberately not the sibling
# `model_context_window` key: setting that one instead is a confirmed,
# currently-unresolved upstream bug that poisons Codex's own compaction
# token accounting after the first context overflow, so compaction never
# fires again -- a permanent crash loop specifically in the headless `exec`
# mode spice drives (openai/codex#16068, duplicate of #16033). That is
# exactly the failure this lever exists to prevent, so `model_context_window`
# must never be set here. 250_000 matches Claude's configured ceiling so
# both drivers compact at the same operator-chosen point.
CODEX_AUTO_COMPACT_WINDOW_TOKENS = defaults.integer(
    "agent", "codex", "auto_compact_window_tokens"
)


def codex_auto_compact_config_overrides(repo_root: Path) -> list[str]:
    """Config override that gets Codex compacting before its real ceiling.

    Mirrors Claude's `auto_compact_window_tokens` lever through Codex's own
    `model_auto_compact_token_limit` config key -- see the extended rationale
    on CODEX_AUTO_COMPACT_WINDOW_TOKENS above for why that key, and not the
    superficially similar `model_context_window`, is the one set here.
    """
    from spice.config.values import configured_codex_auto_compact_window

    window = configured_codex_auto_compact_window(repo_root)
    return [f"model_auto_compact_token_limit={window}"]
