# Auto-Compacting Before the Driver Ceiling

Status: implemented contract, 2026-07-15. Deliverable for COMPACT-1kBNMhg4.

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
`model_context_window` pin (always metered against the configured 250K
ceiling, so *reported* pressure builds even on a session actually running in
a larger overflow context). That pin only affects what spice *displays* as
pressure — it does not feed back into Claude Code's own compaction decision,
which is exactly why compaction could still run toward the real, much larger
ceiling despite pressure reading past 100% in spice's own metering.

## The wiring

`spice/agent/driver.py`:
- `CLAUDE_AUTO_COMPACT_WINDOW_ENV = "CLAUDE_CODE_AUTO_COMPACT_WINDOW"`
- `CLAUDE_AUTO_COMPACT_WINDOW_TOKENS` — read from layered config as
  `agent.claude.auto_compact_window_tokens`; the packaged default in
  `spice/spice.toml` is `250000`, capping the 1M overflow tier back down to
  the 250K configured window the pressure meter already targets, so a
  long-running lane compacts at that tier ceiling instead of running toward
  the real one.
- `claude_auto_compact_environment(repo_root, *, base_env)` returns
  `{CLAUDE_AUTO_COMPACT_WINDOW_ENV: str(CLAUDE_AUTO_COMPACT_WINDOW_TOKENS)}`
  only when the worktree's
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
Claude Code's own `auto` setting) instead of spice's configured default —
spice's own addition backs off entirely once that variable is already set.
The default itself moves through layered config as
`agent.claude.auto_compact_window_tokens`.

## Codex

Extended 2026-08-20 to cover Codex's parallel lever (ad hoc operator request,
not a tracked task deliverable).

Codex CLI has no environment-variable seam for this the way Claude does. Its
own documented lever is the `model_auto_compact_token_limit` config.toml key
— confirmed against the installed binary with `--strict-config` (which
errors on any key the config schema does not recognize; this one passed
clean), and documented at
https://learn.chatgpt.com/docs/config-file/config-reference. `CodexDriver`
sends it as a `-c model_auto_compact_token_limit=<value>` override baked
directly into the `exec` command built by `build_exec_command`, the same way
every other Codex-side config override in this driver already travels
(`model_reasoning_effort`, `personality`, the Playwright MCP registration,
the PostToolUse hook).

This is deliberately **not** the sibling `model_context_window` key, even
though that key looks like the more literal analogue of "context window."
Setting `model_context_window` is a confirmed, currently open upstream bug:
after the first context overflow it poisons Codex's own compaction-trigger
token accounting (`fill_to_context_window` in `protocol/src/protocol.rs`
rewrites `last_token_usage.total_tokens` to a near-zero delta), so the
compaction check never sees enough usage to fire again — a permanent crash
loop, reported specifically in the headless `exec` mode spice drives
(openai/codex#16068, duplicate of #16033, both open with no linked fix as of
this writing). That is exactly the failure this lever exists to prevent, so
`model_context_window` must never be set by spice.

`spice/agent/driver.py`:
- `CODEX_AUTO_COMPACT_WINDOW_TOKENS` — read from layered config as
  `agent.codex.auto_compact_window_tokens`; the packaged default in
  `spice/spice.toml` is `250000`, matching Claude's configured ceiling so
  both drivers compact at the same operator-chosen point.
- `codex_auto_compact_config_overrides(repo_root)` returns
  `[f"model_auto_compact_token_limit={window}"]`, reading `window` from the
  layered `configured_codex_auto_compact_window(repo_root)` resolver.
- `CodexDriver.build_exec_command` splices that override into the same
  `config_overrides` list as every other Codex `-c` flag.

Unlike Claude's environment variable, a Codex `-c/--config` override is
unconditional by the CLI's own documented contract ("Override a
configuration value that would otherwise be loaded from
`~/.codex/config.toml`") — there is no "leave it alone if the operator
already set it" branch to write, matching how every other override in
`build_exec_command` (reasoning effort, personality, the hook and MCP
registrations) already behaves. An operator who wants a different threshold
overrides spice's own configured default through layered config
(`agent.codex.auto_compact_window_tokens`) rather than through Codex's own
config.toml, since spice's `-c` flag wins regardless of what that file says.
