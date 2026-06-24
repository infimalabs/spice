# Semantic ACK Standalone Protocol

Status: decision recorded.

## Decision

Publish the semantic-ACK loop as a small standalone draft protocol, but do not
present it as a broad agent interoperability standard yet.

The useful artifact is a narrow, harness-independent "Semantic ACK" protocol:
a producer creates a durable directive with an unguessable key; an agent retires
that directive only by writing the key and a short description of the change in
its durable transcript; the watcher treats unknown keys as explicit no-ops and
keeps redisplaying unretired directives.

That is worth publishing because it solves one problem cleanly: reliable
operator-to-agent steering with auditable semantic closure. It should not
compete with MCP or A2A. MCP standardizes application-to-tool/context
integration, including tools, resources, prompts, sampling, roots, elicitation,
progress, cancellation, and error reporting
(https://modelcontextprotocol.io/specification/2025-06-18). A2A standardizes
agent-to-agent communication, discovery, task collaboration, streaming, and
opaque-agent interoperability (https://github.com/a2aproject/A2A). Semantic ACK
is smaller: it is the receipt layer for instructions whose correctness depends
on the agent saying what it understood and did.

## Rationale

The core idea is portable outside spice. Any harness with a durable message
queue and a transcript can implement it:

1. Write each directive as a durable record with an unguessable key.
2. Deliver pending directives repeatedly until retired.
3. Retire only when the agent emits `ACK <key>: <summary>` in transcript prose.
4. Archive the directive, key, transcript message, and summary together.
5. Treat nonexistent keys as "retired nothing", not as success.

This creates an at-least-once control channel without pretending that "the model
saw the text" means "the model acted on it." Reading is not delivery. Tool-call
success is not delivery. A syntactic key match alone is not ideal delivery
either; the summary makes the closure semantic enough for a human or later
checker to audit.

The protocol also gives useful failure modes:

- ignored directive: it remains pending and is shown again;
- stale or hallucinated key: the system says it retired nothing;
- duplicate resend: a fresh key forces a fresh closure;
- ambiguous closure: the summary can be reviewed against the directive.

Those properties are the part likely to transfer to other agent harnesses,
chatops systems, IDE agents, CI remediation bots, and long-running coding lanes.

## Protocol Sketch

Names are intentionally generic so another system can adopt the pattern without
copying spice internals.

Directive:

```json
{
  "key": "20260624T034259415973Z",
  "body": "Fix the failing route test.",
  "priority": "normal",
  "note": "Optional routing or operator metadata.",
  "created_at": "2026-06-24T03:42:59.415973Z"
}
```

Agent acknowledgment:

```text
ACK 20260624T034259415973Z: fixed the route matcher and verified the focused test.
```

Required receiver behavior:

- Keys must be hard to guess and unique within the directive store.
- A directive remains pending after being read.
- A directive retires only through a transcript-visible ACK line.
- An ACK must name at least one key and include a human-readable disposition.
- Unknown keys produce explicit "retired nothing" feedback.
- The archive records directive body, key, ACK text, and transcript location.

Recommended behavior:

- Accept low-risk aliases for copied keys only when they remain unambiguous.
- Re-display pending directives on a cadence or at command boundaries.
- Escalate long-unretired directives by creating a fresh key rather than
  mutating the old directive.
- Keep task-capture side effects separate from ACK retirement.

## Non-goals

Do not standardize transport, storage, auth, task schemas, UI rendering, or
agent discovery in this draft. Those are already covered elsewhere or are too
harness-specific. The standalone protocol should be small enough to implement
over files, a database table, a queue, Slack messages, or an IDE extension.

Do not require an LLM judge for semantic validation. A judge can strengthen the
archive later, but the first interoperable unit is the key-plus-summary receipt.

## Publication Path

1. Write `docs/protocols/semantic-ack.md` as an Internet-Draft-style markdown
   note with terminology, state machine, examples, and security considerations.
2. Include a reference implementation over a directory of directive files and a
   transcript text file.
3. Mark it Draft until a second implementation outside spice exists.
4. Revisit broader standardization only after another harness adopts the loop
   and reports whether the fields are sufficient.

The answer is therefore yes, publish it, but as a deliberately small draft. Its
value is in naming a missing control-plane invariant: a directive is complete
only when the agent leaves an auditable semantic receipt in the transcript.
