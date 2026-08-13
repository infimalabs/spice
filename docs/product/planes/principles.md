# Product model — supporting planes — Principles and freedom

[Product model — supporting planes](../planes.md) · [Spice product model](../README.md)
## Cross-cutting principles

The rules that generate everything else. If you keep one page, keep this.

- **One authority per fact.** Every fact has exactly one owner and its own
   freshness counter. No global revision, no re-derivation from a cheaper source.
- **Derive, never duplicate.** Observation folds the authority's own history
   rather than writing a parallel event log. Projections are rebuildable by
   construction; authority is not.
- **Durable before delivered.** Intent is persisted before any attempt to
   deliver it. Delivery is retryable; the intent is never recreated.
- **Semantic retirement.** Nothing is retired by being read. Acceptance and
   refusal are equally first-class and equally terminal.
- **One visible path.** No shims, aliases, fallbacks, or legacy branches.
   Durable data gets exactly one forward migration; everything else is replaced
   outright.
- **Fail loudly; never degrade silently.** A component that read nothing must
   not produce the same output as one that read everything.
- **Prefer the good property; never block on it.** Cross-review, fast-forward at
   launch, rewriting, adjudication — each is attempted, none is required. The
   work always has a way forward.
- **Refusals lead with the repair.** The reader who has just been stopped has no
   second screen. The message is the whole interface.
- **Movement means change.** Any motion corresponds to something that actually
   changed. Re-render is not change.
- **The agent is a user.** It has a UX — briefing, meter, reminders, refusals —
    and that UX has a required *tone*: capability, not restriction.

---

## What is fixed and what is free

| Layer | Verdict |
| --- | --- |
| Language, framework, storage engines | **free** |
| Visual design, colour, layout, iconography | **free** |
| Transport framing, serialization | **free** |
| Track counts, span ladders, tween timings | **free** |
| Phase *names*, handle alphabet, key derivation | **free** |
| One authority per fact, per-facet freshness | **fixed** |
| Durable-before-delivered; semantic retirement | **fixed** |
| Claim exclusivity, lease, takeover-with-notice | **fixed** |
| The three git boundaries; agents never push or pull | **fixed** |
| Landing atomicity and footprint containment | **fixed** |
| Cross-review preferred, never required | **fixed** |
| Lane is not agent | **fixed** |
| Rendering never launches | **fixed** |
| Movement means change | **fixed** |
| Refusals lead with the repair | **fixed** |
| Loud degradation | **fixed** |
| Observation initializes nothing | **fixed** |

**The test for "fixed":** if changing it would make an operator distrust the
system, or leave an agent unable to recover on its own, it is fixed. Everything
else is spelling.
