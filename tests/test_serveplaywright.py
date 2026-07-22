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
    assert "options.args(scratch)" in harness


def test_serve_watch_smoke_uses_real_cli_and_live_fixture_append() -> None:
    smoke = (ROOT / "browser" / "serve_watch_smoke.js").read_text(encoding="utf-8")

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert '"watch"' in smoke
    assert "supervised_codex.jsonl" in smoke
    assert "supervised_claude.jsonl" in smoke
    assert 'document.querySelectorAll(".lane--observer")' in smoke
    assert 'document.querySelectorAll(".lane-composer")' in smoke
    assert ".observer-notice--empty" in smoke
    assert "fs.appendFile" in smoke
    assert "observer append visible" in smoke


def test_serve_playwright_harness_loads_shared_agent_context() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert (  # env-policy: allow
        'playwrightMcpConfigEnv = "SPICE_PLAYWRIGHT_MCP_CONFIG"' in harness
    )
    assert "defaultSharedPlaywrightConfigPath" in harness
    assert 'worktreeGitDir(),\n    ".spice",\n    "agents"' in harness
    assert '"playwright-mcp.json"' in harness
    assert "sharedPlaywrightConfigPath" in harness
    assert "process.env[playwrightMcpConfigEnv]" in harness
    assert "path.resolve(configPath)" in harness
    assert "run spice agent activation to create the worktree default" in harness
    assert 'playwrightMcpConfigEnv +\n          ", or pass a populated' in harness
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


