# UI Invariants

The spice UI is frameworkless by design — no build step means it cannot rot from a broken npm tree. These invariants define the rules the UI must maintain. They are enforced by the test suite (`tests/test_serve*.py`) and represent the contracts a future maintainer must preserve.

## Audio and Speech Playback

### Single-owner playback (generation-gated)
**At most one audio element may sound at any time.** The active element is hard-stopped before any new clip starts.

- **Generation token** (`playbackGeneration`): Bumped on every new playback start. Each audio element captures the generation it was spawned under. If a `play()` promise resolves but the element's generation is stale, it stops itself immediately — ensuring two clips never overlap even if `play()` resolves late.
- **Active reference** (`activePlaybackAudio`): Only one element may hold this reference. Starting a new clip calls `stopActivePlayback()` first.
- **Orphaned playback cleanup**: A late-resolving `play()` that discovers its generation is stale calls `stopOrphanedPlayback(audio)` to terminate itself.

**Enforcement**: `spice/serve/static/app.audio.js:543-586`  
**Test coverage**: `tests/test_servestatic.py:test_audio_playback_enforces_single_owner`, `tests/test_speechprep.py:test_speech_burst_never_overlaps_audio`

### Speech queue and epoch abandonment
**One global sequential queue across all lanes.** Speak/narrate/manual-play all feed the same `speechQueue`.

- **Epoch versioning** (`speechEpoch`): Bumped by every hard reset (stop, manual play, external pause). Each queued entry records the epoch it was enqueued under; the drain loop abandons any entry whose epoch is stale. A single stop clears the whole pipeline regardless of which lane originated the speech.
- **Per-lane abort versioning** (`lane.speechAbortVersion`): Each lane tracks its own abort version. Queued entries capture this version at enqueue time; the drain skips entries whose abort version is stale relative to their lane.

**Enforcement**: `spice/serve/static/app.audio.js:13-20,458-499`  
**Contract**: Stop is idempotent and clears all pending speech immediately, even if already draining.

### Intentional vs external pause
The UI distinguishes controlled pauses (supersession, deliberate stop) from external pauses (OS media keys, lock screen).

- **Intentional pause marker** (`intentionallyPaused` WeakSet): Elements we pause deliberately are marked. Their `pause` event is a controlled settle.
- **External pause behavior**: An unmarked `pause` event (external stop) clears the entire speech queue unless narration mode is active, in which case it's treated as a recoverable interruption.

**Enforcement**: `spice/serve/static/app.audio.js:27-29,479-482,566-575`

### Narration ordering
Messages are spoken in reverse chronological order (newest first) within a lane's automatic speech queue. The `queueSpeechForMessages` function processes `[...messages].reverse()` to enqueue utterances.

**Enforcement**: `spice/serve/static/app.audio.js:45-60`  
**Cursor tracking**: Automatic speech cursors (per agent) prevent re-speaking old messages on page refresh or reconnect.

### Transcript remains the record
**Playback is best-effort ear candy and never blocks the stream.** The written transcript is authoritative; audio failures degrade silently rather than halting message display.

**Enforcement**: `spice/serve/static/app.audio.js:1-6` (opening comment)

## Message Stream

### Initial bootstrap waits for server topology
The app's `init()` function must await both `connectLiveBus()` and `refreshServerTopology()` before starting live updates. This ensures the client knows the server's team/lane structure before processing messages.

**Enforcement**: `spice/serve/static/app.js` (static contract tested in `test_servestatic.py:test_static_initial_bootstrap_waits_for_server_topology`)

### Fresh-start identity applied before refresh
When a lane's thread ID changes (`ensure.threadId !== previousThreadId`), the identity is applied via `applyLaneTargetIdentity` before any route config or inventory updates. This prevents stale identities from leaking into a new session.

**Enforcement**: `spice/serve/static/app.stream.js` (`applyLaneSendResult`, `applyTaskDrainRouteConfig`, `applyRouteConfigToTargetInventory`)  
**Test coverage**: `test_servestatic.py:test_static_send_route_applies_fresh_start_identity_before_refresh`

### Lane status preview requires relative time
A lane's status preview is only shown if `statusLine.lastAssistantAt` exists and can be converted to relative time (e.g., "2m ago"). No timestamp means no preview.

**Enforcement**: `spice/serve/static/app.render.js` (`setLaneStatus`)  
**Test coverage**: `test_servestatic.py:test_static_lane_status_preview_requires_relative_time`

## UI Responsiveness

### Narrow viewport affordances
The UI provides scroll-snap and mobile-specific lane gaps for viewports ≤720px wide. CSS must maintain `@media (max-width: 720px)` rules for narrow-screen usability.

**Enforcement**: `spice/serve/static/index.css`, `spice/serve/static/composer.css`  
**Test coverage**: `test_servestatic.py:test_static_css_has_narrow_viewport_affordances`

## Rationale

These invariants exist to make the frameworkless UI maintainable **without requiring the original author**. The test suite encodes many of these rules; this document explains the *why* behind them so a second person can confidently change the implementation without breaking the contracts.

**Generation-gated playback** ensures no audio overlap even with racy browser `play()` promises. **Epoch abandonment** ensures stop is instant and total. **Intentional vs external pause** preserves mobile background/lock behavior without breaking desktop stop. **Narration ordering** keeps speech chronologically coherent.

The UI is built to survive indefinitely with zero external dependencies. These invariants are the load-bearing rules that make that survivability real.
