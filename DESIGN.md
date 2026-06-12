# spice — design

spice is the Simultaneous Production, Integration, and Control Environment:
an installed agent harness. This document states what the system
fundamentally *is*, the opinions it enforces, and the invariants any
implementation path must preserve.

## What this system is

One idea governs everything:

> **The agent's transcript is the single source of truth, the repo's
> filesystem is the single channel of steering, and everything else —
> supervision, coordination, conscience, hygiene — is derived mechanically
> from those two surfaces.**

Five subsystems realize it. Each is independently useful; together they form
a closed loop.

### 1. The steering fabric (inbox + wrapper + side channel)

Operator → agent communication is a durable filesystem inbox
(`.spice/inbox/*.txt`, UTC-microsecond keys, atomic hardlink publish,
priority + continue/stop note, 24h expiry). Nothing is "delivered" by being
read: an item is retired only when the agent writes `ACK <key>: <response>`
in an assistant message — a semantic acknowledgment extracted from the
transcript by a tuned parser (standalone `ACK` token, `\d{8}T…` key grammar,
filler words, dropped-`Z` aliases, app-directive scrubbing). Unacknowledged
inbox steering re-displays every 15s and, under the ACK watcher, is
re-published with
escalated priority (`urgent`, then `critical`) after every 3 silent
assistant messages.

Agent ← harness delivery rides the agent's own command executions: `spice.sh`
wraps every shell command, routing through a token-optimizing proxy (`rtk`)
and injecting into stderr (a) pending inbox steering, (b) context-pressure warnings
derived from the agent's own transcript token counts, and (c) the
supervisor's side-channel payload over a Unix socket. The terminal is a
duplex steering surface; the agent cannot run a command without hearing the
operator.

The reverse voice channel is `SAY: …` lines in assistant messages — spoken
aloud through TTS, surfaced as badges in the UI, AND echoed as prose.

### 2. The lifecycle plane (worktree-bound agents)

One agent inhabits one git worktree. `agent ensure` starts/resumes it under a
durable supervisor process with state.json, log capture, startup session-id
parsing, and an ensure-lock. The **prompt boundary** is sacred: the initial
prompt is only a neutral skill invocation (`[$spice](path)`); the
operator's actual ask is always recovered live from activation + session
briefing + task board + inbox steering. Renewal never kills: a running agent is
asked (by ordinary inbox steering) to reach a clean handoff; the successor starts on the next
message with rehydration instructions pointing at the ancestor thread. The
supervisor refuses to start with an ambient thread id set, and agents get a
git-shadow environment (`branch.X.remote=.`) so upstream noise never reaches
them — sync is not theirs to do.

### 3. The conscience (maxims)

