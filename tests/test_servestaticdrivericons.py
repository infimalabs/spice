"""Static serve UI contracts: composer driver icons."""

from __future__ import annotations

import subprocess
from pathlib import Path

from spice.serve.web import STATIC_ROOT
from tests.test_servestatic import _assert_contains_all, _serve_css_text


def test_static_composer_driver_icons_use_local_driver_assets():
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_shell = (STATIC_ROOT / "app.shell.js").read_text(encoding="utf-8")
    app_composer = (STATIC_ROOT / "app.composer.js").read_text(encoding="utf-8")
    openai_icon = STATIC_ROOT / "icons" / "openai.svg"
    claude_icon = STATIC_ROOT / "icons" / "claude.svg"
    assert openai_icon.is_file()
    assert claude_icon.is_file()
    assert 'fill="currentColor"' in openai_icon.read_text(encoding="utf-8")
    assert 'fill="currentColor"' in claude_icon.read_text(encoding="utf-8")

    _assert_contains_all(
        app_render,
        (
            "function targetIdentityDriverName(identity)",
            "function targetIdentityDriverModel(identity)",
            "function targetIdentityDriverEffort(identity)",
            "function applyLaneServeAgentIdentity(lane, payload)",
            "function serveAgentDesiredDriverName(identity)",
            "function serveAgentActualDriverName(identity)",
            "function identityDisplayPair(actual, desired)",
            "lane.driverName = identityDisplayPair(actualDriver, desiredDriver);",
            "lane.driverIconName = actualDriver || transcriptOwner || desiredDriver;",
            "function driverIconAssetPath(driver)",
            "function driverDisplayLabel(driver)",
            "function driverIdentityTooltip(fields)",
            'claude: "/static/icons/claude.svg",',
            'codex: "/static/icons/openai.svg",',
            'openai: "/static/icons/openai.svg",',
            '"Codex driver"',
            '"driver: " + driverName',
            '"model: " + model',
            '"effort: " + effort',
            '"thread: " + (threadId || "unbound")',
            '"session: " + session',
        ),
    )
    _assert_contains_all(
        app_shell,
        (
            "const serveAgentIdentity = target.serveAgentIdentity || {};",
            "serveAgentActualDriverName(serveAgentIdentity)",
            "serveAgentDesiredDriverName(serveAgentIdentity)",
            "driverIconName:",
        ),
    )
    _assert_contains_all(
        app_composer,
        (
            "syncComposerDriverIcon(primary, member);",
            "return driverIconAssetPath(driver);",
            "return driverIdentityTooltip({",
            "icon.dataset.composerDriverIcon = driver;",
            "const tooltip = composerDriverTooltip(member, driver);",
            "icon.title = tooltip;",
            'icon.setAttribute("aria-label", tooltip);',
            'icon.setAttribute("role", "img");',
        ),
    )
    assert '"source: worktree launch config"' not in app_composer
    assert (
        'icon.style.setProperty("--composer-driver-icon-url", '
        "'url(\"' + src + '\")');" in app_composer
    )


