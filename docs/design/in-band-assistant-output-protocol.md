# In-Band Assistant Output Protocol

Status: recommendation, 2026-07-01.

## Recommendation

Build the protocol as a small alias layer over the existing text control verbs,
not as a second control plane. `ACK ...` and `TASK ...` remain canonical.
Emoji or emoji-pair markers are sugar the assistant may emit in its own prose
when a compact signal is better than text.

The first implementation should be a scanner and registry, not a hook rewrite:

- define a tiny marker registry mapping canonical emoji sequences to existing
  verbs such as ACK and TASK;
- scan only reconstructed assistant messages, after driver-specific transcript
  framing has already produced one assistant message body;
- reuse and extend the existing ACK/TASK suppression rules so quoted markers,
  fenced code, and examples never fire;
- normalize Unicode before matching, with explicit tests for NFC, variation
  selector 16, ZWJ sequences, and literal side-by-side emoji pairs.

Treat vendor hooks as the ambient inbound channel for non-shell tool stretches.
They complement the assistant-output protocol, but they are not the protocol
itself.

## Context

`PROTOCO-1k93MWlW` asks for an in-band protocol that lives in assistant output:
emoji or emoji-pair markers at turn boundaries or other structured moments,
read by the harness as lightweight control/framing. The purpose is duplex
control: the harness already pushes steering to the agent; this gives the agent
a compact way to push state or intent back through its normal transcript.

Operator corrections on the task constrain the design:

- "emoji pairs" means literal adjacent emoji such as `🌶️📋`, not necessarily a
  single ZWJ grapheme;
- ZWJ sequences remain allowed, but they are one marker shape among others;
- text ACK/TASK forms remain canonical aliases;
- implementation must solve deterministic Unicode matching and suppression;
- this task is design-only, with no marker or hook implementation.

## Findings

### Existing Text Protocol

`spice/mail/acks.py` already models text control verbs as assistant-output
markers. It finds standalone `ACK` and `NACK` tokens, parses key-shaped
arguments, splits keyed segments, and extracts inline `TASK` batch lines. The
guard logic suppresses obvious discussion of ACKs: quoted ACK tokens, inline
backtick contexts, negation, hypothetical phrasing, and narration words.

That is the correct seam to generalize. Emoji markers should compile to the
same internal verbs before retirement or task capture happens. They should not
create a parallel ACK state machine.

Current gap: the ACK guard is not a reusable markdown/block scanner. It has an
inline quote/backtick guard, but a marker scanner will also need fenced-code and
blockquote awareness so examples in design records, docs, or reviews cannot
trigger control effects.

### Driver Message Boundaries

`spice/agent/driver.py` and `spice/agent/watchdog.py` make driver-specific
stdout reconstruction explicit. The marker scanner should run after that layer,
when the harness has a complete assistant message, not on raw stdout chunks.

This keeps the protocol independent of whether a driver speaks marker-framed
stdout or JSON events. It also gives one place to validate transcript fidelity:
the reconstructed assistant message must preserve the exact marker sequences
the scanner receives.

### Vendor Hook Experiment

Session tooling for author session `74bebe1f85064551b0cf7f22d3e5ea22` recovered
the Claude Code hook experiment recorded on the task:

- Claude Code `PostToolUse` supports `hookSpecificOutput.additionalContext`.
- A mid-session `.claude/settings.local.json` probe did not fire, indicating
  hook configuration loads at session startup.
- After restart, a `PostToolUse` hook on non-shell `Read` injected an
  `EDGE-PROBE-OK chili=spicy pending=demo3` reminder that reached the model.
- Under the wrapper there was no hook approval prompt.
- A wildcard matcher would fire on every tool call, so it needs the same
  repeat-suppression discipline as stderr steering.

Conclusion: vendor hooks are a proven inbound ambient channel for non-shell
tool spans. They should be used to close "native tool dark stretches" for
steering delivery, while Bash keeps the existing shell reexec/stderr path.
That does not replace the assistant-output marker protocol; it reduces how
often the assistant must ask the harness for state.

### Unicode Normalization

The registry must store a canonical sequence for every marker. Matching should
normalize the candidate text before lookup, but normalization alone is not
enough:

- NFC does not erase all emoji presentation differences.
- `🌶` and `🌶️` may differ by variation selector 16.
- literal pairs such as `🌶️📋` are valid marker sequences even though they are
  multiple grapheme clusters.
- ZWJ sequences may be valid markers but must not be required for every marker.

The implementation should therefore normalize to NFC, then apply an explicit
emoji marker canonicalization policy owned by the registry. A test vector
should name every accepted spelling for each marker and the exact canonical
sequence it maps to.

## Constraints

- Do not make emoji markers required ceremony. They should reduce text, not add
  obligations.
- Do not let emoji aliases bypass ACK/TASK auditability. The transcript must
  still show a durable receipt and task creation intent.
- Do not parse raw tool output streams for protocol markers. Parse assistant
  messages after driver reconstruction.
- Do not make a shell wrapper like `spice <emoji>` the primary surface. Wrappers
  are secondary affordances.
- Do not implement this in the design task. Implementation belongs to follow-up
  tasks.

## Proposed Marker Shape

V1 should reserve a spice glyph plus a verb glyph, followed by the same payload
the text form would use:

```text
🌶️✅ <inbox-key>: received and applied
🌶️📋 title=Add scanner tests | project=lifecycle.protocol | acceptance=...
```

The scanner maps those to:

```text
ACK <inbox-key>: received and applied
TASK title=Add scanner tests | project=lifecycle.protocol | acceptance=...
```

This keeps all downstream semantics unchanged.

## Validation Requirements

Before enabling the protocol:

- unit-test registry normalization for NFC, variation selector 16, ZWJ, and
  literal adjacent emoji pairs;
- unit-test suppression for inline code, fenced code, blockquotes, quoted prose,
  and narration;
- replay Codex and Claude reconstructed assistant messages through the scanner;
- prove the Claude `PostToolUse "*"` inbound hook path uses repeat suppression
  and does not fire unbounded duplicate reminders;
- include examples where markers are discussed in docs without taking effect.

## Follow-Ups

- `PROTOCO-1k9FypbS`: implement the emoji marker alias scanner and registry.
- `PROTOCO-1k9FypYx`: validate emoji marker transcript fidelity across drivers.