def test_serve_composer_menu_order_smoke_renders_renewal_first() -> None:
    smoke = (ROOT / "browser" / "serve_composer_menu_order_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    assert "composerPrimaryMenuActions(lane, member" in smoke
    assert 'band.querySelectorAll(".composer-band-menu-action")' in smoke
    assert '"Renew this agent",\n  "Create new team",\n  "Leave all teams",' in smoke
    assert "assert.deepStrictEqual(result.labels, expectedLabels);" in smoke


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
    assert "snapshotRevision: laneStore.teamSnapshotRevision()" in smoke
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


def test_serve_team_width_smoke_measures_weighted_column_ratios() -> None:
    smoke = (ROOT / "browser" / "serve_team_width_smoke.js").read_text(encoding="utf-8")

    assert 'require("./serve_playwright_harness")' in smoke
    assert "withServePage(" in smoke
    # Team columns weight by agent count: measured in the real served UI at
    # representative 2:1 and 6:1 team sizes, wide enough to clear the
    # min-width floor.
    assert "TEAM_WIDTH_VIEWPORT = { width: 2560" in smoke
    assert "--lane-weight" in smoke
    assert 'assertPhase("pair (2:1)", result.pair, 2)' in smoke
    assert 'assertPhase("pack (6:1)", result.pack, 6)' in smoke
    assert "ratio: hostWidth / soloWidth" in smoke
    assert "solo column fell under the usable min width" in smoke
    assert "row overflowed its container at this viewport" in smoke


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


def test_serve_composer_pending_send_smoke_covers_lock_and_restore() -> None:
    smoke = (ROOT / "browser" / "serve_composer_pending_send_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'require("./serve_playwright_harness")' in smoke
    assert (
        'appearance: shard.classList.contains("composer-shard--pending-send")' in smoke
    )
    assert 'composerState: textarea.disabled ? "locked" : "editable"' in smoke
    assert "expectEqual(result.afterSecondSubmit.sendCount, 1" in smoke
    assert 'expectEqual(result.successSettled.text, ""' in smoke
    assert "expectEqual(result.failureSettled.text, config.failureText" in smoke
    assert "grouped.commandPending.payloads.length" in smoke
    assert 'grouped.commandFirstSettled.focusState, "focused"' in smoke
    assert "grouped.commandRepeatedPending.payloads.length" in smoke
    assert "grouped.commandRepeatedSettled.focusState" in smoke
    assert "grouped.buttonPayloads.length" in smoke
    assert 'waitForComposerState(textarea, "editable")' in smoke
    assert "new MutationObserver(" in smoke


def test_watch_frame_smokes_authenticate_subscription_generation() -> None:
    watch_smokes = {
        path.name: source
        for path in (ROOT / "browser").glob("*_smoke.js")
        if 'source: "watch"' in (source := path.read_text(encoding="utf-8"))
    }

    assert watch_smokes
    for name, source in watch_smokes.items():
        assert "activateIsolatedLaneWatch(" in source, name
        assert source.count('source: "watch"') == source.count(
            "subscriptionGeneration:"
        ), name
        assert "lane.liveBusSubscriptionGeneration" in source, name


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
    assert "liveTaskCardExpectedMetadata" in smoke
    assert '"todo, review"' in smoke
    assert "liveTaskCardTargetOffset" in smoke
    assert "waitForTaskCardStage" in smoke
    assert "liveTaskCardStageTimeoutMs" in smoke
    for stage in (
        "server-lane",
        "targets-ready",
        "bound-lane-selected",
        "socket-open",
        "subscription-generation",
        "watcher-activation",
        "initial-payload",
        "watch-delivery",
        "card-visible",
    ):
        assert f'"{stage}"' in smoke
    for marker in (
        "liveBusRequestSequence",
        "liveBusSubscriptionGeneration",
        "liveBusWatcherActive",
        "latestPayloadMessageKeys",
        "observedMessageKeys",
        "socketReadyState",
        "subscribePending",
    ):
        assert marker in smoke
    assert 'thread?.state === "bound"' in smoke
    assert "Task capture: " in smoke
    assert "framenavigated" in smoke
    assert "task card appeared after page navigation/reload" in smoke


def test_task_card_live_repeat_runner_adds_concurrent_manifest_load() -> None:
    runner = (ROOT / "browser" / "run_task_card_live_repeat.js").read_text(
        encoding="utf-8"
    )
    manifest = (ROOT / "browser" / "task_card_live_load_manifest.js").read_text(
        encoding="utf-8"
    )

    assert "repeatCount(countValue)" in runner
    assert "manifestValue ||" in runner
    assert 'runChild("browser-manifest-load"' in runner
    assert '"serve_task_card_live_smoke.js"' in runner
    assert "const results = await Promise.all(jobs);" in runner
    assert 'path: "serve_lanes_batch_subscribe_smoke.js"' in manifest
    assert 'path: "serve_mosaic_single_settle_smoke.js"' in manifest


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
    assert "completionMessageKey" in smoke
    assert "restoredHeaderStructure" in smoke
    assert "narrowComposerWidthPx" in smoke


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
    # Every member of a visible fused host stays live without click focus; a
    # same-group selection emits no redundant configuration, while a dirty
    # member batches one concrete resubscribe.
    assert "batchPhaseFocusWhilePending" in smoke
    assert "state.releaseDeferredSubscribe" in smoke
    assert "result.visibleHostLiveWithoutFocus !== true" in smoke
    assert "batchPhaseTopologyActivity" in smoke
    assert "result.topologyDirectChildMutationCount !== 0" in smoke
    assert "result.splitMemberFocused !== false" in smoke
    assert "result.fusedMemberFocused !== true" in smoke
    assert "result.pendingSelectedFocus !== true" in smoke
    assert "result.preReleaseFocusConfigureCount !== 0" in smoke
    assert "result.focusConfigureCount !== 0" in smoke
    assert "result.dirtyMemberFrameCount !== 1" in smoke
    assert "result.dirtyMemberCleared !== true" in smoke
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


def test_task_filter_pill_smoke_covers_live_unavailable_and_resolved_states() -> None:
    smoke = (ROOT / "browser" / "serve_task_filter_pills_smoke.js").read_text(
        encoding="utf-8"
    )

    assert 'count: "2·1·2"' in smoke
    assert 'count: "0·0·3"' in smoke
    assert 'count: "3·0·0"' in smoke
    assert 'count: "0·2·0"' in smoke
    assert 'count: "1·0·0"' in smoke
    assert 'title:\n      "0 ready, 0 active/in flight' in smoke
    # Saturation encodes agent coverage layered with work: a covered stem climbs
    # saturated -> active -> assigned, an uncovered-but-ready stem reads idle, and
    # nothing-movable-uncovered rests one step above it on dormant.
    assert 'tone: "saturated"' in smoke
    assert 'tone: "active"' in smoke
    assert 'tone: "assigned"' in smoke
    assert 'tone: "idle"' in smoke
    assert 'tone: "dormant"' in smoke
    # The coverage ramp guard reads each tone's rendered color and asserts the
    # covered saturated->active->assigned steps plus dormant and the uncovered
    # neutral idle floor form one constant-hue --good desaturation -- a
    # green->cyan hue shift regression fails the smoke.
    assert "getComputedStyle(pill).color" in smoke
    assert "function rgbToHsl(" in smoke
    assert "assertCoverageSaturationRamp(pills)" in smoke
    assert "coverage ramp step shifted hue instead of desaturating" in smoke
    assert (
        "coverage ramp saturation did not fall saturated>active>assigned>idle" in smoke
    )
    # Dropping coverage (stopping the lane) desaturates the same ready work off
    # saturated, and hidden stems keep the restored warn accent instead of
    # collapsing onto the public dormant tone.
    assert "async function dropCoverage(page)" in smoke
    assert "assertHiddenWarnAccent(pills)" in smoke
    assert "hidden stem desaturated to gray instead of the warn accent" in smoke
    assert (
        "uncovered serve did not desaturate below its covered saturated tone" in smoke
    )
    assert "uncovered ready stem is not the neutral idle endpoint" in smoke
    assert (
        "hidden oops pill collapsed onto the public dormant tone instead of warn"
        in smoke
    )
    assert (
        'labels !== "serve,studies,cli,tests,lifecycle,agent,oops,maxim_proposal"'
        in smoke
    )
    assert 'inventory.revision = "10000000000000000000000000000";' in smoke
    assert 'inventory.revision = "100000000000000000000000000000";' in smoke
    assert 'ariaHidden: strip?.getAttribute("aria-hidden")' in smoke
    assert "page.screenshot(" in smoke


def test_structural_status_smoke_covers_constant_hue_pip_desaturation() -> None:
    smoke = (ROOT / "browser" / "serve_structural_status_smoke.js").read_text(
        encoding="utf-8"
    )

    assert '"running",\n  "starting",\n  "stopping"' in smoke
    assert '"running-stale",\n  "idle"' in smoke
    assert "getComputedStyle(lane.pipEl).backgroundColor" in smoke
    assert "function assertPipSaturationRamp(colors)" in smoke
    assert "pip status shifted hue instead of desaturating" in smoke
    assert "pip saturation did not fall running>starting>stopping>stale>idle" in smoke
    assert "startup-stalled pip diverged from the stopping ramp step" in smoke
    assert "page.screenshot({ path: screenshotPath })" in smoke


# The serve browser smokes that source their target/team wire shape from the
# shared payload_factory authority. serve_structural_status is deliberately
# absent: it exercises a variable-version watch/status payload with no
# target-identity trio and no actor-id prefix, so it has nothing to fold.
PAYLOAD_FACTORY_SMOKES = (
    "serve_lifetime_team_smoke.js",
    "serve_mosaic_join_smoke.js",
    "serve_composer_accent_smoke.js",
    "serve_lifetime_reflow_smoke.js",
    "serve_mosaic_team_smoke.js",
    "serve_team_width_smoke.js",
    "serve_lane_prefs_local_smoke.js",
    "serve_lanes_batch_subscribe_smoke.js",
    "serve_mosaic_single_settle_smoke.js",
    "serve_identity_smoke.js",
)

# A migrated smoke reaches the authority through one of these public tokens:
# in-page smokes call window.spicePayloads.*, Node-scope smokes destructure the
# builders or the actor-id helpers from the require.
PAYLOAD_FACTORY_API_TOKENS = (
    "spicePayloads",
    "targetPayload",
    "teamPayload",
    "teamSnapshot",
    "threadActorId",
    "targetActorId",
    "installScript",
)


def test_payload_factory_is_the_single_wire_shape_authority() -> None:
    factory = (ROOT / "browser" / "payload_factory.js").read_text(encoding="utf-8")

    # One set of top-level builders serves both delivery modes: Node-scope
    # require (module.exports) and in-page injection assembled from the same
    # helper list (installScript -> window.spicePayloads), so neither mode can
    # drift from the other.
    assert "module.exports = {" in factory
    assert "const installScript =" in factory
    assert "FACTORY_HELPERS" in factory
    assert "fn.toString()" in factory
    assert "window.spicePayloads" in factory
    # The target/thread actor-id prefixes are defined here once, inline in the
    # actor-id helpers, as mirrors of the production sources they must stay in
    # lockstep with.
    assert 'return "target:" + id' in factory
    assert 'return "thread:" + id' in factory
    assert "spice/serve/static/app.js" in factory
    assert "spice/serve/team/ids.py" in factory
    # The public builder set the smokes consume, plus the deep-merge override
    # hook that lets a smoke state only its intentional deviation.
    for symbol in (
        "function targetActorId",
        "function threadActorId",
        "function targetPayload",
        "function teamPayload",
        "function teamSnapshot",
        "function withOverrides",
        "return withOverrides(base, overrides)",
    ):
        assert symbol in factory, symbol


def test_serve_smokes_share_the_payload_factory_authority() -> None:
    for name in PAYLOAD_FACTORY_SMOKES:
        smoke = (ROOT / "browser" / name).read_text(encoding="utf-8")
        assert 'require("./payload_factory")' in smoke, name
        assert any(token in smoke for token in PAYLOAD_FACTORY_API_TOKENS), name
