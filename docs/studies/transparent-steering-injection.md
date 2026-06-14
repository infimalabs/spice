# Spike — transparent steering injection without an explicit wrapper

Status: design exploration (spike). No implementation here.
Tracking: `STUDIES-20260614T024336971144Z` (battery), `STUDIES-20260614T025014632454Z`
(PATH-takeover deep-dive + this committed doc).
Related: `OOPS-20260614T023147659176Z`, `MAIL-20260614T023710464275Z`.

## Problem

Today the steering fabric delivers to the agent *only when the agent prefixes
`./spice.sh`* (→ `spice agent run` → `inject_agent_side_channel()` relays the
supervisor side-channel payload to the wrapped command's stderr). DESIGN.md
asserts "the agent cannot run a command without hearing the operator" — but that
is currently true by **agent discipline**, not by enforcement. An agent that
runs a bare command hears nothing. (That is exactly how the first run under the
Claude driver silently missed every operator message.)

## Goal (operator)

Get a reference to stderr — the steering readout — into the agent **as early as
possible, automatically, with no agent choice**. Killing the explicit
`./spice.sh` requirement is the point. Special launch/agent config is acceptable.

## Empirical findings (Claude driver, this session)

Bash-tool shell, measured by probing it directly:

| Probe | Result |
| --- | --- |
| Tool shell | `/bin/zsh` 5.9, **`argv0=/bin/zsh` (absolute path)** |
| Invocation flags `$-` | contains `l` (login), no `i` → **login, non-interactive** |
| Parent process | direct child of `claude` (PPID → `claude`) |
| Persistence | none — fresh shell PID per command (env/functions do not carry over) |
| `BASH_ENV`/`ENV`/`ZDOTDIR` | all unset at launch |

Mechanism proofs (an injector that writes to fd 2):

| Mechanism | Fires? | Notes |
| --- | --- | --- |
| `ZDOTDIR/.zshenv` under `zsh -lc` | yes | stderr precedes command stdout |
| `ZDOTDIR/.zshenv` under plain `zsh -c` | **yes** | `.zshenv` is always sourced, login or not |
| `ZDOTDIR/.zprofile` under `zsh -lc` | yes | login-only; redundant with `.zshenv` |
| `BASH_ENV=file bash -c` | yes | non-interactive bash sources it |
| PATH-prepended `git` shim | yes | per-command shim intercepts a shadowed binary |
| PATH-prepended `zsh` shim (shell) | **no** | tool execs `/bin/zsh` absolutely; PATH bypassed |
| `.zshrc` | **no** (by spec) | non-interactive zsh skips `.zshrc` |
| `precmd`/`PROMPT_COMMAND` | **no** (by spec) | interactive-only; tool shell is non-interactive |

**Decisive consequence:** each tool command is a *fresh* shell and `.zshenv` is
sourced unconditionally, so an injector in a spice-controlled `.zshenv` runs on
**every** command, per-command, with no way for the agent to opt out.

## The battery

### A. Shell-init env interposition (driver-agnostic, set in the agent launch env)

1. **`ZDOTDIR` → spice `.zshenv`** *(top pick for zsh)*. Supervisor sets
   `ZDOTDIR=<spice dir>` in the `claude`/`codex` process env; that dir's
   `.zshenv` runs the injector then chains to the user's real config. Fires on
   every zsh command (proven). Automaticity: full. Freshness: per-command.
   zsh has **no** `BASH_ENV`-style "extra file" var, so redirecting `ZDOTDIR` is
   the only env-only hook for zsh.
2. **`BASH_ENV` → spice script** *(companion for bash)*. For agents whose tool
   shell is bash (`bash -c` sources `BASH_ENV`). Proven. Pair with #1 so both
   shells are covered regardless of which the driver picks.
3. **`ENV` → script (POSIX sh)**. Analog for `/bin/sh` (dash/ash). Completeness.
4. **Append a guarded block to the user's real `~/.zshenv`** instead of moving
   `ZDOTDIR`. Avoids ZDOTDIR-chaining but mutates user dotfiles globally — weaker
   isolation. Not recommended.

### B. Claude Code native hooks (settings.json; Claude driver only — config is fine per operator)

5. **`UserPromptSubmit` hook** *(top pick for "as early as possible")*. Runs at
   turn start; stdout injects into the model's context *before any tool call*.
   Earliest possible delivery, no shell, no stderr — steering lands directly in
   context. Limit: turn boundaries only (pair with PreToolUse for mid-turn).
6. **`PreToolUse` matcher `Bash` (or `*`)** *(top pick for hard "no choice")*.
   Fires before every tool call; injects `additionalContext`, or with exit code
   2 **blocks the tool** and shows stderr to Claude — gating every command on
   ACK. Per-command. Use additionalContext by default; reserve blocking for
   unACK'd critical/stop steering.
7. **`SessionStart` hook**. Fires on start/resume — natural home for the initial
   briefing + already-pending steering, complementing the bootstrap prompt.
8. **`Stop` hook**. Can refuse to let the agent stop while steering is unACK'd.

### C. Driver-launch delivery vehicle

9. The driver's `build_exec_command`/launch already owns the agent's environment,
   so it is the natural place to set #1–#3 **and** write the Claude
   `settings.json` hooks of #5–#8. One seam, both families.

### D. PATH takeover — deep dive (operator-requested esoteric option)

Two distinct ideas hide under "PATH takeover":

- **D1. Shell-shadow** — prepend a dir with a `zsh`/`bash` shim that injects then
  `exec`s the real shell, so the shim fires on the shell that runs *every*
  command. **Not viable here:** the tool execs the shell as `/bin/zsh`
  (absolute), so PATH is never consulted for the shell entrypoint (proven). It
  would only work if the harness launched the shell by bare name — which we do
  not control and cannot rely on.
- **D2. Per-command shim** — prepend a dir shadowing individual binaries
  (`git`, `ls`, …), each injecting then `exec`ing the real tool. **Viable but
  partial (proven for `git`):** it only covers binaries you pre-shadow. Shell
  builtins (`echo`, `cd`, `:`), absolute-path invocations (`/usr/bin/x`), and any
  un-shadowed command escape. Approaching completeness means shadowing a large,
  brittle, OS-specific set — high maintenance, never airtight.

**Verdict:** PATH takeover is **strictly dominated** by A1 (`.zshenv`/`ZDOTDIR`)
for the "every command, no choice" goal. `.zshenv` fires on shell *entry*
itself — env-based, immune to absolute-path invocation, and covers builtins and
absolute-path commands that a PATH shim cannot. PATH takeover's only niche is
targeting *specific* commands, or a constrained environment where you can set
PATH but not `ZDOTDIR`/`BASH_ENV` — not our situation.

### E. Rejected / weak (recorded so they are not re-litigated)

- **`PROMPT_COMMAND`/`precmd`** — interactive-only; tool shell is non-interactive.
- **`.zshrc`** — not sourced non-interactively.
- **stdin/PTY injection** — violates the "never touch the agent's stdin" invariant.
- **`LD_PRELOAD`/exec interposer** — overkill, platform-fragile.
- **Static prompt/system-prompt text at launch** — not live; cannot carry
  steering that arrives mid-session.

## Recommendation

Two complementary layers, both set once at the driver-launch seam (C):

1. **Env interposition (A1 + A2)** as the *universal substrate* — `ZDOTDIR`
   `.zshenv` + `BASH_ENV`, each invoking the existing side-channel relay
   (`inject_agent_side_channel`, already implemented). Driver-agnostic, proven,
   per-command, unavoidable. This **replaces** the wrapper as the steering
   delivery path — the wrapper's injection role is then deleted (it persists
   only for proxy/git-shadow routing); no dual-path bridge is kept.
