# Stability

spice is still moving, but not every surface moves at the same speed. Treat this
page as the adoption map: build on the stable rows, wrap or pin the moving rows,
and assume anything unlisted is internal unless another document says otherwise.

| Surface | Status | Build-on guidance |
| --- | --- | --- |
| Inbox file format and ACK protocol | Stable | Durable inbox items, UTC keys, priority/note fields, pending readout, and transcript `ACK <key>: ...` retirement are core protocol. Adopters can build tooling that writes operator steering and watches semantic ACKs. |
| Constitution constants and hook policy | Stable | Policy limits, flex/sticky behavior, repo-shape checks, env-literal inventory, magic-number ratchet, and commit-message rules are executable doctrine. Changes should be explicit contract changes, not silent drift. |
| Public library seam | Stable | Modules documented in the README library seam (`spice.errors`, `spice.policy`, `spice.paths`, `spice.procs`, `spice.repocfg`, and named study helpers) are intended for repo tools. Underscored names remain private. |
| Agent bootstrap contract | Stable | Worktree skill invocation, `spice agent activation`, `spice session briefing`, and task-board rehydration are the supported prompt-boundary path. |
| Release commands | Stable enough | `spice release prepare`, `notes`, `publish`, and `github` are operator-facing commands. Minor output changes are possible, but the workflow contract should remain intact. |
| Task allocator CLI | Settling | Handles, phases, claims, review flow, and `spice task next` are real operating surfaces. Script against command output cautiously; prefer the CLI over direct Taskwarrior storage. |
| Session forensics | Settling | `spice session briefing`, `phases`, and `messages` are supported for agent rehydration and review. Deeper analytics families may still be renamed or split. |
| Serve lane UI and live bus | In motion | Lane rendering, WebSocket message shapes, browser payload details, and task-drain refresh behavior are active product surfaces, not stable extension APIs. Use them through `spice serve` rather than depending on wire details. |
| Team API and store schema | In motion | Fused teams, lane membership, revisions, metric attribution, and renewal lineage are still being shaped. Expect schema and command changes. |
| Static browser modules | Internal | `spice/serve/static/app.*.js` files are frameworkless implementation modules. Tests document invariants, but module boundaries are not public APIs. |
| Supervisor internals | Internal | Watchdog, side-channel, lifecycle state files, and process supervision details may change as long as the public agent/session/task contracts remain true. |

When in doubt, prefer commands and documented library seams over internal files.
Stable means compatibility matters; in motion means the idea is real, but the
shape is still allowed to improve.
