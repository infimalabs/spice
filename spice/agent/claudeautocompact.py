"""Claude Code's own reactive-compaction threshold, kept configurable.

Split out of spice.agent.driver to keep that module under its file-length
budget, alongside its sibling spice.agent.codexautocompact (Codex's parallel
CODEX_AUTO_COMPACT_WINDOW_TOKENS / codex_auto_compact_config_overrides, split
out for the same reason).
"""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path

from spice import defaults
from spice.agent.driver import CLAUDE_DRIVER, driver_for

# Claude Code reads this at launch and takes it as the token count at which it
# reactively summarizes the conversation, taking precedence over its own
# `/config` auto-compact setting. Left unset, a session can run toward its
# real (possibly ~1M-token overflow-tier) API ceiling before compacting --
# matching the operator's own observation that auto-compact did not appear to
# trigger before ~1M tokens. 250_000 is the configured ceiling that
# ClaudeDriver.context_snapshot_fields (in spice.agent.driver) already meters
# pressure against -- see its "always meter against the configured ceiling"
# comment: the goal is only to cap the 1M overflow tier back down to that
# chosen window, not to compact early, so a long-running lane compacts at the
# tier ceiling without operator intervention.
CLAUDE_AUTO_COMPACT_WINDOW_ENV = "CLAUDE_CODE_AUTO_COMPACT_WINDOW"  # env-policy: allow
CLAUDE_AUTO_COMPACT_WINDOW_TOKENS = defaults.integer(
    "agent", "claude", "auto_compact_window_tokens"
)


def claude_auto_compact_environment(
    repo_root: Path | None, *, base_env: Mapping[str, str]
) -> dict[str, str]:
    """Env addition that gets Claude Code compacting before its real ceiling.

    A no-op for a non-Claude worktree, and a no-op when the operator (or a
    parent process) already set the variable explicitly -- this only supplies
    a default, never overrides one already in play.
    """
    if driver_for(repo_root) is not CLAUDE_DRIVER:
        return {}
    if CLAUDE_AUTO_COMPACT_WINDOW_ENV in base_env:
        return {}
    from spice.config.values import configured_claude_auto_compact_window

    return {
        CLAUDE_AUTO_COMPACT_WINDOW_ENV: str(
            configured_claude_auto_compact_window(repo_root)
        )
    }
