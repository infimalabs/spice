from pathlib import Path

ROOT = Path(__file__).resolve().parent


def test_serve_playwright_harness_starts_short_lived_scratch_server() -> None:
    harness = (ROOT / "browser" / "serve_playwright_harness.js").read_text(
        encoding="utf-8"
    )

    assert 'require("playwright")' in harness
    assert "fs.mkdtemp" in harness
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
    assert "New team" not in smoke


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
