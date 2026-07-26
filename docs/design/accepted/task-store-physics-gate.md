# Task-Store Physics Gate

Status: recommendation, 2026-07-25.

## Recommendation

Do not proceed with the full Taskwarrior-to-owned-store migration on the
current evidence. Stand the remaining STORE substrate battery down after this
gate is reviewed.

The revision-owned Serve projection removed the dominant read-side movement:
at a stable task revision, one lane payload now launches no Taskwarrior
process, performs no full-board export, and serializes no task-board bytes.
The write path still persists four representations and still has
cross-representation failure windows, but its authoritative TaskChampion row
mutation is already atomic. The remaining witness and lifecycle copies are
event-rate costs and projection-consistency risks, not per-payload board
movement. Those residual costs do not presently outweigh a 23-consumer,
mixed-version migration of the live coordination plane.

This is a recommendation about the full substrate replacement, not a claim
that the write path is finished. Targeted work may still make the claim witness
and lifecycle facts derived from one native mutation stream, and should be
preferred before reopening the store migration.

## Read-Side Measurement

### Method

The measurement ran on the live backend through production entry points:

- `tw.run(["status.any:", "export"])` measured the raw Taskwarrior process and
  exact UTF-8 stdout size.
- `messages_payload_for_worktree()` measured the standalone payload for this
  worktree and thread with `limit=5`.
- Wrappers around `tw.run` and `tw.export` counted Taskwarrior child processes
  and full-board exports. The task revision was held constant during the warm
  sample so concurrent fleet mutations could not turn a cache-hit measurement
  into a legitimate new-revision rebuild.
- The warm result is 30 sequential production payloads after one observation
  build. Latency includes transcript, inbox, lifecycle, agent-status, and wire
  work; only the task revision was pinned.

The live board contained 1,450 rows. Its measured export was 7,912,733 bytes
(7.54 MiB). A separate five-run raw export sample measured 7,910,648 bytes and
a 174.5 ms median; the few-kilobyte difference is live board mutation between
the samples.

### Results

| Lane payload case | Board rows | Taskwarrior processes | Full-board exports | Board JSON moved | Payload JSON | Latency |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 2026-07-25 baseline, warm | 1,330 | 4 | 4 | about 27.2 MiB (four copies of an approximately 6.8 MiB board) | not recorded | 671 ms |
| Revision-owned projection, warm stable revision | 1,450 | 0 | 0 | 0 bytes | 18,640 bytes | 376.1 ms median |
| Revision-owned projection, observation miss | 1,450 | 1 | 1 | about 7.91 MB once | 18,640 bytes | 369.5 ms median |

The warm 30-sample distribution was 346.1 ms minimum, 376.1 ms median,
406.5 ms mean, 626.4 ms nearest-rank p95, and 692.5 ms maximum. The older
baseline recorded four exports taking 563 ms of its 671 ms payload. The new
warm path eliminates all four exports and their board bytes, while reducing
the observed median payload latency by 43.9%.

The observation-miss row is a five-sample cache-clear measurement and exists
to price the retained work: one revision materialization, shared by all lanes
and task-derived surfaces until the next task mutation. It is not the warm
comparison. A raw full-board export by itself measured 167.6–180.5 ms across
five runs, with a 174.5 ms median.

The deterministic contract remains stronger than the wall-clock sample:
`test_measured_board_moves_once_across_inventory_messages_and_metrics` proves
that eight concurrent first readers, two inventory lanes, two standalone
message payloads, and on-demand metrics perform exactly one export and exactly
one normalization per each of 1,330 rows.

## Write-Side Measurement

### Method

The write probe used an isolated temporary Git repository and isolated
Taskwarrior/TaskChampion and Serve-team databases. It created a two-phase task,
then called the production `claimstate.do_claim()` and `ops._advance()` paths.
Before and after each event it inspected:

1. the exported current task row,
2. TaskChampion `operations` rows for that task UUID,
3. the actor's `claim-witness.json`, and
4. ServeTeamStore `task_events` rows for that task UUID.

