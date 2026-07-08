# Session Transcript Fixtures

These compact JSONL files distill supervised-lane transcript patterns that
session forensics must keep recognizing.

| File | Grammar | Patterns |
| --- | --- | --- |
| `supervised_codex.jsonl` | Codex rollout events | Codex `task_started`/`task_complete` turn boundaries; bare and preamble `$spice` skill-invocation user messages; compaction-summary continuation packets; `<task-notification>` blocks; environment scaffold; human operator prose; `<1kCodex>` steering re-injection represented by assistant `ACK`/`NACK` headers; three compaction boundaries. |
| `supervised_claude.jsonl` | Claude stream-json events | Claude `promptId` turn boundaries; raw `summary` and `system compact_boundary` compaction markers; bare and preamble `$spice` skill-invocation user messages; compaction-summary continuation packets; `<task-notification>` blocks; environment scaffold; human operator prose; `<1kClaude>` steering re-injection represented by assistant `ACK`/`NACK` headers; three compaction boundaries. |
