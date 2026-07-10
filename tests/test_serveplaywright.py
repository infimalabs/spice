from pathlib import Path

ROOT = Path(__file__).resolve().parent


def test_serve_playwright_harness_starts_short_lived_scratch_server() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert 'require("playwright")' in harness
    assert 'require("fs")' in harness
    assert "fs.mkdtemp" in harness
    assert "repoLocalServeCommand" in harness
    assert "defaultServeCommand()" in harness
    assert '"--port"' in harness
    assert "String(options.port ?? 0)" in harness
    assert '"--until"' in harness
    assert '"--task-backend"' in harness
    assert "backendDir" in harness
    assert "stopFile" in harness
    assert "waitForProcessExit" in harness


def test_serve_playwright_harness_loads_shared_agent_context() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert "defaultPlaywrightConfigPath" in harness
    assert '".spice"' in harness
    assert '"agent"' in harness
    assert '"playwright-mcp.json"' in harness
    assert "readSharedPlaywrightContextOptions" in harness
    assert "missing shared Playwright config" in harness
    assert "config.browser.contextOptions" in harness
    assert "must define browser.contextOptions" in harness
    assert "serveBrowserContextOptions" in harness
    assert "await serveBrowserContextOptions(options)" in harness
    assert "browser.newContext(options.contextOptions || {})" not in harness


def test_serve_playwright_harness_cleans_up_when_context_creation_fails() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert "let browser = null;" in harness
    assert "browser = await chromium.launch" in harness
    assert "await serveBrowserContextOptions(options)" in harness
    assert "if (browser) await browser.close().catch(() => {});" in harness
    assert "await server.stop();" in harness
    assert harness.index("let browser = null;") < harness.index(
        "await serveBrowserContextOptions(options)"
    )
    assert harness.index("await serveBrowserContextOptions(options)") < harness.index(
        "finally"
    )


def test_serve_playwright_harness_rejects_per_smoke_color_scheme() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert "rejectColorSchemeOverride" in harness
    assert 'hasOwnProperty.call(contextOptions, "colorScheme")' in harness
    assert "inherit colorScheme" in harness
    assert "shared agent Playwright config" in harness


def test_serve_playwright_harness_captures_browser_errors() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert 'page.on("console"' in harness
    assert 'page.on("pageerror"' in harness
    assert "assertNoBrowserErrors(browserErrors)" in harness


def test_serve_menu_smoke_uses_harness_for_interaction() -> None:
    smoke = (ROOT / "browser" / "serve_menu_smoke.js").read_text(encoding="utf-8")

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert ".spice-menu-button" in smoke
    assert ".spice-context-menu .spice-menu-action" in smoke
    assert "Fast mode" in smoke
    assert "fastModeDetail" in smoke
    assert "fastModeReloadedDetail" in smoke
    assert "Fast mode did not survive reload" in smoke
    assert "New team" not in smoke


