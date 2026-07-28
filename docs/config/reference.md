# Configuration Reference

Spice configuration has exactly three scopes, in increasing precedence order:

| Scope | Path | Behavior |
| --- | --- | --- |
| `system` | `<installed spice package>/spice.toml` | Installed defaults; mutations preview by default and require `--apply` |
| `repository` | `<repository>/spice.toml` | Tracked Spice tables such as `[agent]` |
| `worktree` | `<worktree-git-dir>/.spice/config/spice.toml` | Local plain Spice tables for one worktree; structurally untracked |

Managed runtime state has three ownership namespaces. Here,
`<worktree-git-dir>` is the path reported by `git rev-parse --git-dir` for the
current worktree, while `<git-common>` is the path reported by
`git rev-parse --git-common-dir`:

| Namespace | Ownership | Examples |
| --- | --- | --- |
| `<repository>/.spice` | Deliberately visible live and generated integration surfaces | Git hook shims, inbox files, learning records, browser artifacts |
| `<git-common>/.spice` | Managed state shared by every worktree of the repository | Task backend by default, team state, ACKs, maxim metrics, attachments, task artifacts, flex claims, executable-configuration authority |
| `<worktree-git-dir>/.spice` | Managed state and operator-owned input for one worktree/lane | Worktree configuration, initialization receipt, agent runtime, sticky constitution state, disabled-maxim state |

An explicit absolute `SPICE_TASK_BACKEND` redirects task configuration,
TaskChampion storage, and team state. It does not redirect repository-owned ACK
or maxim state, nor does it change either canonical Git-internal namespace.

Tables merge recursively from `system` through `worktree`. A scalar or list at
a later scope replaces an earlier non-table leaf completely; lists never
concatenate, and `key = []` explicitly clears an inherited list. Literal
`false` is the only non-table value that can replace an inherited table,
making table disablement explicit; every other scalar-for-table substitution
refuses and names both layers. A later table can replace an earlier scalar,
including `false`. Named wrapper groups are the one table-level atomic
boundary: defining
`[wrappers.<group>]` in a later scope replaces that whole inherited group.
Every inline `scopes = { ... }` selector is also one atomic leaf: a later
configuration layer replaces the complete selector instead of inheriting
individual axes from earlier layers.

Registry-shaped tables share one removal rule. In `commands`, `maxims`,
`policy.pre_commit_builtins`, `policy.taste.words`, `tasks.reports`, the
`wrappers` group registry, and the entry registry inside each wrapper group, a
literal boolean `false` at a later scope disables that named inherited entry.
Empty strings, lists, and tables retain their domain-specific meanings; none is
a removal spelling. A new packaged named-entry registry must declare this
contract and prove its consumer returns the same disabled answer before the
configuration gate accepts it.

Every Spice table is checked against the structural configuration schema
before layers merge. An unknown structural key refuses with its dotted path
and winning source layer, and suggests the nearest known sibling when the edit
distance is small. Data-keyed maps such as command paths, wrapper names, lock
names, maxim bags, and policy word maps remain open while their fixed nested
fields are checked.

## Packaged defaults

The machine-readable
[packaged-default manifest](packaged-defaults.toml) documents every exact key
and value in the installed system layer. A test compares its complete parsed
tree with `spice/spice.toml`, so runtime and documentation must change
together. Repository and worktree layers apply the single layering and
registry-removal contracts above.

Selector axes, live routing distinctions, mutation commands, and v0.30
migration refusals live in the
[layers and routing companion](layers-and-routing.md). They all use the layer
and removal rules above.

## Executable repository configuration authority

Tracked configuration that names a command is inert until an operator approves
it. The executable capabilities are `commands`, `wrappers`,
`policy.pre_commit`, `policy.pre_commit_success`,
`policy.pre_commit_builtins`, `say.command`, `judge.bin`, `rtk.executable`,
`policy.suite_seam.run`, `policy.reachability_providers`, and
`policy.python_typecheck_interpreter`. Spice digests each capability
independently: approving or changing `wrappers` neither approves nor invalidates
`policy.pre_commit`.

