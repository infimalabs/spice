# Single-Operator Remote Reach

Status: implemented contract, 2026-07-18.

## Implemented Contract

Remote reach for one operator is a **transport** choice over the serve endpoint
that already exists, not a new addressing model and not a new auth model. In one
line: **reach the co-located serve over SSH and/or a tailnet, present the one
shared token, and add no multi-user auth.**

- **Transport — SSH and/or tailnet, both operator-owned.**
  - *SSH port-forward (default).* Serve stays on loopback (`--host 127.0.0.1`,
    the default); the operator forwards a port
    (`ssh -L 8080:127.0.0.1:8765 host`). No exposed bind, SSH keys are the
    transport auth, the shared token is unchanged defense-in-depth. Works today,
    no code change.
  - *Tailnet bind (always-on).* Serve binds its tailnet address with
    `--auth-token`; the mesh's device enrollment and ACLs gate who connects.
    Already permitted by serve's exposed-bind guard (exposed bind + token).
    Works today, no code change.
- **Addressing — unchanged, local.** A worktree target is keyed on a local
  `repo_root` the serve process reads directly — git `HEAD`, filewatch, skills,
  link roots (`spice/serve/worktree/target.py`) — and serve is co-located with
  its worktrees (`docs/design/accepted/single-install-runtime-model.md`). Remote
  reach addresses the **serve endpoint** (`host:port`); the operator picks
  worktrees with the existing `id`/`branch`/`name`/`repo_root` selectors once
  connected. Targets are never taught to point at another machine.
- **Token — one shared token, presented by the remote client.** Configured once
  on the serve host via `--auth-token`; the remote browser or CLI presents it
  over the encrypted transport (SSH tunnel or tailnet link), never cleartext.
  One operator, one token — there is no token-distribution protocol to build.

**Multi-user auth is explicitly out of scope.** This posture is single-operator
by decision: no per-identity tokens, sessions, or OIDC. The multi-human question
is answered separately in `docs/design/accepted/no-privileged-channel-multi-human.md`;
any such controls are front-door additions, not part of this model.

## Context

`studies.posture` / POSTURE-1kCXTyHP asks how one human reaches their own agent
fleet remotely without becoming multi-tenant. The transport works with today's
Serve flags; only dedicated first-class packaging is unbuilt. This record fixes
the direction so future packaging does not invent transport or auth on the fly.
The posture umbrella (POSTURE-1kCXR4K7) is complete, and this is its remote-reach
growth vector.

## Evaluation

Serve already has the primitives this needs: `--host`/`--port`, a single
`--auth-token`, and an exposed-bind guard that requires the token (or an explicit
`--allow-insecure-bind`) for any non-loopback bind (`spice/serve/app.py`:
`_guard_exposed_bind`, `_warn_exposed_bind`). Worktree targets are inherently
local — `discover_serve_worktrees` resolves each `repo_root` and reads it
directly for git HEAD, file watching, skills, and links
(`spice/serve/worktree/target.py`). So the only thing that must cross the network
is the operator's connection to serve, which makes remote reach a transport
question rather than an addressing or auth-model question.

Making `repo_root` a remote address is rejected: it would proxy every local read
across the network for no posture benefit and fights the single-install runtime
model, which fixes serve as one deployment co-located with its operated
worktrees. SSH forwarding keeps serve on loopback (smallest surface, right
default for "reach my own box"); a tailnet bind trades a token-guarded exposed
bind for always-on multi-device reach, letting the mesh do the device gating
spice deliberately does not. Both keep exactly one shared token.

## Constraints / Non-Goals

- No multi-user auth, per-identity tokens, sessions, or OIDC. One operator, one
  shared token.
- No change to worktree target addressing; no remote-worktree filesystem proxy.
- No hosted relay or rendezvous service; both transports are operator-owned
  (their SSH host, their tailnet).
- Transports are transport-layer front-door controls; they do not add a second
  steering channel (consistent with the no-privileged-channel axiom).

## Examples

```
# SSH (default): serve stays on loopback
spice serve --auth-token "$SPICE_SERVE_TOKEN" # 127.0.0.1:8765
ssh -L 8080:127.0.0.1:8765 serve-host         # from the remote device
# browse http://127.0.0.1:8080, present the shared token

# Tailnet (always-on): exposed bind guarded by the token
spice serve --host <tailnet-addr> --auth-token "$SPICE_SERVE_TOKEN"
# browse http://<tailnet-addr>:8765 from any enrolled device, same token
```

## Follow-Ups

None required. Both transports work against today's serve with no code change,
so this record spawns no implementation task. Dedicated packaging may be added
when it removes observed operator friction. If worktrees ever genuinely cannot
be co-located with serve, reopen remote target addressing against that concrete
need, not speculatively.
