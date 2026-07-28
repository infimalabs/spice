# Policy Configuration

This companion to the [configuration reference](reference.md#policy) documents
the detailed policy datasets, rules, and executable gates.

## `[policy.languages]`

Suffix families for `complexity`, `magic`, `env`, and `c_grammar` scans.

## `[policy.lockfiles]`

Generated lockfile `suffixes` and `names` exempt from file-shape pressure.

## `[policy.file_shape]`

`source_suffixes` selects files for LOC/byte pressure. `generated_patterns`
exempts generated sources such as protobuf modules, minified bundles, and build
outputs.

## `[policy.env_access]`

`family_suffixes` maps language families to suffixes. `default_patterns` maps
families to env-access regexes; custom pattern families must have suffixes.
`baseline` points at existing `env-policy` findings. The env-name ledger only
accounts for extractable literal names and scans tests like production.

## `[policy.suite_seam]`

Per-lane verification is a subset twice over. An agent runs the tests that name
the module it changed, and for a widely depended-on module that direct-import
view understates the real reach by an order of magnitude. It then runs that
subset against its own pinned baseline, which the other lanes have moved since.
Both gaps close only on the integrated tree, so this gate runs there: after
`spice task done` merges the task onto the baseline and before it pushes.

`paths` lists the repository paths whose reach is the whole suite. A task whose
footprint touches one runs `run` -- the whole suite -- against the merged tree,
and a red suite refuses the publish with the merge left in the tree to fix. A
task that touches nothing declared matches nothing and runs nothing, so only the
landings that need the coverage pay for it, and no commit pays at all.

`run` is an argv list, kept in order and with repeats intact. Optional `seconds`
is the cost the repository accepts when the gate fires; the gate prints it before
starting and reports the measured wall clock afterwards, so a stale declaration
is visible on every seam landing.

```toml
[policy.suite_seam]
seconds = 200
run = ["spice", "dev", "pytest", "-q", "--ignore=tests/browser"]
paths = ["spice/tasks/tw.py", "spice/policy.py"]
```

Choose `paths` by measurement rather than by feel: a path belongs here when it
is transitively reachable from most of the test suite, which is the condition
that makes a lane's own subset misleading. `spice study suite-seam-reach` is
that measurement, and it fixes the terms the answer turns on -- a test module
is a collected `test_*.py` file under the configured test roots, and reach
follows imports wherever they appear, including inside function bodies. It
ranks every package module by how many test modules reach it, alongside how
many name it directly, so a candidate is compared against the whole ordering
rather than judged alone. The angle-bracketed fields below are filled from the
live graph:

```console
$ spice study suite-seam-reach --limit 30
suite-seam-reach: <declared> declared module(s) of <package-modules>, reached by at least <floor> of <test-modules> test module(s)
suite-seam-reach: <widest-undeclared-path> leads the undeclared rest at <reach>, so the band is <verdict>
  <reach> reached <direct-imports> imported  <package-path> [declared]
  ...
```

The command's two header lines are the decision. The first reports the reach
of the narrowest module in the declared band; the second reports the widest
module left out. When the first is greater than the second, the declaration
names a group the import graph already separates. The command is the source of
these point-in-time figures, which change as tests and imports change, and
exits non-zero when the break closes. `--json` emits the same ranking for a
repository that wants to consume it.

A repository that gates on this should assert the result rather than restate
it. In this one,
`tests/test_suiteseam.py::test_this_repository_declares_exactly_the_widest_reaching_modules`
requires the declared paths to occupy the leading slots of that ranking and the
boundary below them to be a strict break, so a path added by feel, or a module
that grows into the band without being declared, fails the suite with the
ranking in hand.

A red suite here is a refusal to publish, not a lost merge. The integrated tree
stays checked out, so the failures reported are the ones the branch would have
taken; fix them, commit, and run `spice task done` again.

## `[policy.csharp_unused_retention]`

Tracked declarations for C# members that are reached by framework convention
rather than by direct C# references. The table only adds retained findings; the
built-in partial-declaration and attribute-retention defaults still apply when
the table is absent or when no declaration matches.

```toml
[policy.csharp_unused_retention]
base_types = ["HostedServiceBase"]
interfaces = ["IPluginModule"]
attribute_names = ["ServiceEntryPointAttribute"]
```

For example, a dependency-injection container may instantiate every class
derived from `HostedServiceBase`, while a plugin host may discover classes that
implement `IPluginModule`. Private methods and fields inside those types are
reported as retained with reasons such as
`configured_base_type:HostedServiceBase` or
`configured_interface:IPluginModule`. Attribute names match with or without the
`Attribute` suffix, so `[ServiceEntryPoint]` can match
`ServiceEntryPointAttribute` and records
`configured_attribute:ServiceEntryPointAttribute`.

Policy constants enforced by default: files `1000` LOC / `80000` bytes with
`1.5x` flex, routines CCN `20` / length `80`, commit text wrap `100`,
repo-root markdown `10000` chars plus `10000` per nested directory until
`30000`, magic-number threshold `10`, and magic baselines against `HEAD`.

## `[policy.limits]`

Base caps: `file_loc`, `file_bytes`, `routine_ccn`, `routine_length`,
`commit_message_wrap`, and `repo_truth_doc_chars`.

## `[policy.flex]`

Default `ratio` is `1.5`; explicit per-bound flex caps override it. Breaching
flex makes the item sticky until it shrinks under the base cap.

## `[policy.complexity]`

| Key | Default | Meaning |
| --- | --- | --- |
| `hotspot_limit` | `20` | Default number of rows shown by `spice study complexity-hotspots` when `--limit` is omitted. |

## `[policy.taste.words]`

The authoritative built-in map is `policy.TASTE_WORD_SUGGESTIONS`. It feeds
`spice study taste`, the staged pre-commit taste gate, and task-creation wording;
file scans cover tracked `.md`, `.txt`, and `.rst` prose. The defaults include
explicit singular, plural, past-participle, and gerund suggestions for
`allowlist`, `allowlists`, `allowlisted`, and `allowlisting`, plus `blocklist`,
`blocklists`, `blocklisted`, and `blocklisting`.

A bare key matches one whole word case-insensitively. Only a trailing `*` opts
into stem matching and covers every word-character suffix. Values are the exact
suggestions shown to the user; an empty value means remove or rephrase.

The resolver starts from the built-in map, then normalizes repository keys to
lowercase and assigns repository entries in TOML order. A matching normalized
key replaces only that suggestion; new keys extend the map, and every other
built-in entry remains active.

## `[policy.markdown_depth_budget]`

Generated `repo_truth_doc_chars` scopes for tracked markdown: repo root gets
`10000` chars, one nested directory `20000`, two nested directories `30000`,
and deeper docs are unlimited. `extensions` defaults to `[".md"]`; set it to
`[]` to replace generated rules with explicit `[[policy.rules]]`
entries.
`stem_pattern` optionally full-matches file stems; binary files are skipped.

## `[policy.debt]`

Allowed-finding counters, not size limits. Defaults are `0` for
`reachability_test_only` and `assertion_free_tests`; non-zero values are
explicit cleanup debt.

## `[[policy.rules]]`

Each policy rule is one payload with an inline universal selector:

```toml
[[policy.rules]]
scopes = { paths = ["Docs"], extensions = [".md"] }

[policy.rules.repo_truth_doc_chars]
min = 20000
flex = 1.25
```

`paths` uses the canonical PATHPOL glob-or-subtree contract without a
policy-only matcher variant; `extensions` composes with it through the universal
AND-across rule. Flat rule keys apply to every numeric bound; named sub-tables
target `file_loc`, `file_bytes`, `routine_ccn`, `routine_length`,
`commit_message_wrap`, or `repo_truth_doc_chars`. Settings accept `multiplier`,
`min`, `max`, `unlimited = true`, and optional `flex`. A nested
`magic.examine_threshold` overrides magic-number scanning. Universal selector
specificity chooses the winning applicable rule; repository-authored rules
outrank generated markdown-depth rules.

## `[policy.magic]`

`examine_threshold` defaults to `10`; `baseline_ref` defaults to `HEAD`.

## `[policy.commit_message]`

`allowed_trailers` optionally limits Git trailer keys to a finite set;
`blocked_trailers` optionally rejects specific keys. Both are unset by
default, so every trailer -- including `Co-Authored-By` -- rides through.
When a configured policy would reject the attribution trailer
(`Co-Authored-By`), spice also disables the native driver's attribution so it
never emits a trailer the commit-msg gate then rejects.

Command-step tables accept:

`label`, `mount`, `run`/`argv`, `scopes`, `formatter`, and `enabled`.
`pre_commit` steps receive `SPICE_STAGED_PATHS`; mounted steps also receive
`SPICE_MOUNTED_COMMAND=1` and `SPICE_VISIBLE_PROG`. `scopes.paths` narrows the
staged-path set with the universal PATHPOL contract, while `scopes.phases`
selects `pre-commit` or `pre-commit-success`. `scopes.drivers` and
`scopes.models` select the effective configured worktree driver and model. All
four axes compose through the universal AND rule; omitting any axis means all
values on that axis.

Reachability provider tables accept `name`, `run`, and optional
`scopes = { paths = [...] }`. `name` must not be `python`. During staged scans,
the universal selector both decides applicability and narrows
`SPICE_STAGED_PATHS`; full scans run every configured provider. Providers write
JSON findings with `kind`, `subject`, `path`, and `imported_by`; `kind` routes
whole-file findings to `reachability` and symbol findings to
`symbol-reachability`.

`internal_couplings` entries accept `path`, `test`, and `target`; all are
required non-empty strings. `test` is the test function name or `<module>`, and
`target` is the private production symbol the test imports or reaches.

## `[policy.pre_commit_builtins]`

Each built-in key may be:

- `true` to keep the default.
- A mounted command name to replace it.
- A command-step table using `mount`, `run`, or `argv`.
- A command-step table may set `enabled = false`.

The shared registry-removal rule in the
[configuration reference](reference.md) applies to bare built-in entries.
Every pre-commit invocation prints each disabled built-in by name. A
disablement from tracked repository configuration is covered by the
executable-configuration digest approved through `spice init --apply`; `spice
doctor` reports a required failure until that current approval receipt exists.
Git-dir operator-owned disablements are already explicit operator intent and do
not require a repository approval receipt.