`spice init --apply` is the exact-approval path. It records the current digest
of every repository-defined executable capability in
`<git-common>/.spice/repository-config-trust.jsonl`. The append-only file is
mode `0600`, is shared by linked worktrees, and is absent from clones. Authority
never lives in `<repository>/.spice`, root `spice.toml`, another tracked path, or
any path an agent can force-commit. The old worktree initialization-receipt
digest is migrated once in v0.31.0 through one repository-shared common-Git-dir
migration marker; after that migration no linked worktree can import another
legacy receipt and the old digest is not an approval path. If the lane that
wins this one-time migration has no complete receipt matching its current
configuration, it imports nothing; explicitly reapprove the current
capabilities with `spice init --apply`.

Reads and appends are serialized by a bounded lock in the same common-Git-dir
namespace. A truncated, malformed, non-regular, or group/world-accessible
authority log refuses instead of falling back to another approval source.

Repositories that intentionally accept future signed updates can opt into a
standing grant. Grant and revoke are authored-input mutations: both preview a
versioned plan and change nothing until an operator supplies `--apply` (and may
assert the displayed digest).

```sh
spice config trust show
spice config trust grant \
  --path wrappers \
  --signer SHA256:operator-approved-key
spice config trust grant \
  --path wrappers \
  --signer SHA256:operator-approved-key \
  --apply=<previewed-plan-digest>
```

Omitting `--path` selects all executable capabilities currently present in
repository configuration. `--signer` is repeatable and must match the
fingerprint Git reports for each capability-changing commit.

A standing grant pins the current `origin` URL, tracked upstream ref, exact
anchor commit, capability set, and signer fingerprints. A later digest is
derived only when all of these facts hold:

- HEAD equals the pinned, locally fetched upstream ref and still descends from
  the anchor;
- root `spice.toml` is tracked, clean, and byte-identical to HEAD;
- the remote URL and branch routing still equal the grant;
- every commit since the anchor that changes a delegated capability has a
  verifiable Git signature from an explicitly named fingerprint.

These current remote/ref/HEAD/clean-tree facts are rechecked on every delegated
use. A previously derived digest does not remain executable from a later local
or divergent HEAD; only an exact operator approval is independent of the
standing provenance route.

Successful derivation appends the commit, capability digest, and signer
evidence to the same common-Git-dir log. Serve launch fast-forward and task
publication do not write grants and do not confer authority: a clean Serve
advance merely supplies verifiable Git evidence, while an unsigned agent commit
remains refused even after task publication moves the trusted ref to it.
Unsigned commits, unknown signers, force-pushed/divergent history, local commits,
dirty or untracked configuration, changed remotes, and ambiguous refs all
refuse before the named command starts. The refusal includes the capability
digest, command words, and the failed provenance fact.

Revoke all exact and delegated authority without erasing its audit history:

```sh
spice config trust revoke --reason "rotate signing authority"
spice config trust revoke \
  --reason "rotate signing authority" \
  --apply=<previewed-plan-digest>
```

Revocation immediately invalidates all exact and delegated executable-
configuration authority while preserving its audit history. It is therefore
also the recovery path for an exact approval when no standing grant is active.
To recover, first make the lane clean and equal to the intended trusted ref. An
unsigned or untrusted commit cannot be repaired by a later signed descendant
because its provenance remains in the range: inspect the new anchor, revoke the
old authority, then preview and apply a replacement grant, or explicitly
approve only the current capability digests with `spice init --apply`. A
changed remote, ref, or force-pushed history likewise requires revocation and a
newly inspected grant.

## Runtime Model

Runtime is not a per-repo config surface. The `spice` executable is installed as
a uv tool by default; operators deploying from source use
`uv tool install -e /path/to/spice-main`, making that editable main tree the
server deployment. Worker worktrees are operated trees: config can shape agent
defaults and policy in those trees, but it does not choose a different spice
source checkout, import path, or virtualenv for the running code.

