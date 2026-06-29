# UI Invariants

The `spice serve` UI is frameworkless by design: no build step, no generated
client bundle, and no dependency on a healthy npm tree. These invariants are the
root contract for keeping that UI maintainable. The implementation-level
enforcement map lives in [docs/serve/ui-invariants.md](docs/serve/ui-invariants.md).

## Audio And Speech Playback

**At most one audio element may sound at any time.** New playback hard-stops the
active element before starting another clip. Each clip also carries a generation
token, so a late `play()` resolution cannot overlap with a newer clip.

**One global speech queue owns speak, narrate, and manual playback.** Hard reset
events bump a speech epoch; lane-scoped aborts bump the lane abort version.
Queued work whose epoch or lane version is stale is abandoned before playback.
Stop is therefore immediate, idempotent, and global.

**The UI distinguishes deliberate pauses from external pauses.** Pauses caused
by supersession or operator stop are marked as intentional. An unmarked pause
from OS media controls or lock-screen behavior clears queued speech unless
narration mode is intentionally recovering from the interruption.

**Automatic narration speaks newest messages first within a lane.** Per-agent
speech cursors prevent old messages from being re-spoken after refresh or
reconnect.

**The transcript is authoritative.** Audio is best-effort playback for operator
attention; failures degrade silently and must never block the visible stream.

## Message Stream

**Bootstrap waits for server topology.** The app must connect the live bus and
refresh server topology before it starts processing live updates. The client
needs the current team/lane structure before rendering session events.

**Fresh-start identity lands before refresh.** When a lane changes thread ID,
the new target identity must be applied before route config or inventory
updates. A renewed session must not inherit stale identity from the previous
thread.

**Lane status previews require relative time.** A preview is shown only when the
server provides `statusLine.lastAssistantAt` and the client can render it as
relative time. Missing or unrenderable timestamps produce no preview.

## Responsiveness

**Narrow viewports keep lane navigation usable.** CSS must preserve the
max-width 720px affordances that add scroll snap behavior and mobile-specific
lane gaps.

## Operating Rule

Root invariants should stay short enough to ride in context. Add detailed code
locations, test names, or browser-race explanations to the serve reference; keep
this file focused on contracts a maintainer must preserve.