def test_serve_lane_reload_smoke_asserts_server_shell_persistence() -> None:
    smoke = (ROOT / "browser" / "serve_lane_reload_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    # Reload persistence is server-side: the scratch backend's shell team
    # rehydrates by teamId from the team snapshot -- no live targets, no
    # localStorage hints.
    assert "addLane(" not in smoke
    assert "laneHintsByTargetId" not in smoke
    assert 'teamCommandPayload("createTeam"' in smoke
    assert "page.reload" in smoke
    assert "lane.teamId === before.teamId" in smoke
    assert "rehydrated lane changed teamId" in smoke
    assert "stamped lifetime did not survive the reload" in smoke


def test_serve_fresh_startup_import_shell_smoke_asserts_stale_hint_reset() -> None:
    smoke = (ROOT / "browser" / "serve_fresh_startup_import_shell_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "installStaleOpenLaneHints" in smoke
    assert "page.reload" in smoke
    assert "waitForImportShell(page)" in smoke
    assert "assertEqual(\n        afterReload.storedConfig," in smoke
    assert '"[]"' in smoke
    assert "fresh startup topology must settle on the import shell" in smoke
    assert "fresh startup must rewrite stale lane config" in smoke
    assert "snapshotRevision: teamSnapshotRevision" in smoke
    assert "stale open-lane hints must not mutate the team store revision" in smoke


def test_serve_team_metrics_smoke_asserts_work_follows_agent() -> None:
    smoke = (ROOT / "browser" / "serve_team_metrics_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "renderLaneMetricsPane(source)" in smoke
    assert "laneMetricsRenderModel" in smoke
    # Work follows the agent: counters leave the source lane and land on the dest.
    assert "{ acked: 1, sends: 2, toolCalls: 3" in smoke
    assert "{ acked: 14, sends: 25, toolCalls: 36" in smoke
    assert "stale/duplicate cells" in smoke
    assert "selectsStable" in smoke
    assert "focused metric lens select did not survive refresh" in smoke
    assert "chart did not use top metrics area" in smoke
    assert "grid did not use available horizontal space" in smoke


def test_serve_pending_badge_smoke_asserts_differential_ack() -> None:
    smoke = (ROOT / "browser" / "serve_pending_badge_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "LARGE_MESSAGE_COUNT = 5000" in smoke
    assert "lane.pending" in smoke
    assert "placeholderAfterSend" in smoke
    assert "placeholderAfterAck" in smoke
    assert "composer placeholder did not show submitted inbox" in smoke
    assert "composer placeholder did not clear after lane.pending ack" in smoke
    assert "latestPayloadPending" in smoke
    assert "lane.pending ack triggered an unexpected refresh" in smoke


def test_serve_task_card_live_smoke_asserts_task_add_without_reload() -> None:
    smoke = (ROOT / "browser" / "serve_task_card_live_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "SPICE_" + "TASK_BACKEND" in smoke
    assert "CODEX_" + "THREAD_ID" in smoke
    assert "--origin" in smoke
    assert "liveTaskCardSmokeOrigin" in smoke
    assert "liveTaskCardTargetOffset" in smoke
    assert 'thread?.state === "bound"' in smoke
    assert "Task capture: " in smoke
    assert "framenavigated" in smoke
    assert "task card appeared after page navigation/reload" in smoke


def test_serve_submit_latency_smoke_asserts_timing_buckets() -> None:
    smoke = (ROOT / "browser" / "serve_submit_latency_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "__spiceSubmitLatencySamples" in smoke
    assert "optimisticRenderMs" in smoke
    assert "liveBusOpenMs" in smoke
    assert "sendResultWaitMs" in smoke
    assert "responseHandlingMs" in smoke
    assert "totalMs" in smoke
    assert "serverTiming" in smoke
    assert "missing submit latency server timing" in smoke
    assert "submit latency smoke used the real lane.send transport" in smoke
    assert 'stubbed: type === "lane.send"' in smoke


def test_serve_structural_status_smoke_asserts_watcher_driven_completion() -> None:
    smoke = (ROOT / "browser" / "serve_structural_status_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "handleLiveBusMessage" in smoke
    assert 'source: "watch"' in smoke
    assert "latestActivityKind" in smoke
    assert "targetChoiceStatus" in smoke
    assert "maxStatusTransitionMs" in smoke
    assert "Confirmed fixed." in smoke


def test_serve_submission_lifecycle_smoke_asserts_keyed_progress() -> None:
    smoke = (ROOT / "browser" / "serve_submission_lifecycle_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "sendLanePayload" in smoke
    assert "handleLiveBusMessage" in smoke
    assert 'type: "lane.submission"' in smoke
    assert 'source: "watch"' in smoke
    assert "accepted" in smoke
    assert "received" in smoke
    assert "completed" in smoke
    assert "maxLifecycleTransitionMs" in smoke
    assert "submissionResponseKey" in smoke


def test_serve_composer_typing_latency_smoke_asserts_no_layout_work() -> None:
    smoke = (ROOT / "browser" / "serve_composer_typing_latency_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "typingLatencyLaneCount = 4" in smoke
    assert "typingLatencyCardsPerLane = 48" in smoke
    assert "syncLanePaneMetrics = function" in smoke
    assert "mosaicRenderMessageStream = function" in smoke
    assert "typing triggered pane metric syncs" in smoke
    assert "typing triggered message packing" in smoke


def test_serve_composer_reorder_smoke_asserts_swap_contract() -> None:
    smoke = (ROOT / "browser" / "serve_composer_reorder_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "snapshotComposerReorder(state)" in smoke
    assert "composerReorderDropTarget(" in smoke
    assert "clearComposerMoveDropHighlights()" in smoke
    # Lifted + dropped-on swap; the middle shard must not move.
    assert '["gamma", "beta", "alpha"]' in smoke
    assert "untouched shard beta moved" in smoke
    assert "gained a horizontal scrollbar" in smoke
    assert "transforms not cleared on teardown" in smoke


def test_serve_identity_smoke_uses_harness_for_mismatch() -> None:
    smoke = (ROOT / "browser" / "serve_identity_smoke.js").read_text(encoding="utf-8")

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "claude -> codex" in smoke
    assert "claude-opus -> gpt-5.5" in smoke
    assert "session: claude" in smoke
    assert "driver actual" in smoke


def test_serve_lanes_batch_subscribe_smoke_asserts_coalesced_single_render() -> None:
    smoke = (ROOT / "browser" / "serve_lanes_batch_subscribe_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    # One frame per microtask tick, one message-bearing render per fused host.
    assert "liveBusRequest = async (type, fields = {}) =>" in smoke
    assert '"lanes.subscribe"' in smoke
    assert "result.initialFrameCount !== 1" in smoke
    assert "result.initialHostRenderCount !== 1" in smoke
    assert "single host render did not cover every member's initial messages" in smoke
    # Reconnect resync covers every open lane in exactly one frame.
    assert "resubscribeLiveBusLanes();" in smoke
    assert "result.resyncFrameCount !== 1" in smoke
    # Thread-change and config-revision resubscribes coalesce into one flush.
    assert "ensureTeamMemberLane(" in smoke
    assert "result.coalescedFrameCount !== 1" in smoke
    # A failed lane keeps its batch slot; siblings render normally.
    assert '"boom from batch"' in smoke
    assert "result.siblingFreshRenderCount !== 1" in smoke


def test_serve_lane_prefs_local_smoke_asserts_hint_scoped_interface_prefs() -> None:
    smoke = (ROOT / "browser" / "serve_lane_prefs_local_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    # Mount branches: stored hint wins for the hinted lane, defaults for the
    # other, and the two lanes differentiate on real mounted values.
    assert '"narrate"' in smoke
    assert "hinted lane did not mount with its stored interface prefs" in smoke
    assert "unhinted lane did not mount with interface defaults" in smoke
    assert "result.hintSpeechMode === result.unhintedSpeechMode" in smoke
    # Setters persist per-target browser-local hints only.
    assert "setLaneSpeechMode(" in smoke
    assert "setLaneSelectedView(" in smoke
    assert "setter did not persist the browser-local hint" in smoke
    # Interface prefs never touch the shared store: config revision holds and
    # zero team commands leave the browser.
    assert "result.revisionAfter !== result.revisionBefore" in smoke
    assert "result.teamCommandCount !== 0" in smoke
    assert "local prefs did not survive the following snapshot read" in smoke


def test_serve_mosaic_single_settle_smoke_asserts_single_settle() -> None:
    smoke = (ROOT / "browser" / "serve_mosaic_single_settle_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    # Cold load comes solely from the server team snapshot and rides one
    # batched subscribe covering every lane.
    assert "applyTeamSnapshotPayload(" in smoke
    assert '"lanes.subscribe"' in smoke
    assert "result.subscribeFrameCount !== 1" in smoke
    # Every lane's initial messages present at the first settled paint (fused
    # team of two members plus a solo lane), including hydrated ack contexts.
    assert "result.fusedPresentCount !== result.fusedExpectedCount" in smoke
    assert "result.soloPresentCount !== result.soloExpectedCount" in smoke
    assert "result.fusedMessageRenderCount !== 1" in smoke
    assert "result.ackContextPresent !== true" in smoke
    # The per-host mosaic full-replay counter is a positive observable read from
    # the live event log, and equals its initial-mount value across a quiet
    # window (no post-settle reshuffle).
    assert "mosaicEventLog" in smoke
    assert '"full-replay"' in smoke
    assert "result.fusedReplayInitial < 1" in smoke
    assert "result.fusedReplayFinal !== result.fusedReplayInitial" in smoke
    assert "result.soloReplayFinal !== result.soloReplayInitial" in smoke


def test_serve_nack_render_smoke_asserts_warn_polarity() -> None:
    smoke = (ROOT / "browser" / "serve_nack_render_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    # The refusal renders like an ack but warn-colored: refused polarity classes,
    # a NACK chip, the --warn accent, and the combined ACK+NACK card.
    assert "ack-quotes--refused" in smoke
    assert "message-body--refused" in smoke
    assert "nackHasRefusedClass" in smoke
    assert "nackQuoteAccentIsWarn" in smoke
    assert 'result.nackBadges.includes("NACK")' in smoke
    assert "mixedHasBoth" in smoke
    assert "page.screenshot(" in smoke