def test_composer_driver_icon_rerender_keeps_matching_dom_node():
    app_composer = STATIC_ROOT / "app.composer.js"
    script = Path(__file__).with_name("fixtures") / "composer_driver_icon_reconcile.js"

    result = subprocess.run(
        ["node", str(script), str(app_composer)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_accepted_composer_send_clears_only_target_draft_stack():
    app_composer = STATIC_ROOT / "app.composer.js"
    script = Path(__file__).with_name("fixtures") / "composer_accepted_draft_clear.js"

    result = subprocess.run(
        ["node", str(script), str(app_composer)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_composer_driver_icons_style_local_driver_assets():
    css = _serve_css_text()
    icon_start = css.index(".composer-driver-icon {")
    icon_rule = css[icon_start : css.index("}", icon_start)]
    icon_before_start = css.index(".composer-driver-icon::before {")
    icon_before_rule = css[icon_before_start : css.index("}", icon_before_start)]
    claude_rule_start = css.index(".composer-driver-icon--claude {")
    claude_rule = css[claude_rule_start : css.index("}", claude_rule_start)]
    openai_rule_start = css.index(
        ".composer-driver-icon--codex,\n.composer-driver-icon--openai {"
    )
    openai_rule = css[openai_rule_start : css.index("}", openai_rule_start)]
    menu_open_rule_start = css.index(
        ".composer-band--menu-open .composer-driver-icon {"
    )
    menu_open_rule = css[menu_open_rule_start : css.index("}", menu_open_rule_start)]
    textarea_start = css.index(".composer-band--primary textarea {")
    textarea_rule = css[textarea_start : css.index("}", textarea_start)]

    _assert_contains_all(
        icon_rule,
        (
            "bottom: 8px;",
            "cursor: help;",
            "height: 18px;",
            "pointer-events: auto;",
            "position: absolute;",
            "right: 8px;",
            "width: 18px;",
        ),
    )
    _assert_contains_all(
        icon_before_rule,
        (
            'content: "";',
            "inset: 2px;",
            "-webkit-mask: var(--composer-driver-icon-url) center / contain no-repeat;",
            "mask: var(--composer-driver-icon-url) center / contain no-repeat;",
            "position: absolute;",
        ),
    )
    assert (
        "--composer-driver-icon-color: color-mix(in srgb, #d97706 88%, var(--fg));"
        in (claude_rule)
    )
    assert "opacity: 0.72;" in claude_rule
    assert (
        "--composer-driver-icon-color: color-mix(in srgb, var(--fg) 86%, var(--control));"
        in openai_rule
    )
    assert "opacity: 0.74;" in openai_rule
    assert "display: none;" in menu_open_rule
    assert "padding-bottom: 28px;" in textarea_rule
    assert "padding-right: 32px;" in textarea_rule


def test_static_target_choice_driver_icons_reuse_shared_driver_logic():
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")
    app_composer = (STATIC_ROOT / "app.composer.js").read_text(encoding="utf-8")

    _assert_contains_all(
        app_lanes,
        (
            "function targetChoiceMetadataParts(target)",
            "const statusIndex = parts.length;",
            "function targetChoiceDriverIconName(target)",
            "function targetChoiceDriverTooltip(target, driver)",
            "function targetChoiceDriverIcon(target, driver, src)",
            "function renderTargetChoiceMetadata(metadataEl, target)",
            "const src = driverIconAssetPath(driver);",
            "return driverIdentityTooltip({",
            "icon.dataset.targetChoiceDriverIcon = driver;",
            'icon.setAttribute("aria-label", tooltip);',
            'icon.setAttribute("role", "img");',
            'icon.className = "target-choice-driver-icon target-choice-driver-icon--"',
            'icon.style.setProperty("--target-choice-driver-icon-url", ',
            "if (metadataEl) renderTargetChoiceMetadata(metadataEl, target);",
        ),
    )

    # The menu and the composer resolve their driver emblem through the same
    # render.js helper, so the icon asset and the tooltip format live in exactly
    # one place instead of being copied per surface.
    assert "const src = driverIconAssetPath(driver);" in app_lanes
    assert "return driverIconAssetPath(driver);" in app_composer
    assert "return driverIdentityTooltip({" in app_lanes
    assert "return driverIdentityTooltip({" in app_composer


def test_target_choice_driver_icon_replaces_middle_dot():
    app_lanes = STATIC_ROOT / "app.lanes.js"
    script = Path(__file__).with_name("fixtures") / "target_choice_driver_icon.js"

    result = subprocess.run(
        ["node", str(script), str(app_lanes)],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_static_target_choice_driver_icons_style_inline_marker():
    css = _serve_css_text()
    icon_start = css.index(".target-choice-driver-icon {")
    icon_rule = css[icon_start : css.index("}", icon_start)]
    before_start = css.index(".target-choice-driver-icon::before {")
    before_rule = css[before_start : css.index("}", before_start)]
    claude_start = css.index(".target-choice-driver-icon--claude {")
    claude_rule = css[claude_start : css.index("}", claude_start)]
    openai_start = css.index(
        ".target-choice-driver-icon--codex,\n.target-choice-driver-icon--openai {"
    )
    openai_rule = css[openai_start : css.index("}", openai_start)]

    _assert_contains_all(
        icon_rule,
        (
            "cursor: help;",
            "display: inline-block;",
            "height: 12px;",
            "position: relative;",
            "vertical-align: -2px;",
            "width: 12px;",
        ),
    )
    _assert_contains_all(
        before_rule,
        (
            "background: var(--target-choice-driver-icon-color);",
            'content: "";',
            "inset: 0;",
            "-webkit-mask: var(--target-choice-driver-icon-url) center / contain no-repeat;",
            "mask: var(--target-choice-driver-icon-url) center / contain no-repeat;",
            "position: absolute;",
        ),
    )
    assert (
        "--target-choice-driver-icon-color: color-mix(in srgb, #d97706 88%, var(--fg));"
        in claude_rule
    )
    assert "opacity: 0.82;" in claude_rule
    assert (
        "--target-choice-driver-icon-color: color-mix(in srgb, var(--fg) 86%, var(--control));"
        in openai_rule
    )
    assert "opacity: 0.84;" in openai_rule
