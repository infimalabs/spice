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
    assert "New team" in smoke
