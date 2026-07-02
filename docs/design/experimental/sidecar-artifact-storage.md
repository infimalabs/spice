# Sidecar Artifact Storage

Status: recommendation, 2026-06-26.

## Recommendation

Add a task-addressed sidecar artifact store for large non-code outputs that
should be durable and reviewable without becoming source-tree changes.

Implement a small production command surface:

```sh
spice task artifact add <task> <path> [--name NAME] [--type CONTENT_TYPE]
spice task artifact list <task>
spice task artifact show <task> <artifact-id>
spice task artifact prune [--older-than DURATION]
```

Do not add new task phases for plans, research, prototypes, or long reviews.
Artifacts belong to tasks; tasks remain the allocator and review unit.

## Context

`docs/design/experimental/top-level-non-code-phases.md` identifies a missing
artifact space: some useful outputs are too large for task notes but should not
alter the worktree. Examples include raw research notes, trial logs, benchmark
output, screenshots, rejected plans, review reports, and prototype evidence.

Spice already has a precedent for durable out-of-tree blobs through inbox
attachments. Those live under the shared git common dir, are content-addressed,
and render back as links in steering readouts. Task sidecars should use the same
broad storage principle but a different address: task handle first, content
digest second.

## Storage Layout

Store artifacts under the shared git common dir so all worktrees for one
repository see the same sidecars:

```text
<git-common-dir>/spice/artifacts/tasks/<TASK-HANDLE>/
  manifest.json
  objects/
    <sha256>/
      metadata.json
      payload
```

`manifest.json` is the task-local index. It contains ordered entries:

```json
{
  "version": 1,
  "task": "PHASES-20260626T055920182642Z",
  "artifacts": [
    {
      "id": "A1",
      "name": "research-notes.md",
      "sha256": "...",
      "content_type": "text/markdown",
      "size": 12345,
      "created_at": "2026-06-26T06:00:00Z",
      "source": "spice task artifact add",
      "summary": "Raw notes from option survey"
    }
  ]
}
```

Objects are content-addressed by SHA-256 to deduplicate repeated files and make
integrity checks cheap. The task manifest owns display order, names, summaries,
and review-facing ids. Never store artifacts under `.spice/` in the worktree;
that would make sidecars disappear or conflict across worktrees.

## Task Show / Render Integration

`spice task show <handle>` should append an `artifacts:` block when a manifest
exists:

```text
artifacts:
  A1 research-notes.md text/markdown 12.1 KiB
     spice task artifact show PHASES-... A1
```

The block should be compact by default: id, name, content type, size, and the
show command. Full artifact content belongs behind `spice task artifact show`,
not in every task packet.

Serve and any future task renderers should expose the same manifest summary.
Binary artifacts should render as links; text artifacts may expose a short
preview if they are below the inline preview limit.

## Review Citation Behavior

Review notes should cite sidecars by stable task artifact id:

```text
See artifact A2 on PHASES-20260626T055920182642Z for the raw benchmark log.
```

`spice task review` should not copy large artifact content into task annotations.
It should accept citations in prose and rely on `task show` / `artifact show`
to resolve them. A later implementation can add citation validation, but the
first version only needs stable ids and render visibility.

## Retention

Retention should be explicit, not implicit cleanup of "old-looking" files.

- Artifacts attached to pending or waiting tasks are retained.
- Artifacts attached to completed tasks are retained by default.
- `spice task artifact prune --older-than <duration>` may remove artifacts for
  completed tasks when their manifest marks them `retention: prunable`.
- Artifacts marked `retention: permanent` are never pruned by the default
  command.
- Deleting a task should not delete artifacts automatically; deletion should
  mark the manifest orphaned so a later prune can report it.

This is conservative. Design and review artifacts are often only useful much
later, when someone asks why a decision was made.

## Size And Type Limits

Initial limits should be deliberately small:

- max artifact size: 16 MiB;
- max artifacts per task: 32;
- max filename length: 96 characters after sanitization;
- allowed text types: `text/plain`, `text/markdown`, `application/json`,
  `text/csv`, and `text/tab-separated-values`;
- allowed binary types: `image/png`, `image/jpeg`, `image/webp`, and
  `application/pdf`.

Reject unknown binary types in the first version. Allow a future task to add
explicit waivers or larger limits after the UI and retention paths are proven.

## Command Surface

Implement the command surface in production, not as a docs-only convention.
Without a command, agents will invent paths and reviewers will not know where
to look.

Minimum behavior:

- `add` copies an existing file into the content-addressed object store, updates
  the manifest atomically, and prints the artifact id.
- `list` prints the compact manifest rows.
- `show` prints text artifacts to stdout and prints a path for binary artifacts.
- `prune` performs a dry run by default unless passed `--apply`.

The command should require a valid task handle and should not claim or complete
tasks. Artifact creation is supporting evidence, not allocator selection.

## Non-Goals

- Do not stream arbitrary command stdout directly into the store in the first
  version.
- Do not make sidecar artifacts part of git commits.
- Do not add new task phases.
- Do not put large artifact bodies into task annotations or serve lane payloads.
- Do not build a general file manager before the task-addressed path exists.

## Follow-Ups

- `ARTIFAC-20260626T060419124312Z`: implement the `spice task artifact` storage
  and CLI surface, including task show integration, review citation visibility,
  retention flags, and focused tests.
