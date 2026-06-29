# Interface

`spice serve` is the operator interface for the loop. It composes agents into
Drive lanes, splits worktrees into parallel lanes, routes by task filter, shows
live transcript attachments, and exposes the controls needed to steer or audit a
running session.

The serve header and browser title default to `[project].name` from
`pyproject.toml`; set `[tool.spice.serve] brand = "Name"` to override them.

Binding `spice serve` to `0.0.0.0` or another wildcard address intentionally
degrades the WebSocket Origin guard: any Host on the bound port is compatible,
so the check becomes Origin-equals-Host rather than the rebinding-resistant
authority match used for loopback or explicit binds. Use `--auth-token`; on
wildcard binds the supplied token, not the Origin authority match, is the
operative defense.

## Lanes And Teams

The UI model is the lane: an operator-owned container over a worktree target.
Agents are occupants, so renewal can hand the lane to a new thread while the
message stream remains readable. Lanes can run independently or compose into a
team-backed Drive lane that presents multiple agents behind one operator
surface.

Task filters route board stems to lanes. Lane metrics are projections over the
current membership; per-agent counters remain the source of truth so work
follows the agent, not the team. Steering, ACKs, labels, transcript controls,
attachments, and diagnostics stay visible inside the live stream.

| Compose and route | Parallel lanes |
| --- | --- |
| <img src="screenshots/spice-compose-team-drive.png" alt="Composed Drive lane with three agents"> | <img src="screenshots/spice-three-agent-drive-controls.png" alt="Three Drive lanes across active worktrees"> |
| <sub>A composed Drive lane groups multiple worktree-bound agents behind one operator control surface.</sub> | <sub>Separate lanes keep concurrent work readable while preserving per-agent Drive and speak controls.</sub> |

| Lane controls | Steering and ACKs |
| --- | --- |
| <img src="screenshots/spice-interface-routing-controls.png" alt="Interface routing controls with filters, metrics, info, and assignment chips"> | <img src="screenshots/spice-live-review-steering.png" alt="Live interface showing steering and ACK flow"> |
| <sub>Filters, metrics, info, and worktree assignment live in the lane header.</sub> | <sub>Operator steering, ACKs, labels, and transcript controls stay visible in the live stream.</sub> |

| Attachments in transcript | Live image evidence |
| --- | --- |
| <img src="screenshots/spice-filters-attachment-gallery.png" alt="Filters and attachment gallery"> | <img src="screenshots/spice-live-attachments-multilane.png" alt="Multi-lane interface with live image attachments"> |
| <sub>Transcript attachments remain browsable inside the lane.</sub> | <sub>Screenshots, browser captures, and diagnostics stay part of the operating record.</sub> |

## Lifetime Modes

Every operator send carries a lane lifetime:

- **Steer** keeps the lane manually routed.
- **Drive** auto-subscribes to task projects the team creates or claims.
- **Drain** dissolves the task boundary so all assignable work is visible.

Tracked defaults live in `[tool.spice.serve] default_lifetime`; see
[../CONFIG.md](../CONFIG.md).
