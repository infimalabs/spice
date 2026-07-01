# Task Markdown DAG

Status: initial contract, 2026-07-01.

The task board remains authoritative. Markdown is a projection: useful as an
input dialect and as an export ledger, but never the source of truth after rows
exist in Taskwarrior.

## Commands

- `spice task ledger HANDLE` exports `HANDLE` and its dependency closure to
  canonical markdown.
- `spice task ingest PATH --project task.example` imports canonical markdown, or
  normalizes freeform markdown into task rows when no canonical block exists.

Both verbs are intentionally single words. `ledger` is read-only; `ingest`
creates rows, notes, and native dependency edges.

## Canonical Form

The canonical markdown document is a fenced JSON block tagged
`spice.task-dag.v1`. The exporter defines the schema. V1 records the creation
surface of a task DAG: node id, title, project, priority, flow, acceptance,
description, annotations, and `after` dependency ids.

Runtime state such as claim owner, review metadata, validation, and completion
status is excluded from v1. Those fields belong to live board state and can be
added to a later ledger form without changing the creation/import contract.

`ingest` annotates created tasks with `markdown-id: <id>`. A later `ledger`
export uses that annotation as the node id, which gives canonical imports a
stable projection identity even though Taskwarrior handles are minted at import
time.

## Freeform Dialect

| Markdown construct | V1 mapping |
| --- | --- |
| Heading | Task node. Lower-level headings become child nodes. |
| Bullet or numbered list item | Task node. Nested items become child nodes. |
| Parent/child relation | Parent task depends on each child task. |
| `Acceptance: ...` line | Acceptance item on the current node. |
| Plain paragraph text | Description text on the current node. |
| Code fence | Annotation on the current node. |
| Blockquote | Annotation on the current node. |
| Table row | Annotation on the current node. |
| Link line | Annotation on the current node. |
| Multiple top-level roots | Synthetic `Markdown import` root depending on roots. |

Unsupported markdown stays text or annotation content. HTML blocks, reference
definitions, footnotes, and rich table semantics are not interpreted in v1.

## Incremental Plan

1. Land `ledger` and `ingest` with canonical fenced JSON, freeform headings and
   lists, annotations, and dependency edges.
2. Add serve-side affordances for exporting a task closure or a time slab once
   the CLI contract has review mileage.
3. Join ACKed steering into deterministic per-node decision sections using task
   context windows and handle references.
4. Extend canonical v2 only after v1 round-trip examples show which runtime
   fields are worth exporting.