The supervisor tees agent stdout, extracts assistant prose (stopping at
generated tool-output boundaries), trigger-scans it against word-bag maxims
("no fallbacks", "no shims", "no modes", "no aliases", "no legacy", "no
polling", "no backwards compatibility"), and routes hits to a local LLM
judge — two parallel judges, shuffled four-line IFF YES/NO prompt, retry on
ambiguity, any-violation fails. Violations come back as `[MAXIM]` inbox steering,
gated once per compaction epoch, self-echo suppressed. The repo's opinions
police the agent in real time.

### 4. The coordination plane (tasks + teams + serve)

Work distribution is Taskwarrior in the git common dir (all worktrees share
one board): phase flows (`todo → review`) in UDA slots, atomic claims with
TTL and context links, single active claim per actor, review
separation-of-duties (the author cannot claim their own review; the
allocator may assign it), an `oops` board capturing tool friction, urgency
allocation via `task next`, and git integration bound exclusively to task
boundaries — claims fast-forward to the baseline, completions publish
baseline-first merges; the only git event an agent ever sees is a real
content conflict.

The operator surface is `serve`: a zero-dependency stdlib HTTP server with a
hand-rolled WebSocket **live bus** (request/response + push;
`lane.subscribe/refresh/history/send/taskDrain`, `targets.*`, `teams.*`,
heartbeat/liveness/backoff-reconnect). The UI's model is the **lane**: an
operator-owned container over a concrete
worktree target. Agents are *occupants* — renewal hands the lane to a new
thread while the message stream survives, attributed per occupant. Lanes fuse
into groups (drag-to-gutter) backed by server-side **teams** (SQLite,
revisioned, optimistic concurrency; create/close/split/merge/move/config
commands). Every message send carries a **lifetime** intent on a slider:
**Renew** (graceful succession), **Steer** (default), **Drive** (drain the
task queue through structured control metadata, honor task filters).
Task filters route board stems to lanes; pills show per-stem open counts and
drainability. Messages stream live from the transcript (kqueue on macOS,
watchfiles elsewhere) as envelopes: ACK segments laid out quote-then-response
in the agent's order, presence records (tool calls/reasoning) that carry
activity without consuming the visible budget, plan updates, compaction
dividers, image extraction, FINAL/MAXIM/ACK/SAY badges, TTS playback with a
narration mode and a global sequential speech queue.

### 5. The constitution (hygiene as executable opinion)

Quality gates are the hook backend, not a ritual: `.githooks` shims call
`dev pre-commit` (repo shape → staging → policy → formatters → assets →
authored-tree → study guards) and `dev commit-msg`. The opinions, exactly:

- **Namespace packages only** — no `__init__.py` under the package, enforced.
- **Path shape** — package dirs/files match `^_*[0-9a-z]+_*$`; generic
  split names (`.PartNN`, `…[a-z].py` shards) are rejected: splitting a file
  requires naming the seam.
- **File shape pressure** — 1000 LOC base / 1500 flex (×1.5) / 80,000 bytes;
  a file that ever breached flex stays held to base until it shrinks (sticky
  state in the git dir, rename-following).
- **Routine complexity** — CCN ≤ 20, length ≤ 80, same flex+sticky regime.
- **Magic numbers** — staged scan diffed against a HEAD baseline; only
  regressions fail.
- **Commit messages** — subject ≤ 100 chars; body auto-folded at 100;
  literal `\n` rejected; URLs and trailers exempt.
- **Env policy** — literal env-var names in source require an
  `env-policy: allow` waiver comment.
- **Fully-staged rule** — partially staged files fail the gate.
- **No negative tests** — assert intended behavior, never absence or
  migration trails.
- A successful gate clears sticky state it no longer needs; `dirty` renders
  the same pressure against the uncommitted tree as steering, not as a block.

## Design principles

0. **Standalone product, not a repo organ.** spice is installed once
   (`uv tool install spice`) and operates on any repo from outside. A target
   repo contains only what spice
   writes into it: runtime state under `.spice/`, hook shims that call
   `spice dev pre-commit`, and an optional `spice.sh` shim. The worktree
   skill ships as package data (per-repo override honored); the supervisor
   respawns via `python -m spice`; every self-referencing command string is
   `spice …`. When the target repo is the spice source checkout, the checkout
   wins over any installed editable copy by being first on `PYTHONPATH`;
   ordinary target repos continue to use the installed product. The spice repo
   itself is just another target of its own constitution.
1. **The driver seam.** Agent-CLI specifics (binary/argv, thread-id
   environment, rollout location and grammar, stdout section markers,
   session-id parsing) live in one `AgentDriver` value in
   `spice/agent/driver.py`. One built-in driver, no modes; a second driver is
   a new module.
2. **Agent launch defaults have two scopes.** Tracked `[tool.spice.agent]`
   project defaults set model and thinking for every clone; current-worktree
   overrides live in `.spice/config/state.json` through
   `spice config agent --scope worktree`. Explicit launch flags still win.
3. **The opinions are configuration with teeth.** Limits (LOC, CCN,
   length, wrap, flex factor) live in one `spice/policy.py` constants module
   that both the gates and the docs read. The *defaults are the opinion*;
   overriding is editing your repo's policy file, not passing flags.
4. **Any-language studies.** spice gates repositories in any language, not
   just Python. File shape pressure is suffix-free; complexity covers every
   language lizard parses; the magic-number regex holds across the C-grammar
   family (Python rides its own ast scan); language families are declared
   once in `spice/policy.py`. The Python-package guards bite only under
   declared `package_roots`.
5. **Stdlib-first forensics.** Session forensics run over plain JSONL
   iteration — no database dependency for modest filtering. Runtime
   dependencies are `watchfiles` plus the gate backends `ruff` and `lizard`,
   which install with the product so hooks never depend on the invoking
   shell's PATH.
6. **The task vocabulary opens up.** spice ships the approved stems
   `task/serve/agent` + repo-configurable stems via tracked `pyproject.toml`.
7. **No top-level mail verb.** The inbox/ACK machinery is an internal
   steering fabric used by the wrapper, supervisor, and serve UI; it is not
   advertised as `spice mail`.
8. **One coherent UI.** The lane model, live bus protocol, occupants,
   fusing, lifetime slider, filters, and speech share one visual language
   (palette tokens, ACK/FINAL/SAY tint semantics), one coherent
   implementation sized to the essentials.
9. **Mounted commands.** spice unifies a repo's custom tooling without
   owning it: `[tool.spice.commands]` in tracked `pyproject.toml` mounts repo
   commands under `spice <name> …` with verbatim argument passthrough.
   Built-in verbs always win; shadowing fails loudly. Mount names stay
   one-level; large families mount a single namespace owner, and the repo
   tool owns nested grouping through passthrough args (`spice toolbox lint
   css --fix`), rather than inventing dotted, spaced, or per-tool spice
   mount names.

## Module map

| subsystem | modules |
| --- | --- |
| steering fabric | `spice/mail/` internals, `spice/agent/wrap.py`, `spice/agent/sidechannel.py` |
| lifecycle | `spice/agent/{lifecycle,renewal,activation,gitshadow,watchdog,driver}.py` |
| conscience | `spice/agent/maxims.py`, `spice/agent/maximcli.py` |
| tasks | `spice/tasks/` |
| serve | `spice/serve/` + lane-cockpit static UI (app.{render,stream,lanes,shell,groups,audio}.js) |
| forensics | `spice/sessions/` (briefing, sweep, summary, tokens, turns, compactions, user-log, commits) |
| constitution | `spice/studies/`, `spice/hooks/`, `spice/policy.py` |
| infra | `spice/{paths,config,configcli,locking,flexstate,procs,worktrees}.py` |
| bootstrap | `.githooks` shims, `.agents/skills/spice`, `spice.sh`, AGENTS.md |

## Behavioral invariants

- Inbox steering: atomic publish, collision suffixes, direct-child names only, 24h
  expiry, `Priority:`/`Note:` composition, continue vs graceful-stop notes,
  resend escalation urgent→critical, 15s redisplay, bare reads never clear.
- ACK grammar: standalone token, key shape, fillers,
  separators, `Z`-dropped aliases, `::directive{}` scrubbing, segment
  splitting with preamble.
- Wrapper: proxy routing (`proxy` verb passthrough), git-shadow env for
  direct `git`, scrubbed env for nested harness calls, side-channel hello
  protocol, context-meter cache (15s) and warning repeat (15m, persisted),
  pressure levels green/<75/yellow/85/orange/90/red with keep-working
  instructions that forbid finish-before-rollover behavior.
- Lifecycle: ensure-lock, startup grace/timeouts, supervisor state
  publication contract, ambient-thread-id refusal, prompt-boundary rule,
  renewal handoff text and rehydration template.
- Watchdog: stdout section scanner keyed on driver markers, compaction-gated
  reminder dedupe, judge-statement boundary at diff/patch markers,
  suppression of `[MAXIM]`/`WATCHDOG:` echoes.
- Tasks: handle grammar `KEY-YYYYMMDDThhmmssffffffZ` (key derived, never
  stored; identity is `incepted`), claim TTL 3600s, claim context ±300s,
  phase slots 0..6, review urgency coefficient 4.0, oops wait 2099-01-01,
  priority SLA due dates (H:1d, M:7d, L:30d), single-active-claim,
  same-author-review guard, sentinel actor, git sync only at boundaries.
- Serve: message keys `timestamp#offset`, tail scan 1MB chunks / 8MB cap,
  presence records excluded from visible budget but one newest kept,
  paired view-image collapse, activity active/active-ish/inactive at 60s/5m,
  Drive drain suffix on explicit steering, ordinary empty-message rejection,
  team revisions monotonic, lifetime vocabulary `Renew|Steer|Drive`.
- Narration speaks edges, not essays: explicit SAY/ACK utterances win;
  the fallback reads only the first and last paragraphs of the body
  (final-answer bodies narrate even in speak mode); image markdown is
  described, never read; image-only messages stay silent. Every prose
  message carries a manual play button.
- Constitution: every limit listed above, sticky flex semantics, auto-fold,
  hook shim shapes, install via `core.hooksPath`.
- The gate maximizes what an agent communicates per crank: anything it can
  fix itself (formatting, safe lint fixes) it fixes and restages instead of
  bouncing the commit; agent attention is spent only on real findings.
- Repo-truth docs (`AGENTS.md` by default; widened via tracked
  `[tool.spice.policy] repo_truth_docs`) are capped at 5000 characters —
  the constitution governs more than source files.

## Dependencies

Runtime: `watchfiles`, `ruff`, `lizard`. Optional binaries, degrade loudly: `task`
(Taskwarrior), `rtk` (proxy; absent = passthrough), `afm-cli` (judge;
configurable), `say` (TTS; non-Darwin no-op), the agent CLI itself.
Dev: `pytest`, `ruff`, `lizard` (the complexity gate requires it and fails
loudly when missing — no degraded counting path).
