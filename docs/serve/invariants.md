# Serve UI Invariants

The `spice serve` UI is frameworkless by design: no build step, no generated
client bundle, and no dependency on a healthy npm tree. These invariants are the
contract for keeping that UI maintainable. Each entry states the durable
contract first, then the implementation site and tests that enforce it.

## Audio And Speech Playback

### Single-owner playback

**At most one audio element may sound at any time.** New playback hard-stops the
active element before starting another clip. Each clip also carries a generation
token, so a late `play()` resolution cannot overlap with a newer clip.

- `playbackGeneration` is bumped on every new playback start. Each audio element
  captures the generation it was spawned under. If a `play()` promise resolves
  after the element becomes stale, that element stops itself immediately.
- `activePlaybackAudio` is the single active reference. Starting a new clip
  calls `stopActivePlayback()` first.
- `stopOrphanedPlayback(audio)` terminates stale clips whose `play()` promise
  resolved late.

Enforcement: `spice/serve/static/app.audio.js:543-586`

Tests:

- `tests/test_servestatic.py:test_audio_playback_enforces_single_owner`
- `tests/test_speechprep.py:test_speech_burst_never_overlaps_audio`

### Speech queue and epoch abandonment

**One global speech queue owns speak, narrate, and manual playback.** Hard reset
events bump a speech epoch; lane-scoped aborts bump the lane abort version.
Queued work whose epoch or lane version is stale is abandoned before playback.
Stop is therefore immediate, idempotent, and global.

- `speechEpoch` is bumped by hard reset events: stop, manual play, and external
  pause. Each queued entry records the epoch it was enqueued under; the drain
  loop abandons stale entries.
- `lane.speechAbortVersion` is lane-scoped. Queued entries capture their lane's
  abort version and are skipped if the lane version has changed.

Stop clears all pending speech immediately, even while speech is already
draining.

Enforcement: `spice/serve/static/app.audio.js:13-20,458-499`

### Intentional vs external pause

**The UI distinguishes deliberate pauses from external pauses.** Pauses caused
by supersession or operator stop are marked as intentional. An unmarked pause
from OS media controls or lock-screen behavior clears queued speech unless
narration mode is intentionally recovering from the interruption.

- `intentionallyPaused` marks elements paused by the UI itself. Their `pause`
  events are controlled settle events.
- An unmarked `pause` event is external. It clears the speech queue unless
  narration mode is treating the event as a recoverable interruption.

Enforcement: `spice/serve/static/app.audio.js:27-29,479-482,566-575`

### Narration ordering

**Automatic narration speaks newest messages first within a lane.** Per-agent
speech cursors prevent old messages from being re-spoken after refresh or
reconnect.

`queueSpeechForMessages` processes `[...messages].reverse()` to enqueue
utterances. Automatic speech cursors are per agent, so page refresh and
reconnect do not replay old messages.

Enforcement: `spice/serve/static/app.audio.js:45-60`

### Transcript remains the record

**The transcript is authoritative.** Audio is best-effort playback for operator
attention; failures degrade silently and must never block the visible stream.

Enforcement: `spice/serve/static/app.audio.js:1-6`

## Message Stream

### Bootstrap waits for server topology

**Bootstrap waits for server topology.** The app must connect the live bus and
refresh server topology before it starts processing live updates. The client
needs the current team/lane structure before rendering session events.

The app's `init()` function must await both `connectLiveBus()` and
`refreshServerTopology()` before starting live updates.

Enforcement: `spice/serve/static/app.js`

Test: `tests/test_servestatic.py:test_static_initial_bootstrap_waits_for_server_topology`

### Fresh-start identity applied before refresh

**Fresh-start identity lands before refresh.** When a lane changes thread ID,
the new target identity must be applied before route config or inventory
updates. A renewed session must not inherit stale identity from the previous
thread.

When a lane's thread ID changes (`ensure.threadId !== previousThreadId`), the
identity is applied through `applyLaneTargetIdentity` before route config or
inventory updates.

Related functions:

- `applyLaneSendResult`
- `applyTaskDrainRouteConfig`
- `applyRouteConfigToTargetInventory`

Enforcement: `spice/serve/static/app.stream.js`

Test: `tests/test_servestatic.py:test_static_send_route_applies_fresh_start_identity_before_refresh`

### Lane status preview requires relative time

**Lane status previews require relative time.** A preview is shown only when the
server provides `statusLine.lastAssistantAt` and the client can render it as
relative time. Missing or unrenderable timestamps produce no preview.

Enforcement: `spice/serve/static/app.render.js` (`setLaneStatus`)

Test: `tests/test_servestatic.py:test_static_lane_status_preview_requires_relative_time`

## Responsiveness

### Narrow viewport affordances

**Narrow viewports keep lane navigation usable.** CSS must preserve the
max-width 720px affordances that add scroll snap behavior and mobile-specific
lane gaps.

Enforcement:

- `spice/serve/static/index.css`
- `spice/serve/static/composer.css`

Test: `tests/test_servestatic.py:test_static_css_has_narrow_viewport_affordances`

## Operating Rule

State each contract in prose a maintainer must preserve, then keep its
enforcement note — code location, test names, browser-race explanation —
directly beneath it. When serve UI code moves, update the enforcement note in
the same change so the contract and its proof stay together.
