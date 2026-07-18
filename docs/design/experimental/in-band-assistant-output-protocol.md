# In-Band Assistant Output Protocol

Status: prototype result, 2026-07-18.

## Decision

The emoji alias protocol is rejected and decommissioned. `ACK ...`, `NACK ...`,
and `TASK ...` remain the only canonical assistant-output control verbs. Do not
restore emoji or emoji-pair control markers without a new design decision based
on a concrete need that text verbs cannot satisfy.

## Prototype Result

The original recommendation proposed a Unicode-normalized registry that mapped
markers such as `🌶️✅` and `🌶️📋` onto the existing ACK/TASK semantics after
driver message reconstruction. It correctly required markdown suppression,
variation-selector handling, literal-pair and ZWJ tests, and no parallel control
plane.

That path was implemented in commit `c2ee1098` and its transcript-fidelity work
was implemented in `2d48dd69`. The operator then rejected the marker surface;
commit `398d9dd7` reverted the implementation and `bd9eaad0` reverted the
fidelity layer. Current `spice/mail` code has no emoji registry or scanner.

## Why It Stays Rejected

- Text ACK/NACK/TASK forms are already explicit, searchable, portable, and
  auditable.
- Emoji aliases add Unicode canonicalization and markdown-suppression risk
  without adding a new semantic capability.
- A compact alias can become accidental ceremony and makes discussion/examples
  harder to distinguish from control output.
- Vendor PostToolUse hooks solve ambient inbound steering coverage; they do not
  create a need for a second assistant-output vocabulary.

## Surviving Authority

`docs/design/accepted/semantic-ack-standalone-protocol.md` owns durable ACK
semantics. `docs/design/accepted/transparent-steering-injection.md` owns shell
and agent-native inbound delivery. Git history preserves the rejected prototype
implementation.

## Reopening Condition

Reopen only if measured operator or agent friction demonstrates a control
operation that canonical text verbs cannot express compactly and safely. A new
proposal must account for the prior reversion and prove that examples, quoted
text, markdown, Unicode variants, and reconstructed driver messages cannot fire
accidentally.
