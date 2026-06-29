# Serve UI Invariants Reference

This file holds the durable implementation map for the root
[UI_INVARIANTS.md](../../UI_INVARIANTS.md) contract.

## Audio And Speech Playback

### Single-owner playback

At most one audio element may sound at any time. The active element is stopped
before any new clip starts.

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

Speak, narrate, and manual play all feed the same `speechQueue`.

- `speechEpoch` is bumped by hard reset events: stop, manual play, and external
  pause. Each queued entry records the epoch it was enqueued under; the drain
  loop abandons stale entries.
- `lane.speechAbortVersion` is lane-scoped. Queued entries capture their lane's
  abort version and are skipped if the lane version has changed.

Enforcement: `spice/serve/static/app.audio.js:13-20,458-499`

Contract: stop is idempotent and clears all pending speech immediately, even if
speech is already draining.

### Intentional vs external pause

Controlled pauses and external pauses have different consequences.

- `intentionallyPaused` marks elements paused by the UI itself. Their `pause`
  events are controlled settle events.
- An unmarked `pause` event is external. It clears the speech queue unless
  narration mode is treating the event as a recoverable interruption.

Enforcement: `spice/serve/static/app.audio.js:27-29,479-482,566-575`

### Narration ordering

Messages are spoken in reverse chronological order within a lane's automatic
speech queue. `queueSpeechForMessages` processes `[...messages].reverse()` to
enqueue utterances.

Enforcement: `spice/serve/static/app.audio.js:45-60`

Cursor tracking: automatic speech cursors are per agent, so page refresh and
reconnect do not replay old messages.

### Transcript remains the record

Playback is best-effort operator attention. The written transcript is the
record, and audio failures must not halt message display.

Enforcement: `spice/serve/static/app.audio.js:1-6`

## Message Stream

### Initial bootstrap waits for server topology

The app's `init()` function must await both `connectLiveBus()` and
`refreshServerTopology()` before starting live updates. This ensures the client
knows the server's team/lane structure before processing messages.

Enforcement: `spice/serve/static/app.js`

Test: `tests/test_servestatic.py:test_static_initial_bootstrap_waits_for_server_topology`

### Fresh-start identity applied before refresh

When a lane's thread ID changes (`ensure.threadId !== previousThreadId`), the
identity is applied through `applyLaneTargetIdentity` before route config or
inventory updates.

Enforcement: `spice/serve/static/app.stream.js`

Related functions:

- `applyLaneSendResult`
- `applyTaskDrainRouteConfig`
- `applyRouteConfigToTargetInventory`

Test: `tests/test_servestatic.py:test_static_send_route_applies_fresh_start_identity_before_refresh`

### Lane status preview requires relative time

A lane status preview is shown only if `statusLine.lastAssistantAt` exists and
can be converted to relative time.

Enforcement: `spice/serve/static/app.render.js` (`setLaneStatus`)

Test: `tests/test_servestatic.py:test_static_lane_status_preview_requires_relative_time`

## Responsiveness

### Narrow viewport affordances

The UI provides scroll snap and mobile-specific lane gaps for viewports at or
below 720px wide. CSS must maintain the max-width 720px rules for narrow-screen
usability.

Enforcement:

- `spice/serve/static/index.css`
- `spice/serve/static/composer.css`

Test: `tests/test_servestatic.py:test_static_css_has_narrow_viewport_affordances`