2. **Claude `UserPromptSubmit` + `PreToolUse(Bash)` hooks (B5 + B6)** as the
   *Claude-native upgrade* — steering into context at turn start (earlier than
   any command) plus per-tool refresh, with an optional hard ACK gate. Best
   realization of "as early as possible, no choice."

Codex lanes get layer 1; Claude lanes get both. PATH takeover (D) is not
recommended.

### Implementation sketch (for a follow-up build task, not this spike)

- **Extract** the single existing injection entrypoint
  (`inject_agent_side_channel()`: resolve repo_root from cwd, connect to the
  side-channel marker socket, relay payload to stderr, fast no-op when no
  marker/socket exists) into one callable invoked by *both* `.zshenv`/`BASH_ENV`
  and the wrapper. One shape, not a mirrored copy — do not add a parallel
  `steer-dump` alongside the existing logic.
- `.zshenv`/`BASH_ENV` body: guard, then call `steer-dump`.
  - **Recursion/noise guard:** `SHLVL`/breadcrumb so nested shells and scripts
    do not re-inject; lean on the existing 15s on-disk repeat-suppression for
    rate limiting.
  - **Env fidelity:** the spice `.zshenv` must source the user's real
    `${HOME}/.zshenv` (chain) so the agent shell keeps the operator's PATH/env.
  - **Scope guard:** inject only inside a spice worktree (marker present).
- Claude hooks: write `settings.json` hook entries at launch (the `update-config`
  surface); `UserPromptSubmit` emits the readout to stdout; `PreToolUse(Bash)`
  emits `additionalContext`, escalating to exit-2 block for unACK'd
  critical/stop steering.

### Invariants to preserve

- Never touch the agent's stdin.
- ACK semantics unchanged: items retire only on a transcript `ACK <key>`.
- Existing 15s repeat-suppression remains the single rate-limiter.
- When the substrate lands, **delete** the wrapper's steering-injection role
  outright rather than keeping it as a parallel path; the wrapper remains only
  for proxy/git-shadow routing. Replace the old shape, do not bridge it.

## Open questions for the operator

- Confirm Codex's exec tool shell (login zsh? bash? absolute or PATH?) so layer 1
  covers it; the Claude shell is characterized above, Codex is assumed-similar
  but unverified.
- Hard ACK-gating (PreToolUse exit 2) vs. soft context injection as the default
  posture for unACK'd steering.
