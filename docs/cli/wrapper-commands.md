# Wrapper And Mounted Commands

Status: implemented contract.

Spice has two command-extension surfaces with different owners:

- `spice agent run -- <cmd>` is the agent shell wrapper. It is how agent-run
  shell commands receive steering, keep-working guidance, RTK rewrite routing,
  source checkout routing, and configured wrapper functions
  before the requested command executes.
- `[commands]` mounted commands are repository-owned command paths.
  They let a project expose its own tools under `spice <verb>` or
  `spice <verb> <subcommand> ...` without making those tools built-ins for
  every repository.

## Agent Command Wrapper

Agent launch points `ZDOTDIR`/`BASH_ENV` at the packaged redirector hook dir
and precomputes configured wrapper functions into `SPICE_SHELL_HOOK_WRAPPERS`.
The first non-interactive command shell with an execution string runs the
redirector hook, which clears `ZDOTDIR`/`BASH_ENV` and reexecs through:

```sh
spice agent run -- <shell> -c "<original command>"
```

Agents normally run shell commands directly; the startup hooks perform this
reexec. `agent run` repoints `ZDOTDIR`/`BASH_ENV` at the packaged static hook
dir for the shell command it runs, so that shell and its descendants run the
static stage only: source the user's real startup files, rearm the packaged
hook environment, and eval `SPICE_SHELL_HOOK_WRAPPERS` without a second
`agent run` hop or second steering injection. The redirector and static stages
are distinct packaged hook directories, not an environment marker, so there is
no reexec counter to read. Use `spice agent run -- <command>` explicitly only
when recovering a command path or inspecting wrapper behavior.

The native harness or shell startup hook must hand the complete top-level shell
command string to `spice agent run` exactly once. `agent run` owns RTK rewrite
because it is the only layer that sees the full shell string before execution.

The wrapper does this before running the requested command:

