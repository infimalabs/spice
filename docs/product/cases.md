# Case battery

Each case names the expected behavior and the source rule it exercises. They are
written to become tests; together they cover every entry in the invariant
register.

---

## Where to start

Ordered by how quietly the property fails. Passing only these does not prove the
product contract; failing any of them disproves it.

| Property | Proof |
| --- | --- |
| Evidence isolation | run the gates with the candidate's module path exported; the probe still refuses |
| Rendering never launches | open every read surface with agents stopped; assert zero starts |
| Layout purity | replay recorded event logs; assert identical positions |
| Zero-write on no-op | count style writes on an unchanged render; must be 0 |
| Single wake per decision | several ready tasks, all lanes stopped; exactly one starts |
| Starvation is an age | one ready task; starts only after the age |
| Semantic retirement | read without acking; count unchanged |
| Landing atomicity | interrupt a landing; tree is pre-merge or recoverable |
| Install reversibility | install twice, then unapply; the operator's original state returns |
| Footprint containment | land a diff touching a foreign path; refused |
| Review softness | single-agent fleet completes a review phase |
| Claim exclusivity | concurrent `next`; distinct rows |
| Takeover notification | expire a claim; displaced lane receives an inbox item |
| Priority inheritance | critical blocked by trivial; trivial is handed out first |
| Lane persistence | renew mid-draft; draft, filters, history survive |
| Discovery fails closed | fail enumeration; zero lanes close |
| Facet independence | deliver facets out of order and twice; state converges |
| Overlay discipline | open every menu; no layout box changes |
| Projection loudness | corrupt a projection; the answer differs from a healthy empty |
| Repair-first refusals | every refusal with a way out leads with a runnable command |
| Provenance | every effective config value names its source |
| Observer inertness | run observation; assert no repo, team, claim, or hook writes |

---

## The battery

Grouped by behavior for navigation. Each situation is its own durable, descriptive identity.

### Families

- [Core work](cases/core.md) — Steering; Claims and allocation; Capacity and restart; Integration; Lifecycle; Topology; Reading surface; Audio
- [Supporting planes](cases/planes.md) — Checks; Session and counsel; Metrics; Task documents; Extension and wire; Distribution and observation; Identity and authority; Work, continued; Lifecycle, continued; Topology and surface, continued; Checks, session, extension, continued; Conduct
- [Surface and checks](cases/surface.md) — The rewind guard; The composer strip; Check mechanics
- [Release](cases/release.md) — Release as a plan; Release gates and evidence; Release notes; Publication
- [Installation and session](cases/session.md) — Installation; Reversal and the ownership history; Session — sources, window, budget; Session — windows and the record; Session — meter, effort, learnings
- [Orchestration and audit](cases/conduct.md) — Affinity and anti-affinity; The prepended plan phase; Steering escalation and the shell boundary; Maxim bags; Audited by review, not settled by a case
