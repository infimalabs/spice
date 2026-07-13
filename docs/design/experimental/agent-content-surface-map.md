# Agent Content Surface Map

Status: investigative map, 2026-07-06. Prerequisite artifact for PRESENT-1kBMbq1z;
not a decision, not a proposal to change anything yet.

## Method

Every claim below was checked against direct observation in one live session
(thread `311e4763af1f4163a84042ee497c953e`, driver `claude`, repo root of this
checkout) rather than recalled from training data or prior sessions. Where a
claim rests on config/doc inspection instead of an observed
event in this transcript, it is marked "inspected, not observed firing."

## Surfaces, in delivery order

| # | Surface | Fires on | Lifetime | Channel |
|---|---|---|---|---|
| 1 | Initial skill bootstrap prompt | Session start (agent launch) | Once; the human turn text itself, permanently in transcript | Claude Code user turn, authored by the wrapper |
| 2 | `spice agent activation` output | Explicit command | Command lifetime only; must be re-run to refresh | Bash stdout |
| 3 | `spice session briefing` output | Explicit command | Command lifetime only | Bash stdout |
| 4 | `spice task status` / `spice task next` output | Explicit command | Command lifetime only (task record is authoritative until claim window expires) | Bash stdout |
| 5 | Shell "Working state" banner (`🌶️ Working state: ...`) | Every `spice agent run`-wrapped shell command | Once per command, prepended to that command's output | stderr, injected by the shell reexec wrapper, captured into the same Bash tool result |
| 6 | PostToolUse hook `additionalContext` ("Inbox Steering") | Every native tool call (`matcher: "*"` in `<worktree-git-dir>/.spice/agents/claude-post-tool-hook.json`, running `spice agent post-tool-hook`) | Until the pending key is ACKed/NACKed (redisplays after 15s if untouched); bare reads never clear it | harness `<system-reminder>` attached after the tool result, not stdout |
| 7 | Harness deferred-tool-list reminder | Session start, and again whenever a new deferred tool/MCP server becomes available (observed: `mcp__playwright__*` appeared mid-session once the playwright MCP server finished connecting) | Persists until the tools are fetched via ToolSearch; reappears on new arrivals | harness `<system-reminder>` |
| 8 | Harness "available agent types" / "available skills" reminders | Session start | Static for the session (not seen to change) | harness `<system-reminder>` |
| 9 | `claudeMd` context block (global `CLAUDE.md` protocols + `MEMORY.md` index + `userEmail`/`currentDate`) | Attached to a user turn | Observed once this session; likely re-sent after compaction (not directly confirmed this session) | harness `<system-reminder>` |
| 10 | `gitStatus` snapshot | Session start | Frozen snapshot, does not update | harness `<system-reminder>` |
| 11 | RTK rewrite of tool output | Every discrete shell command run through the wrapper | N/A — it is a transform, not a separate surface | Alters Bash stdout before it reaches the agent |

## Detail per surface

### 1. Initial skill bootstrap prompt

The literal first human turn is `[$spice](.agents/skills/spice/SKILL.md)`
(or equivalent). Per the skill's own **Prompt Boundary** section, this is a
neutral bootstrap signal, not the operator's actual ask — the wrapper
deliberately never puts operator prose here. The real ask has to be recovered
from `spice session briefing` (`Latest Ask` / `Latest Final`) and
`spice task status`/`task next`. Confirmed by direct behavior this session:
the bootstrap prompt carried no task content, and the actual work
(PRESENT-1kBMbq1z) only surfaced after running the three required commands.

### 2-4. The three required commands

`spice agent activation`, `spice session briefing`, `spice task status` are
run once at turn start per the skill file. Their output is command stdout,
not hook-injected, so it only exists at the moment it's printed — nothing
re-pushes it later. Content:

- **activation**: a static contract block (worktree/thread ids, hook wiring,
  work/commit contracts, browser-validation contract, rtk contract,
  interaction-cadence contract, task-command cheat sheet, ack/task capture
  format, prompt-boundary policy). Load-bearing but nearly identical across
  sessions in this repo — mostly a standing-rules dump, high value on first
  read, low marginal value on re-reads within a session.
- **briefing**: dynamic — a time-windowed digest (files/turns/window,
  `keep_working` guidance, `Latest Ask`/`Latest Final` truncated text,
  activity counters, git branch/dirty state, inbox pending/refused list).
  This is the actual per-turn signal surface; unlike activation, it changes
  every call.