- prints pending operator steering and keep-working guidance on stderr;
- preserves ACK semantics by leaving inbox retirement to transcript ACK lines;
- asks optional [RTK](https://github.com/rtk-ai/rtk) `0.42.4` or newer for each
  eligible shell command string or direct argv, falling back to the native
  command whenever that attempt is unusable;
- routes git through the worktree shadow environment;
- routes `spice` and `python` commands to the correct worktree source checkout
  or target repository virtual environment;
- makes configured shell wrapper functions available.

### RTK Rewrite Protocol

Spice invokes `rtk rewrite -- <command...>`. Exit `0` or Exit `3` with
non-empty stdout replaces the command with stdout. Exit `1` with empty stdout
means the command is unmatched and runs unchanged. Launch failures, malformed
direct argv, and every other exit/stdout combination emit one bounded,
repeat-suppressed diagnostic and execute the original native command unchanged.
Rewrite selection optimizes command output; it does not grant command
permission.

The layered `rtk.executable` setting accepts one trusted executable basename or
absolute path. Spice performs no earlier availability lookup: health probes and
rewrites invoke the exact winning identity. RTK remains the sole owner of
rewrite selection and emits a canonical `rtk` frontend; Spice remaps only that
frontend to the configured identity before applying any finite post-selection
route below.

Spice sets `RTK_DB_PATH` to the current agent thread's
`<worktree-git-dir>/.spice/agents/<thread>/rtk/history.db` and owns that
location, while RTK owns its history contents. Activation `rtk_status`, Doctor,
and bounded, repeat-suppressed stderr diagnostics are health telemetry, not
rewrite selectors. The built-in `common` wrapper owns only the finite
command-shape transformations below.
The complete install, verification, and ownership contract is in
[CONFIG.md](../../CONFIG.md#rtk-rewrite-companion).

## Wrapper Groups

Wrapper functions are generated from the effective `wrappers.<group>` tables.
The selected groups come from the effective `agent.wrappers` list. In
every layer those names are `[wrappers.<group>]` and `[agent]`; tracked
repository settings live in root `spice.toml`. When no list is configured,
spice selects the built-in `common` group. An explicit empty list disables
wrapper generation.

The ordinary recursive layer merge applies until the named group boundary. A
later `[wrappers.common]` replaces the complete inherited `common` group rather
than merging individual routes. Other scalar and list leaves replace earlier
values, so `wrappers = []` is the deterministic way to clear an inherited
selection.

The built-in `common` group contains the global `rtk` and plain `grep`
wrappers. It does not choose commands for RTK; it preserves native semantics
after RTK selection by routing:

- rg-only grep flags (`--files`, `--type`, `--type=*`, `--no-heading`, `-g`,
  `--glob`, `--glob=*`) to `rg`, since those flags are an explicit request for
  ripgrep's own frontend. Trailing-directory operands (any operand ending in
  `/`) instead stay on the canonical `rtk grep` frontend: a grep-dialect command
  must never be forwarded to `rg`, which reinterprets grep short flags (`-r` as
  `--replace`, `-E` as `--encoding`, `-L` as `--follow`, `-U` as `--multiline`)
  and would silently rewrite or misdirect the search;
- native find predicates and actions to `find`;
- diagnostic git flags such as `--check` and `--name-status` to `git`;
- every `rtk grep` carrying a file or directory search operand through the
  driver-scoped recursion route — `rtk grep -E -r` for Codex, `rtk grep -r` for
  Claude — so a bare directory operand recurses through native grep instead of
  failing, and every remaining Codex-authored `rtk grep` through a final
  head-only `rtk grep -E` route.

Because these routes are shell functions named after the wrapped command, they
intercept only ordinary command words. A `command`-prefixed invocation such as
`command rg --files docs/design` bypasses the generated `rtk()` function through
the POSIX `command` builtin: RTK still rewrites it to
`command rtk grep --files docs/design`, but no `wrappers.common.rtk` match flag
can fire, so it reaches the real RTK grep frontend unrouted and the rg-only
`--files` forwards to the platform grep to fail natively. The `command`-prefixed
form is a known limitation of the shell-function mechanism, not a routed case;
Spice's post-selection routing governs only wrapper-visible command words, never
RTK's external rewrite selection.

Every selected direct wrapper whose argv head is not the configured RTK
executable owns its command word before RTK rewrite, regardless of whether the
wrapper came from packaged defaults, repository configuration, or an extension.
The global plain `grep` wrapper therefore makes an RTK candidate ending at
`rtk grep` yield: an agent-authored `rg` remains `rg`, preserving ripgrep's
regular-expression dialect and flags. RTK-headed and unselected wrapper words
remain eligible for rewrite.

Both drivers receive that plain `grep` wrapper. Its shared base invokes native
grep, preserving Claude-authored BASIC alternation such as `\|`; a Codex-scoped
catch-all injects `-E` so Codex-authored `| + ? ( )` remain operators. Explicit
`-E`, `-F`, `-P`, or `-G` matcher flags route first and pass through unchanged,
so no injected matcher competes with the caller's choice. The separate
driver-scoped `rtk grep` routes still provide recursive `-r` behavior for search
operands. Unsupported or conflicting backend flags pass through unchanged to
fail natively rather than selecting another path. Any new transformation
belongs in the published contract and its executable tests; it is not another
rewrite selector.

Selecting `common` inherits this global default in full. A later-scope
`wrappers.common` table replaces the whole group atomically at the named-group
boundary—its routes do not concatenate with the default's—so a partial override
must re-list every route it still wants. Omitting the table inherits the
default; `wrappers = []` disables wrapper generation; and a `false` group or
entry disables that inherited name explicitly. A malformed replacement fails
with the winning scope and source path.

Repos that need exact shell-function control can override or extend groups
(replacing the whole `common` group, so the native reroutes and the `grep -E`
default only ride along if re-listed):

```toml
[wrappers.common]
wrap = ["grep", "find", "git"]
```

Selectors are command names, not paths. Path selectors such as `/bin/sh` fail
loudly until a redirector stage exists. A wrapper cannot intercept itself, and
duplicate selectors fail during wrapper generation.

Wrapper entries may also be direct argv wrappers with an `argv = [...]` list;
spice shell-quotes each argv word while building
`SPICE_SHELL_HOOK_WRAPPERS`. A wrapper group, direct wrapper, or individual
`match` route may set `scopes = { drivers = ["codex", "claude"] }`; the
universal scope parser validates and normalizes those names, and only entries
matching the active worktree driver render. Prefer stable repository-owned
commands over hook-private environment variables. For example, a repository
can opt into a local code-generation wrapper by selecting its own extension
group alongside `common`, without implying that `codegen` belongs to the
generic default:

```toml
[agent]
wrappers = ["common", "repo-tools"]

[wrappers.repo-tools]
codegen = { argv = ["uv", "run", "python", "-m", "tools.codegen"] }
```

The spice checkout itself uses the same local-extension pattern to catch the
common agent habit of running bare `pre-commit`, while leaving the generic
`common` group unchanged:

```toml
[agent]
wrappers = ["common", "spice-dev"]

[wrappers.spice-dev]
pre-commit = { argv = ["spice", "dev", "pre-commit"] }
```

## Mounted Commands

Mounted commands come from the effective `commands` table. Repositories usually
declare them in tracked root `spice.toml`:

```toml
[commands]
release = ["uv", "run", "python", "-m", "spice.release"]
```

Every layer uses the same plain `[commands]` table. Command entries merge by
dotted command name across scopes; a later leaf replaces the earlier argv
exactly. `spice config show` reports the winning scope and path for each
effective command leaf.

`spice release notes` runs the mounted command from the repository root with
`notes` passed through verbatim. String mounts are shell-split once; list mounts
pass their argv exactly.

Mounted names are dot-separated segment paths whose segments match
`^[a-z][a-z0-9-]*$`. Mounts that shadow built-in or extension-provided spice
actions are refused at any depth: `spice doctor` reports each refusal while
built-in commands and valid sibling mounts remain available. Dotted mounts
under built-in verbs are allowed only when the full path is a novel action name:

```toml
[commands]
toolbox = ["uv", "run", "toolbox"]
report.inspect = ["project-tool", "report", "inspect"]
"study.repo-tool" = ["project-tool", "study", "repo-tool"]
```

`spice toolbox lint css --fix` then passes `lint css --fix` to `toolbox`.
`spice report inspect --limit 40` then passes `--limit 40` to the mounted nested
path backend. `spice study repo-tool --limit 40` does the same for a novel
action under the built-in `study` verb, while `study.csharp-members` would fail
because that full path is already registered.

Mounted commands can import the public repo-tool seam documented in the README.
They should not rely on private spice modules unless the seam is deliberately
expanded with tests and documentation.

### Versioned command plans

A mounted command opts into Spice's command-plan protocol solely through its
stdout. A successful command whose entire stdout is a valid
`spice.command-plan` JSON document is a side-effect-free planner; no
configuration field, alternate argv, or second executable is declared. Any
other output, stderr, and exit status pass through exactly as ordinary mounted
command output does.

The document carries `schema_version`, `command`, `plan_digest`, and one ordered
`operations` list. Every operation names its `kind`, `target`, `scope`, and
optional digest-bound `executor` (default `spice`); an applicable `file` or
`git-config` operation also carries total
`observed_before` and `intended_after` states. The digest is SHA-256 over the
schema version and complete normalized ordered list, so a one-operation plan
and a fifty-operation plan use the same protocol:

```json
{
  "protocol": "spice.command-plan",
  "schema_version": 1,
  "command": "generate",
  "plan_digest": "<sha256>",
  "operations": [
    {
      "order": 1,
      "kind": "file",
      "executor": "spice",
      "target": "generated.txt",
      "scope": "worktree-file",
      "observed_before": {"value": null, "mode": null},
      "intended_after": {"value": "generated\n", "mode": 420}
    }
  ]
}
```

Bare invocation prints the preview. For the default `spice` executor,
`spice generate --apply=<sha256>` invokes the mounted planner once, recomputes
and verifies the document digest, and then Spice applies the closed file and
Git-config operation vocabulary itself. An unknown operation kind refuses
rather than being delegated back to the mounted executable. An operation
explicitly marked `managed: false` is preflighted and preserved.

Commands that own effects outside that closed vocabulary put
`executor: "command"` on every operation. Their digest-authorized apply first
plans without effects, then Spice invokes the same configured argv for one
authorized execution with the verified digest in a private environment
assertion. The command replans and checks that assertion before acting. Mixed or
unknown executors, stale digests, and bare command-owned `--apply` refuse before
execution. Command-owned effects are not generically receipted or reversible.
The repository's mounted `spice release` appliance uses this boundary for its
validation, build, Git, registry, and GitHub operation vocabulary.

The internal execution-digest variable is also a capability sentinel. Its
absence never selects a generic fallback: the candidate reads the installed
parent distribution version, and only parent `0.30.1` publishing candidate
`0.30.2` may use the former single self-owned apply path. `0.30.2` is the named
retirement boundary for that one forward bridge; every other missing or unknown
capability refuses before effects. New parents always send the sentinel and
therefore have exactly one current path: the digest-authorized
planning/execution split.

A changed authored input produces a different plan and refuses before any
operation while naming the current ordered operations. Bare `--apply` remains
available to nondestructive Spice-owned plans without asserting a previous
digest.

Application appends a bounded write-ahead intent before each effect and a
completion fact afterward to a mount-scoped JSONL receipt under the worktree
Git directory. Both facts reuse the plan's normalized operation record and
digest; the mounted child does not write or know the receipt path. If completion
append is interrupted after the effect, resume observes the intended-after state
and completes the same intent without repeating the effect.

`spice generate --unapply` reads that authoritative receipt and previews its
ownership-aware reverse plan without invoking the mounted child.
`--unapply=<receipt-digest>` asserts the selected receipt, while
`--apply=<plan-digest>` independently asserts and applies the recomputed reverse
plan. Clean reversal removes the inactive receipt. An interrupted apply or
unapply resumes from the durable operation prefix, divergent state is retained
with a recovery record, and explicitly unmanaged state remains untouched.
Callers never pass a receipt path through this mounted-command seam.

### Execution context: mount vs gate step

The environment a command receives reflects *what it actually is*, consistently
across both surfaces:

- A **mounted command** carries the mount environment — `SPICE_MOUNTED_COMMAND=1`
  and `SPICE_VISIBLE_PROG` — whether it is run as `spice <name>` or as a
  `pre_commit`/`pre_commit_success` step that names it via `mount` (or a bare
  mounted-command name). A mount run by the gate is still that mount under spice,
  so it presents identically on both paths.
- A **raw `run`/`argv` gate step** is not a mounted command, so it does **not**
  get the mount signals; it runs with its argv as written.

Every `pre_commit` command step — mount or raw — additionally gets
`SPICE_STAGED_PATHS` (newline-delimited staged paths, narrowed by
`scopes.paths`; `scopes.drivers` and `scopes.models` select the effective
configured worktree agent; `scopes.phases` selects the pre-commit or success
hook phase). Each omitted axis is unconstrained.
The guarantee is representational: the env says what the command is (a mount,
or not) rather than where it was triggered from.

## Choosing A Surface

Use `spice agent run -- <cmd>` for agent-owned execution where steering,
keep-working guidance, RTK rewrite routing, worktree routing, and wrapper
functions must apply.

Use a mounted command for repository-owned tools that operators or hooks should
run as `spice <verb>` in that repository only. Release tooling is mounted in
this repository for that reason: other repositories can mount their own release
implementation without competing with a global spice built-in.
