# Product model — core

The model of the product: every entity, state machine, decision policy,
boundary, failure mode, and journey, stated independently of any implementation
of them.

Nothing here describes an implementation. Where a number appears it is **policy**
with the reasoning that chose it, not a constant to copy blindly.

The invariant register owns every cross-document label; local statements are
identified by their language and enclosing section.

---

## Contents

- [Orientation and domain](model/domain.md) — Orientation; Domain model
- [Entity state machines](model/states.md) — Entity state machines
- [Decision policies](model/decisions.md) — Decision policies
- [Control loops and boundaries](model/boundaries.md) — Control loops; Boundaries and seams; Concurrency and serialization
- [Failure, journeys, and ontology](model/failures.md) — Failure and degradation; Journeys; Ontology