Taskwarrior process calls were instrumented as an ancillary count. Both
mutations also advanced the task-revision marker used to invalidate the Serve
observation. That marker is a persisted wake signal, not another semantic
claim or phase representation, so it is reported but not counted as a fifth
fact copy.

### Claim Event

| Persisted representation | Observed write |
| --- | --- |
| Current TaskChampion task row | `start` plus 12 claim fields became active in one guarded `modify` |
| TaskChampion operations log | 14 `Update` rows: 12 claim fields, `start`, and `modified` |
| Per-actor claim witness | one new 130-byte JSON record with `active=true`, actor, handle, and task UUID |
| Serve lifecycle facts | one `task_events` row with kind `claim` |

The event launched two Taskwarrior processes: the guarded row mutation and an
exact-UUID export used to construct the witness. The task-revision marker
advanced once.

### Phase Advance

| Persisted representation | Observed write |
| --- | --- |
| Current TaskChampion task row | phase moved from `todo[0]` to `review[1]`; native active state and claim fields cleared; readiness timestamp and review author recorded |
| TaskChampion operations log | 17 `Update` rows for phase/index, claim clears, `start`, readiness, review author, and `modified` |
| Per-actor claim witness | the same file was atomically rewritten from 130 bytes active to 118 bytes with `active=false` |
| Serve lifecycle facts | one `task_events` row with kind `phaseAdvance` |

The event launched two Taskwarrior processes: one exact task readiness export
and one row mutation. The task-revision marker advanced once.

### Transaction Boundary

The four representations do not share a transaction:

- Claim commits the authoritative task mutation before exporting the row,
  writing the witness, and inserting the lifecycle fact. A crash can therefore
  leave a valid live claim without its witness or metric event.
- Phase advance commits the phase and claim release before retiring the
  witness and inserting `phaseAdvance`. A crash can therefore leave an active
  witness for a released row or omit the lifecycle fact.

This is real duplication. It also explains the previously measured parity
finding: retained-history comparison matched all 662 completions while claim,
phase-advance, and review event counts diverged. The important qualification
for this gate is that allocation authority stays in the TaskChampion task row:
the claim fields plus `start` are changed atomically, stale takeover uses an
owner-and-deadline compare-and-swap guard, and phase plus claim release are one
row mutation. The witness is recovery/notification state and `task_events` is
an observation fact series.

## Stop/Go Balance

### What still argues for an owned store

- Four semantic representations span TaskChampion current state and operation
  history, a per-worktree JSON witness, and ServeTeamStore lifecycle facts.
- No transaction can commit or roll back those technologies together.
- The external binary, generated taskrc, schema overrides, subprocess
  deadlines, and repair paths remain product and maintenance weight.
- An owned transaction could make current state, one semantic ledger event,
  and one monotonic revision commit together.

### What now argues against the migration

- The read-side reason has materially changed: warm stable-revision payloads
  pay zero processes, exports, and board bytes instead of four full exports.
- The two authoritative task transitions measured here are already atomic
  inside TaskChampion. The residual split concerns witness recovery and
  lifecycle observation rather than two competing allocation owners.
- The behavior ledger pins 23 consumer modules and 30 command fragments. A
  replacement must also preserve READY/ACTIVE derivation, urgency, annotations,
  dependencies, dates, handles, operation history, fleet quiescence, and
  stale-version fail-closed behavior across 1,450 live rows.
- Retiring Taskwarrior changes stated product doctrine and concentrates risk in
  a one-time migration. The read win reduces the ongoing benefit available to
  repay that risk.

The current stop/go result is therefore **NO-GO for the full STORE substrate
build**. Reopen only if a residual claim race escapes the existing atomic row
and compare-and-swap boundary, Taskwarrior becomes a material distribution or
reliability blocker, or targeted derivation of witness/lifecycle facts from one
native transition stream proves insufficient. Any reopened case must use the
new zero-export warm baseline, not the superseded four-export cost.
