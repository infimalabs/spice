# Bounded coverage-subsumption study — 2026-07-16

## Reproducible recording

The development dependency set now declares `pytest-cov`. A fresh checkout can
record and analyze branch-aware per-test coverage with:

```console
uv sync --group dev
uv run spice study subsumption --record --package spice
```

The recorder sets `COVERAGE_FILE` to an explicit path, disables pytest's caller
checkout cache, puts pytest temporary files and JUnit output under a temporary
base, and starts Python with `-B`. The default coverage database is disposable.
`--retain-coverage PATH` is the only way to retain it, and an existing path is
never overwritten. The pytest child runs under the named 600-second `coverage`
deadline and process-group cleanup policy, so normal completion, test failure,
timeout, and parent interruption all unwind the temporary directory without
creating an implicit `.coverage` file in the caller checkout.

This recorded run explicitly retained its database outside the checkout:

```console
uv run spice study subsumption --record --package spice \
  --retain-coverage /tmp/spice-a-subsumption-20260716T0140-2.db \
  --limit 25
```

## Run denominator

| Measure | Count |
| --- | ---: |
| Total pytest suite | 2,249 |
| Analyzed per-test contexts with `spice` production features | 1,421 |
| Production files represented | 159 |
| Distinct normalized coverage contexts | 1,421 |
| Normalized contexts excluded by the package filter | 0 |
| Context-free coverage contexts | 1 |
| Suite tests without an analyzed production context | 828 |
| Coverage-containment candidates | 316 |
| Deterministic cohorts | 239 |

The denominator is therefore the 1,421 analyzed test contexts, not the full
2,249-test suite. The 828-test difference includes tests that execute no
instrumented `spice` production feature; the report does not imply whole-suite
coverage for them.

The stable identity, relation, candidate count, and representative for every
cohort are recorded in
[`subsumption-cohorts-2026-07-16.tsv`](subsumption-cohorts-2026-07-16.tsv).

## Bounded adjudication

The largest cohort contained 31 candidates and was intentionally excluded by
the 25-finding cap. The highest-count cohort within the cap was named the
**session briefing signal-integrity cohort**:
`strict-subset-6b1cb7dcac79`, five candidates, coverage-contained by
`tests/test_sessionbriefinginvariants.py::test_supervised_fixture_outputs_exclude_skill_mantra_text`.

Coverage containment was treated only as candidate evidence. The representative
is a broad supervised-fixture integration assertion about removing skill mantra
text; it does not assert any candidate's independent contract.

| Candidate | Decision | Independent behavioral evidence |
| --- | --- | --- |
| `test_compaction_intent_candidates_use_parsed_summary_intent` | Retain | Pins parsed `intent_text` as the candidate text and the prior assistant text as its label. The representative never asserts compaction-intent field selection. |
| `test_rehydration_command_candidates_order_by_failures_then_recency` | Retain | Pins failure-first ordering and recency within the failure class. The representative only renders fixtures and cannot distinguish this rank-key regression. |
| `test_rehydration_file_candidates_order_by_last_touch_then_hotspot` | Retain | Pins last-touch ordering and hotspot tie-breaking with deliberately conflicting counts. The representative has no assertion over file candidate order. |
| `test_sweep_horizon_caps_requested_count` | Retain | Pins the five-window hard cap, horizon basis, final window, and current ask when the request exceeds the cap. The representative does not exercise the over-limit request contract. |
| `test_briefing_repo_root_timeout_reports_cwd_identity` | Retain | Pins the bounded provider's phase and resolved-cwd input identity on timeout. Coverage overlap cannot substitute for this failure-surface contract. |

Result: five retained, zero pruned. The suite footprint remains 2,249 tests
before and after; test files and test LOC are unchanged. Focused behavior
validation ran the five candidates plus the representative and passed 6/6.
The complete coverage recording run passed 2,249/2,249 tests.

## Remaining work and mutation overlap

The remaining 238 cohorts contain 311 candidates. They are captured as deferred
task `SUBSUMP-1kDf7Hhb`, keyed to the stable manifest and constrained to batches
of at most 25 findings.

This repository has no persisted mutation ratchet or named recurring mutation
suite. The mutation study accepts ad-hoc `--test` targets; the selected briefing
tests would be relevant targets when mutating their corresponding session
modules, but no current mutation cohort independently marks them redundant.
Subsumption therefore remains an ad-hoc review diagnostic. A separate recurring
surface is not warranted from this all-retain cohort; that decision can be
revisited only after deferred batches demonstrate repeated behavior-backed
pruning value.