The agent shell can optionally use
[RTK](https://github.com/rtk-ai/rtk) `0.42.4` or newer as a command-output
optimizer. Missing, obsolete, or invalid RTK selects reported native-command
mode without blocking activation, as does an RTK whose rewrite answers a probe
search differently than the command as written. Install and protocol details
live in
[CONFIG.md](../../CONFIG.md#rtk-rewrite-companion).

## `[rtk]`

RTK executable identity is a standard layered setting:

| Key | Default | Meaning |
| --- | --- | --- |
| `executable` | `"rtk"` | One trusted executable basename or absolute path. Relative paths, whitespace-delimited command strings, and argv lists are invalid. |

System, repository, and worktree `spice.toml` files all use `[rtk]`. Later
scopes replace `rtk.executable` normally, and `spice config show` reports its
winning scope and path. Resolution retains
the exact value and performs no `which`, existence, or executable probe.
Activation, Doctor, and `spice agent run` then invoke that exact identity.

Exit `0` or Exit `3` with non-empty stdout applies a rewrite. Exit `1` with
empty stdout is a silent no-match. Other or malformed outcomes are diagnosed,
discarded, and the original native command runs unchanged. RTK owns rewrite
selection and the canonical `rtk` frontend; Spice remaps that frontend to the
configured identity, owns only the built-in `common` wrapper's finite
post-selection routes and the thread-scoped `RTK_DB_PATH` location at
`<worktree-git-dir>/.spice/agents/<thread>/rtk/history.db`, and emits health
telemetry through activation `rtk_status`, Doctor, and bounded stderr
diagnostics. RTK owns the history database contents.

Task-boundary and worktree-discovery git commands have a 120-second default
deadline; network fetch and push default to 30 seconds. Set
`SPICE_GIT_TIMEOUT_SECONDS` to one positive number of seconds to override both
deadlines for unusually slow repositories. Expiry fails loudly with the exact
git argv instead of retaining the task boundary indefinitely.

## `[say]`

The packaged `say` table selects the speech backend and bounds every speech
process:

| Key | Meaning |
| --- | --- |
| `backend` | Active backend name; it must occur in `backend_choices`. |
| `backend_choices` | Valid backend vocabulary. |
| `content_type` | Media type returned by the external-command backend. |
| `words_per_minute` | Base rate scaled by the listener's UI rate. |
| `timeout_seconds` | Positive process deadline for speech rendering. |

Repository and worktree configuration may additionally set `command` for the
external backend and `voice` for macOS `say`; those optional keys have no
packaged value.

### Linux speech with `espeak-ng`

Speech configuration is worktree-local by default. On Debian or Ubuntu, install
the `espeak-ng` package and verify the executable before configuring spice:

```sh
sudo apt-get update
sudo apt-get install espeak-ng
command -v espeak-ng
espeak-ng --version
```

Other Linux distributions should install the package named `espeak-ng` with
their system package manager. Configure its stdout WAV mode, matching audio
content type, and rate slot exactly as follows:

```sh
spice config say --backend external --command "espeak-ng --stdout -s {words_per_minute}" --content-type audio/wav
```

`spice serve` sends prepared speech text to the command on stdin and serves the
WAV bytes returned on stdout as `audio/wav`.

Spice does not know which flag an arbitrary engine spells its rate with, so an
external command names the spot itself: every `{words_per_minute}` token in the
command is replaced with `say.words_per_minute` scaled by the rate the listener
picked in the UI. Substitution replaces that one token and nothing else, so a
command carrying unrelated braces reaches the engine verbatim. A command naming
no slot renders at whatever rate its own engine defaults to, and the UI rate
control then has nothing to act on. Only the macOS `say` backend receives
`say.voice`, which spice writes into the argv it builds itself.

Verify the same executable path independently with:

```sh
printf 'spice speech check' | espeak-ng --stdout > /tmp/spice-speech-check.wav
file /tmp/spice-speech-check.wav
```

## `[judge]`

| Key | Meaning |
| --- | --- |
| `bin` | macOS judge executable; an explicit layered value wins on every platform. |
| `portable_bin` | Judge executable selected off macOS while `bin` still comes from the system layer. |
| `model` | Model named in portable-adapter setup diagnostics. |
| `model_command` | Default argv used by the portable adapter. |
| `timeout_seconds` | Default per-verdict deadline used by the portable adapter. |

Maxim adjudication is off by default. When a trigger bag matches sampled prose,
Spice publishes its `[MAXIM]` reminder directly, launching no judge subprocess
and taking no verdict as assumed — the trade is more false positives for a
deterministic, portable default that needs no local model. Opt into adjudication
per worktree with:

```console
spice config judge --enable
spice config judge --disable
```

`--enable` stores `[judge].enabled = true` in the Git-private worktree
configuration file;
`--disable` restores the judge-free default. Any value other than a true flag
(`true`, `1`, `yes`, `on`) — including an absent one — resolves to judge-free.
When adjudication is enabled, each matched bag is sampled against its maxim: a
`YES` verdict means the sampled text agrees with the maxim and is therefore not
a violation, so its reminder is suppressed; a `NO` verdict is a violation and
publishes. The judge is consulted only on this opt-in path.

Configure the judge binary in the default worktree scope with:

```console
spice config judge --bin /path/to/judge
```

This stores `[judge].bin` in the Git-private worktree configuration file. The
value is one executable path or `PATH` name, not a shell command or argv list.
When unset, the default is keyed to the platform: macOS uses the Apple
Foundation Models `afm-cli` binary; every other platform, where `afm-cli` does
not exist, uses the portable `spice-judge` adapter that ships with Spice. An
explicit `bin` overrides this default on every platform. For each verdict Spice
launches the exact argv `[configured_bin]`. `bin` selects which executable the
enabled adjudication path launches; it does not by itself enable adjudication.
The binary participates in the normal three-layer configuration precedence and
accepts `--scope`; the `enabled` flag is intentionally worktree-local, so
`--enable` and `--disable` require the default `--scope worktree`.

### Portable judge with `spice-judge`

`spice-judge` is Spice's own console script and conforms to the contract in this
section: launched as the exact argv `[spice-judge]`, it reads the prompt on
stdin and writes `YES`/`NO` to stdout. It delegates the judgement to a portable
local model command, obtainable off macOS. The default command runs a small
local model through [Ollama](https://ollama.com); install it and pull the model
once with `ollama pull llama3.2`.

`SPICE_JUDGE_MODEL_CMD` overrides the default with any argv that reads a prompt
on stdin and writes an answer to stdout (for example
`SPICE_JUDGE_MODEL_CMD="ollama run mistral"`). `SPICE_JUDGE_TIMEOUT` sets the
per-verdict deadline in seconds (default `60`; a non-positive value disables
it). There is no silent no-op: when the model command is absent, exits non-zero,
or exceeds its deadline, `spice-judge` exits non-zero with an actionable message
on stderr, which Spice surfaces as its judge error detail. Bring your own judge
by setting `[judge].bin` to any conforming executable instead.

The judge receives one prompt on stdin and writes its verdict to stdout. The
default prompt contains these four lines in a random order on every attempt:

```text
IFF "{maxim}" AGREES WITH "{statement}": ANSWER ONLY "YES".
IFF "{maxim}" DISAGREES WITH "{statement}": ANSWER ONLY "NO".
IFF "{statement}" AGREES WITH "{maxim}": ANSWER ONLY "YES".
IFF "{statement}" DISAGREES WITH "{maxim}": ANSWER ONLY "NO".
```

Before interpolation, Spice collapses whitespace in `maxim` and `statement`
and strips trailing punctuation and whitespace. `--prompt-file` on
`spice maxim agree` or `spice maxim disagree` replaces the default template;
only `{maxim}` and `{statement}` fields are accepted.

The output schema is plain text, not JSON. Spice uppercases stdout, removes
characters other than `Y`, `E`, `S`, `N`, `O`, and spaces, and accepts the
result only when its deduplicated token set is exactly `{"YES"}` or `{"NO"}`.
An ambiguous reply is retried, with two attempts by default.

The process must exit `0`. Launch failure or nonzero exit is immediate; stderr
is included for nonzero exits. Spice imposes no subprocess timeout, so wrappers
for models that can hang must enforce one. Direct maxim checks return `0` when
their requested condition is met, `1` when unmet, and `2` for judge or prompt
errors. During supervision, judge errors are logged and skip that maxim
feedback without stopping transcript capture, steering, or tasks. Learning
distillation likewise skips candidates whose judge call fails.

## `[agent]`

| Key | Meaning |
| --- | --- |
| `personality` | Desired Codex personality. |
| `personality_choices` | Valid personality vocabulary. |
| `wrappers` | Ordered wrapper groups loaded into agent shells; an empty list selects none. |
| `playwright_mcp.server_name` | MCP server registration name shared by Codex and Claude. |
| `playwright_mcp.command` | Executable used to launch the Playwright MCP server. |
| `playwright_mcp.args` | Base argv before Spice appends its generated browser configuration. |
| `claude.default_model` | Claude model used when no layered `agent.model` or explicit launch model exists. |
| `claude.auto_compact_window_tokens` | Claude auto-compaction window exported at launch. |

Repository and worktree configuration may additionally set `model`, `effort`,
and `driver`. These optional launch overrides have no packaged value; the
active driver supplies their fallback.

Agent personality defaults to the worktree scope through `spice config
personality`; pass `--scope system` or `--scope repository` to set it in
another layer. The effective default is `pragmatic`.

Personality is a launch knob, and unlike `model` and `effort` it does not cross
every driver: Codex carries it into each launch as a config override, while
`claude --print` has no launch-time flag for a personality or for fast mode.
Each driver declares which knobs it can carry, the launch path sends only those,
and `spice config personality` names the active driver's answer as it writes the
value. A launch asked for a knob its driver cannot carry reports the knob on the
`spice agent ensure` output rather than dropping it quietly.

## `[wrappers.<group>]`

RTK rewrite selection happens inside `spice agent run`; wrapper entry shapes,
driver scopes, command ownership, the `common` group, and the supervised Claude
tool boundary are documented in the
[layers and routing companion](layers-and-routing.md#wrappersgroup).

## `[commands]`

Mounted commands put repo tooling under the `spice` namespace.

```toml
[commands]
release = ["uv", "run", "python", "-m", "spice.release"]
bench = "python -m myproj.bench"
report.inspect = ["project-tool", "report", "inspect"]
```

Keys are dot-separated command paths with lowercase/digit/hyphen segments.
Mounts cannot shadow built-in or extension-provided `spice` actions at any
depth. Dotted mounts under built-in verbs are allowed when the full command path
is a novel action name. Collisions are refused individually and reported by
`spice doctor`; built-in commands and valid sibling mounts remain available.
For example, these entries are refused because the full paths already resolve
to registered actions:

```toml
[commands]
"study.csharp-members" = "./scripts/csharp-members.sh"
"dev.pre-commit" = "./scripts/pre-commit.sh"
```

This is allowed when no `spice study repo-tool` action is registered:

```toml
[commands]
"study.repo-tool" = ["project-tool", "study", "repo-tool"]
```

Values are command strings or argv lists; remaining CLI arguments are passed
through verbatim.

## `[locks]`

Resource locks coordinate exclusive local resources such as editor instances,
emulators, databases, license seats, or a fixed pool of sandbox shards. The
tracked table declares the resources; `spice lock run` holds one while a child
command runs and releases it when that child exits.

| Key | Meaning |
| --- | --- |
| `lock_contention_exit_code` | Default exit code when a named lock is held. |
| `chosen_shard_contention_exit_code` | Default exit code when a specifically requested pool shard is held. |
| `pool_exhaustion_exit_code` | Default exit code when no pool shard is free. |
| `state_root` | Repository-relative root for derived named-lock and pool paths. |
| `named.<name>` | Named-lock registry; entries accept `path` and `contention_exit_code`. |
| `pools.<name>` | Pool registry; entries accept `directory`, `shards`, `chosen_shard_contention_exit_code`, and `pool_exhaustion_exit_code`. |

```toml
[locks]
lock_contention_exit_code = 75
chosen_shard_contention_exit_code = 76
pool_exhaustion_exit_code = 77

[locks.named.editor]
path = ".spice/locks/editor.lock"
contention_exit_code = 75

[locks.pools.android]
directory = ".spice/locks/android"
shards = 3
chosen_shard_contention_exit_code = 76
pool_exhaustion_exit_code = 77
```

`spice lock run editor -- project-tool edit` acquires the single named lock.
`spice lock run android --pool -- project-tool test` acquires the first free
pool shard. `spice lock run android --pool --shard 1 -- project-tool test`
requires a specific zero-based shard.

Default state paths live under `.spice/locks/` when a resource omits `path` or
`directory`. Each held lock writes JSON holder metadata into its lock file with
`pid`, `cwd`, and `started_at`; `spice lock status --json` lists configured
locks and pool shards with that metadata without acquiring the resource locks.
A non-empty malformed metadata record reports `unknown`, never `free`.
Contention exits name the recorded holder on stderr. Per-invocation flags
such as `--path`, `--directory`, `--shards`,
`--lock-contention-exit-code`, `--chosen-shard-contention-exit-code`, and
`--pool-exhaustion-exit-code` override the tracked defaults for that one run.

## `[policy]`

The policy table extends the constitution. Exact defaults come from the
packaged manifest above; `spice.policy` exports frozen base values only for
resolver and compatibility seams.

`exclude`, `generated_paths`, `test_paths`, language suffix families, and
generated-file patterns are domain datasets rather than entry selectors.
Path-bearing datasets retain those names and delegate path evaluation to the
shared PATHPOL matchers.

| Key | Meaning |
| --- | --- |
| `package_roots` | Python namespace-package roots; derived from packaging metadata when unset. |
| `name_cluster_threshold` | Sibling prefix/suffix cluster size before namespace packaging is required. |
| `exclude` | Tracked paths/globs skipped by study walkers. |
| `generated_paths` | Tracked paths/globs exempt from repo-shape guards. |
| `test_paths` | Test roots for production/test classification. |
| `repo_truth.docs` | Doctrine docs checked because they ride in agent context. |
| `env_name_patterns`, `env_names`, `env_access_gate` | Env literal watchlist, exact manifest, and access-waiver gate. |
| `reachability_providers` | Extra language-aware dead-code providers. |
| `csharp_unused_retention` | C# unused-candidate declarations for framework-retained base types, interfaces, and attribute names. |
| `python_typecheck_interpreter` | Optional interpreter for `python-typecheck`; otherwise repo venv/uv is resolved. |
| `assertion_helpers` | Callable names that count as test assertions. |
| `internal_couplings` | Named private-internals allowlist entries `{ path, test, target }`. |
| `suite_seam` | Paths the whole suite depends on, and the suite command that gates a task landing that touches one. |
| `pre_commit` | Extra gate steps that run after built-in pre-commit gates. |
| `pre_commit_success` | Success-only steps that run after the gate passes. |
| `pre_commit_builtins` | Per gate-only pre-commit built-in overrides for `merge-integrity`, `plan-phase`, `repo-shape`, `staging`, `repo-docs`, `config-key-validity`, `config-false-disable`, `config-tracked-trust`, `formatters`, `local-paths`, `taste`, `serve-web-typecheck`, `javascript-unused`, `python-typecheck`, `env-policy`, `env-name-ledger`, `file-shape`, `complexity`, `magic-numbers`, `markdown-links`, `reachability`, `symbol-reachability`, `python-unused`, `assertion-free-tests`, and `private-internals`. |
| `limits`, `flex`, `rules` | Base bounds, sticky headroom, and applicability-selected overrides. |
| `languages`, `lockfiles`, `file_shape`, `env_access` | Suffix/pattern families for grammar-aware gates. |
| `complexity`, `taste`, `magic`, `debt`, `commit_message` | Gate-specific knobs. |
| `markdown_depth_budget` | Generated repo-doc character scopes for markdown. |

Shell env-access patterns intentionally cover name-like parameters, not shell
special or positional parameters such as `$?`, `$$`, `$1`, `$@`, `$*`, `$#`,
`$-`, or `$_`.

Detailed dataset, rule, suite-seam, taste, commit-message, and built-in gate
contracts live in the [policy configuration companion](policy.md). The
packaged-default manifest remains the authority for their exact default values.

## `[maxim]`

The singular table controls maxim judging and proposal generation:

| Key | Meaning |
| --- | --- |
| `max_attempts` | Maximum ambiguous judge replies before a maxim check fails. |
| `parallel_judges` | Number of concurrent verdicts used by any-violation evaluation. |
| `proposal_min_recurrence` | Minimum recurring ACK evidence required to form a proposal theme. |
| `proposal_draft_max_words` | Maximum trigger phrases retained in a generated proposal draft. |
| `prompt_lines` | Ordered prompt framings shuffled for the default judge template. |

## `[maxims.<bag>]`

Maxim bags extend or replace the live prose conscience.

| Key | Default | Meaning |
| --- | --- | --- |
| `words` | required for new bags; inherited for built-ins | Alphabetic trigger words or phrases. |
| `message` | required for new bags; inherited for built-ins | The maxim text published to the agent as steering on a match; when adjudication is enabled it is sent to the judge first and published only on a violation verdict. |
| `scopes` | `{}` (unconstrained) | Universal applicability selector. Maxim bags support the shared `drivers` axis; cite `spice maxim report` evidence before narrowing it. |

```toml
[maxims.routes]
words = ["quiet route"]
message = "Respond to the real event instead."
scopes = { drivers = ["codex"] }
```

Bag names are case-folded. Trigger phrases are normalized to lowercase words.
Configured bags merge with built-ins, so a repo can tune existing bags or add
new curated near-universal preferences. An absent `scopes` leaf applies to all
drivers. The displaced per-bag `drivers` key is unsupported; maxim driver
selection uses the same normalization, validation, matching, and explanation
contract as every other `scopes.drivers` consumer.

Watchdog reminders are deduped by content-derived reminder key within one
compaction epoch. A later compaction can make the same key eligible to publish
again because the agent may have lost the earlier inbox steering, but the
compaction count never changes the configured `message` text.

## `[tasks]`

| Key | Meaning |
| --- | --- |
| `base_stems` | Built-in public and internal project-stem vocabulary. |
| `internal_stems` | System-owned stems excluded from lane assignment. |
| `hidden_stems` | System-owned hidden stems, written without the leading dot. |
| `oops_hidden_stem` | Hidden stem used for tooling-friction triage. |
| `maxim_proposal_hidden_stem` | Hidden stem used for maxim-proposal triage. |
| `approved_phases` | Valid task phase vocabulary. |
| `phase_slot_count` | Number of phase UDA slots generated in Taskwarrior configuration. |
| `default_flow` | Phase sequence for ordinary public tasks. |
| `private_default_flow` | Phase sequence for private and non-oops hidden tasks. |
| `oops_default_flow` | Phase sequence for oops triage tasks. |
| `default_priority` | Long-form priority selected when creation omits one. |
| `severities` | Valid tooling-friction severity vocabulary. |
| `project_min_depth`, `project_max_depth` | Inclusive dotted-depth bounds for public task projects. |
| `claim_ttl_seconds` | Default claim lease duration. |
| `claim_context_seconds` | Transcript window captured on each side of a claim instant. |
| `deferred_wait` | Durable wait timestamp for deliberately deferred work. |
| `oops_wait_seconds` | Initial wait duration for newly captured oops work. |
| `allocator_band_width` | Native-urgency distance within which locality may rank allocator candidates. |
| `allocator_anti_self_review` | Taskwarrior urgency coefficient penalizing self-authored reviews. |
| `priority` | Long-form priority aliases to Taskwarrior priority letters. |
| `severity_priority` | Severity-to-priority mapping for oops tasks. |
| `severity_shorthands` | Single-letter severity aliases. |
| `priority_urgency` | Taskwarrior urgency coefficients for priority letters. |
| `taskwarrior_urgency` | Native Taskwarrior urgency coefficients used in generated taskrc files. |
| `sla_due_seconds` | Due-date offsets keyed by priority letter. |
| `reports.<name>` | Named report registry; each entry requires `description`, `filter`, and `sort`. |
| `analytics.commands` | Taskwarrior analytics command names exposed by diagnostics. |

Repositories may additionally declare `stems`, `flows`, and `phase_models`.
Those extension registries have no packaged entries.

## `[tasks.phase_models.<driver>.<phase>]`

Per-driver, per-phase agent launch overrides. Each driver has its own model
space, so the table is keyed by driver name (`claude` or `codex`) and then by
task phase (`design`, `plan`, `todo`, `verify`, or `review`).

| Key | Default | Meaning |
| --- | --- | --- |
| `model` | unset | Model to launch with while the worktree's claimed task sits in this phase. |
| `effort` | unset | Reasoning effort to launch with for the same phase. |

```toml
[tasks.phase_models.claude.plan]
model = "claude-opus-4-8"
effort = "high"

[tasks.phase_models.claude.todo]
model = "claude-sonnet-5"
```

`spice agent ensure` reads the phase of the worktree's currently claimed task
and looks it up in this table for the active driver. A phase with no entry
(or no claimed task) falls back to the ordinary resolution order: an explicit
`--model`/`--effort` flag, then the effective `agent` table in `worktree`,
`repository`, and `system` precedence order, then the driver's shipped default.

These `model` values are launch payload selected by live task-phase routing;
they are not applicability selectors. A `scopes.models` entry filters a
consumer that declares model applicability and does not participate in lane or
task allocation.

## `[serve]`

| Key | Meaning |
| --- | --- |
| `brand` | Fallback header and browser-title brand; a repository project name may replace the packaged value. |
| `default_lifetime` | Initial serve lane lifetime: `Steer` uses manual filters, `Drive` auto-subscribes to projects the team creates or claims, and `Drain` dissolves the task boundary so all assignable work is visible. |
| `valid_lifetimes` | Accepted lane-lifetime vocabulary. |
| `host` | Default bind host for `spice serve`, `spice watch`, and `spice demo`. |
| `port` | Default bind port for those serve surfaces. |
