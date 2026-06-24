# Mental Model

spice is easiest to understand as an operating loop around coding agents, not
as an editor or an agent framework. The transcript is the durable record of
what the agent did and understood. The filesystem is the durable channel for
steering. Everything else exists to keep those two surfaces converging.

The operator does not try to write a perfect spec up front. They watch the
agent work, steer the exact thing that drifted, and let the corrected transcript
become the next piece of intent. A good run ends when observed behavior stops
provoking corrections. The spec is the fixed point the loop reaches, not the
document it starts from.

Lanes make this operational. A lane is the operator's visible container for a
worktree and its current agent occupant. The agent can be renewed, replaced, or
moved, while the lane keeps the readable stream and controls. Tasks decide what
work an agent should take next; the allocator, not the agent's memory, owns
selection.

The conscience and constitution reduce the operator's repetitive work. Maxims
turn curated taste into live steering when assistant prose drifts. The
constitution turns repo hygiene into executable gates. Neither is magic: they
work because the rules are narrow, observable, and cheap to correct when they
fire imperfectly.

## Glossary

- **Lanes**: Operator-owned UI containers over worktree targets. A lane holds
  controls, transcript stream, filters, metrics, and the current agent
  occupant.
- **Drive**: A lane lifetime where task filters are managed from projects this
  team creates or claims, so the lane can keep working its intended stream.
- **Drain**: A lane lifetime where the task boundary is dissolved and the lane
  can see all assignable work instead of only stored filters.
- **drain-boundary-dissolved**: The Drain invariant: project boundaries no
  longer constrain allocator visibility for that lane.
- **Renew**: Graceful agent succession. The current agent is asked to reach a
  clean handoff, and a successor resumes the lane from transcript and task
  state.
- **Maxims**: Curated, near-universal operator preferences judged against
  assistant prose and returned as ordinary steering when violated.
- **Constitution**: The executable repo policy enforced by hooks and studies:
  path shape, file shape, complexity, magic numbers, env literals, and commit
  rules.
- **flex/sticky**: Gate pressure model. A limit may flex temporarily, but once a
  file or routine breaches the flex ceiling, the stricter base limit sticks
  until it shrinks back under control.
