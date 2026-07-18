# Bounded Coverage-Subsumption Study — 2026-07-16

Status: research, 2026-07-18.

## Conclusion

The bounded study is closed. Its highest-priority cohort produced five
coverage-containment candidates; behavior review retained all five and pruned
none. The result did not justify manually adjudicating the remaining 311
candidates, so deferred task `SUBSUMP-1kDf7Hhb` was deleted as a stale,
no-payoff queue.

Coverage containment remains an ad hoc diagnostic, not evidence that one test
subsumes another and not a recurring gate. A future study must record a fresh
run and earn a new bounded follow-up from demonstrated pruning value.

## Reproduction

A fresh checkout can record branch-aware per-test coverage with:

```console
uv sync --group dev
uv run spice study subsumption --record --package spice
```

The recorder uses an isolated coverage database, pytest cache, temporary files,
and JUnit output; `--retain-coverage PATH` is the only explicit retention path.
The pytest child runs under the named coverage deadline and process-group
cleanup policy.

## Recorded Denominator

| Measure | Count |
| --- | ---: |
| Total pytest suite at recording time | 2,249 |
| Analyzed per-test contexts | 1,421 |
| Production files represented | 159 |
| Suite tests without an analyzed production context | 828 |
| Coverage-containment candidates | 316 |
| Deterministic cohorts | 239 |

These are dated observations, not current suite inventory. The denominator was
1,421 analyzed contexts, not all 2,249 tests.

## Bounded Adjudication

The selected `strict-subset-6b1cb7dcac79` briefing cohort contained five
candidates. Each pinned behavior absent from the broader representative:

- parsed compaction-intent field selection;
- failure-first then recency command ordering;
- last-touch then hotspot file ordering;
- the five-window sweep hard cap and horizon basis;
- timeout reporting with resolved working-directory identity.

Focused validation passed the five candidates plus the representative, 6/6;
the complete recording run passed 2,249/2,249 tests. The adjudication retained
all five because coverage overlap could not replace their independent
assertions.

## Artifact And Queue Closure

The former `subsumption-cohorts-2026-07-16.tsv` represented a live 239-cohort
queue for `SUBSUMP-1kDf7Hhb`. Once that task was deleted, the TSV lost its
reviewed owner and became a stale generated snapshot. It is removed from the
active design tree and design-ledger allowlist. Git history preserves the exact
dated queue; regenerate a fresh disposable or task-sidecar manifest if another
bounded study earns follow-up work.

There is no persisted mutation ratchet or recurring mutation suite associated
with this result. The all-retain cohort provides no evidence for adding one.
