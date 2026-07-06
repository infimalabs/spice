# Auto-Compacting Before the Driver Ceiling

Status: implemented, 2026-07-06. Deliverable for COMPACT-1kBNMhg4.

## The problem

From the operator's side a lane is one continuous conversation. Left alone,
Claude Code's own auto-compact did not appear to trigger before the
conversation approached its real (possibly ~1M-token, overflow-tier) API
context ceiling — far later than useful, and with no configuration surface
the operator could find to move it earlier.

## The lever

Claude Code reads `CLAUDE_CODE_AUTO_COMPACT_WINDOW` from its process
environment at launch and takes it as the token count at which it reactively
summarizes the conversation, **taking precedence over its own interactive
`/config` auto-compact setting**. This is a genuine driver-level lever, not a
spice invention — confirmed directly from the installed Claude Code binary's
own strings ("`CLAUDE_CODE_AUTO_COMPACT_WINDOW` is set and takes precedence.
Unset it to change this setting.").

This is distinct from `ClaudeDriver.context_snapshot_fields`'s existing
`model_context_window` pin (always metered against the 200K standard tier,
so *reported* pressure builds even on a session actually running in a larger
overflow context). That pin only affects what spice *displays* as pressure —
it does not feed back into Claude Code's own compaction decision, which is
exactly why compaction could still run toward the real, much larger ceiling
despite pressure reading past 100% in spice's own metering.

## The wiring

`spice/agent/driver.py`:
- `CLAUDE_AUTO_COMPACT_WINDOW_ENV = "CLAUDE_CODE_AUTO_COMPACT_WINDOW"`
- `CLAUDE_AUTO_COMPACT_WINDOW_TOKENS = 140_000` — comfortably under the 200K
  standard-tier ceiling the pressure meter already targets, so compaction
  fires before that reported pressure reads 100%.
- `claude_auto_compact_environment(repo_root, *, base_env)` returns
  `{CLAUDE_AUTO_COMPACT_WINDOW_ENV: "140000"}` only when the worktree's
  configured driver is Claude, and only when the variable is not already
  present in `base_env` — an explicit override (operator- or
  parent-process-set) always wins; this only ever supplies a default.

`spice/agent/lifecycle.py`'s `agent_environment()` merges this in for every
launch path (`spawn_agent`, the supervised `spawn_agent_supervisor` →
`run_agent_supervisor` chain, and the generic `agent_supervisor_environment`
callers) since all of them build their process environment from this one
function.

## Overriding

Set `CLAUDE_CODE_AUTO_COMPACT_WINDOW` explicitly in the operator's own shell
or launch environment before starting the agent to use a different value (or
Claude Code's own `auto` setting) instead of spice's 140K default — spice's
own addition backs off entirely once that variable is already set.
