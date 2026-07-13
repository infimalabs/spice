# Agent Hook Systems: two mechanisms, both live

Status: investigative map, 2026-07-06. Deliverable for WIRING-1kBMc8BV. Grounded
in this checkout's code (paths/line refs verified in-tree), not recalled.

The operator's question was: "we write hooks... but is the agent hook under the
worktree Git dir, and is it actually being used?" Answer: there are **two
independent hook systems**, they live in different places, fire at different
times, and **both are live**. They are easy to conflate because both are "spice
hooks," but they share no machinery.

| | Git gate-hooks | Agent driver PostToolUse hook |
|---|---|---|
| Purpose | Quality gates at git operations | Deliver inbox steering + keep-working guidance to the agent |
| Files | `.spice/hooks/{pre-commit, commit-msg, reference-transaction}` (shims) | `<worktree-git-dir>/.spice/agents/<driver>-post-tool-hook.json` (a **descriptor/record**) |
| Wired by | `install_hooks_for_repo` sets worktree `core.hooksPath=.spice/hooks` | `claude_settings_json` embeds `post_tool_hook_settings` in the agent's **launch settings** |
| Fires | On `git commit` (pre-commit, commit-msg) and ref updates (reference-transaction) | After **every** native tool call (`matcher: "*"`) |
| Runs | The shim → `spice dev pre-commit` etc. | `spice agent post-tool-hook --repo-root ...` |
| Output | Pass/fail gate (blocks the commit) | `hookSpecificOutput.additionalContext` = the "Inbox Steering" block |
| Proof it's live | `spice agent activation` lists `dev_hooks_detail=hook ... core.hooksPath=.spice/hooks` | The agent reads the "Inbox Steering" block after tool calls (including non-Bash, e.g. `Read`) |

## System 1 — git gate-hooks (worktree git config)

`install_hooks_for_repo` (`spice/hooks/install.py:41`) writes shim scripts into
`.spice/hooks/` and, if needed, points the **worktree-local** git config
`core.hooksPath` at that directory (`install.py:53-57`). Git then runs those shims
at commit time (`pre-commit`, `commit-msg`) and on reference updates
(`reference-transaction`). The shims dispatch into `spice dev ...` gate logic
(file-loc, complexity, magic-numbers, append-only / flex-slice enforcement). This
is the system that blocked an over-cap `test_lifecycle.py` edit and redirected it
to a fresh seam.

Nothing about this system touches the agent's inbound content — it is purely a
commit-time guard, wired through git config, and only visible to the agent when a
commit is attempted or when `spice agent activation` enumerates it.

## System 2 — agent driver PostToolUse hook (agent launch settings)

This is the channel that carries live operator steering, and the one the operator
was unsure about. Two artifacts, and only one of them is authoritative:

- **The descriptor**
  `<worktree-git-dir>/.spice/agents/<driver>-post-tool-hook.json` is *written*
  by `write_post_tool_hook_config` (`spice/agent/driver.py:276-292`) as a record
  of the hook's shape (driver, event, matcher, command, timeout, capability). It
  is an inspection/record artifact — reading it tells you what the hook does,
  but the running agent is **not** driven by this file.
- **The live registration** is `post_tool_hook_settings`
  (`driver.py:236-252`), which builds the Claude `hooks.PostToolUse` group (matcher
  `*`, `command = spice agent post-tool-hook --repo-root <root>`) and is embedded in
  the agent's launch settings by `claude_settings_json` (`driver.py:488-496`). The
  Codex driver gets the equivalent as TOML overrides via
  `post_tool_hook_codex_config_overrides` (`driver.py:255-273`). Both are derived
  from the same payload as the descriptor, so the descriptor never drifts from the
  live hook — it is generated alongside it (`post_tool_hook_settings` calls
  `write_post_tool_hook_config`, `driver.py:237`).

At runtime the harness runs the hook after every native tool call and feeds its
`additionalContext` back to the agent as the "Inbox Steering" block (pending keys,
`control=`/`note=` lines, ACK/NACK format, persistence rules). Observed directly:
it fires after non-Bash tools too — a `Read` of `SKILL.md` this session produced the
block — so it is genuinely "every tool call," not "every Bash result."

## The one sentence that removes the ambiguity

`.spice/hooks/` is git's hook dir (commit-time gates, wired by `core.hooksPath`);
`<worktree-git-dir>/.spice/agents/<driver>-post-tool-hook.json` is a **record**
of the PostToolUse hook whose live copy rides the agent's launch settings
(every-tool-call steering delivery). Neither is dead: the first gates your
commits, the second is how operator steering reaches you.