- **task status / task next**: dynamic. `task status` gives claim-filter
  scope and board counts (active/ready/review/blocked/waiting/stale/oops).
  `task next` returns the full task record (handle, description, project,
  phase/flow, priority/urgency, claim window, `claim_context_link`, origin
  thread, phase effort, `rehydrate:` commands, `context_check:` instruction,
  and a trailing `next:`/`drive:` directive). This is the single densest
  load-bearing block in the whole session — it is the actual work contract.

### 5. Shell "Working state" banner vs. 6. PostToolUse hook — two distinct channels

These are easy to conflate (the task description that seeded this map assumed
a single "PostToolUse hook additionalContext... injected onto every Bash
result" channel). Direct observation this session shows **two separate
mechanisms**, confirmed against
`docs/design/accepted/transparent-steering-injection.md` and
`docs/design/experimental/in-band-assistant-output-protocol.md`
("treat vendor hooks as the ambient inbound channel for non-shell tool
stretches"):

- **Shell stderr banner** (`🌶️ Working state: claim ... ; last maxim ...`):
  injected by the `spice agent run` shell-reexec wrapper described in
  `transparent-steering-injection.md`. Only fires for shell commands, appears
  as a line prepended inside that Bash tool result.
- **PostToolUse hook `additionalContext`** ("Inbox Steering" block, with
  `control=`/`note=`/RENEW lines, ACK/NACK format instructions, and
  persistence rules): fires on `matcher: "*"` in
  `<worktree-git-dir>/.spice/agents/claude-post-tool-hook.json`, i.e. after
  *every* native tool call. Confirmed directly this session: it appeared
  attached after a **Read** tool call (reading `SKILL.md`), not only after Bash
  calls — so the "every Bash result" framing in the seeding task description
  is measurably wrong, or at minimum incomplete. This is the channel that
  carries live operator steering (pending keys) and is the one the ACK/NACK
  protocol in the skill file targets.

### 7-10. Harness system-reminders

These are Claude Code harness mechanics, not spice-authored:

- Deferred-tool list: appears once, then reappears specifically when a new
  deferred tool set becomes available — observed live this session when the
  playwright MCP server finished connecting mid-turn and its
  `mcp__playwright__browser_*` tools were added to the deferred list.
  High signal exactly once (when it changes), pure repetition otherwise.
- Available agent types / available skills: static enumerations, useful for
  deciding whether to delegate, otherwise inert.
- `claudeMd` block: carries the operator's standing global protocols
  (SURVEY/DREDGE/TRACE/TARGET/EXECUTE/DENOISE/REFLECT/PROBE), the
  `MEMORY.md` index, and `userEmail`/`currentDate`. This is real standing
  guidance (e.g. it is what obligates cycle-local protocol matching), but
  its re-delivery cadence across compactions was not independently tested
  this session — flagged as inspected/asserted, not verified by observing a
  second delivery.
- `gitStatus`: a point-in-time snapshot explicitly labeled as not updating;
  treat `git status`/`git log` as authoritative over it for anything current.

### 11. RTK rewrite

The activation contract (`rtk_contract`) asserts that discrete shell commands
(git, grep, ls, cat, find, diff, log, tree) get compacted by rtk before
reaching the agent, and warns against pre-terse forms (`--oneline`, `| head`)
because those defeat rtk's own compaction. This session did not run a
controlled comparison (same command through rtk vs. a raw bypass) to observe
the rewrite delta directly — this line item is asserted by the contract text,
not independently confirmed by observation. Flagged as a gap: worth a
follow-up task if the actual compaction ratio/behavior ever needs tightening.

## Signal vs. noise, net assessment

**High signal, changes turn to turn:** briefing's `Latest Ask`/Inbox block,
task status/next records, the PostToolUse Inbox Steering block, git dirty
state, newly-arrived deferred-tool announcements.

**High signal, static within a session (read once, reference forever):**
activation's contract block, the skill file's own Prompt Boundary and Working
Rules, available-agent/available-skill enumerations.

**Repeated verbatim, candidate noise:** the ACK/NACK/task-capture format
instructions restated inside every PostToolUse Inbox Steering block once
already internalized from the skill file; the deferred-tool list reprinted
unchanged turn after turn until something new arrives.

**Not yet independently verified (flagged, not asserted as fact):** RTK's
actual rewrite behavior; `claudeMd` block's re-delivery cadence across
compactions.
