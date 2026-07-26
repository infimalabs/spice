"""Static serve UI contracts: message accent palette and team slots."""

from __future__ import annotations

from spice.serve.web import STATIC_ROOT
from tests.test_servestatic import _serve_css_text


def test_static_message_accents_follow_team_slots_for_single_member_teams():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")
    app_stream = (STATIC_ROOT / "app.stream.js").read_text(encoding="utf-8")

    assert ".messages article[data-accent-slot]" in css
    assert ".messages article[data-occupant]" not in css
    assert "Boolean(laneGroupHost(lane).teamId)" in app_stream
    assert "laneMessageAttributionAgentCount(lane) > 1" in app_stream
    assert "function laneMessageAccentIndex(lane, item)" in app_stream
    assert "function laneMessageProducerTargetId(lane, item)" in app_stream
    assert "if (item.producerTargetId) return item.producerTargetId;" in app_stream
    assert (
        "candidate.targetThreadId === threadId ||\n"
        "      candidate.activeThreadId === threadId" in app_stream
    )
    assert (
        "const index = laneGroupMemberTargetIds(host).indexOf(targetId);" in app_stream
    )
    # Attribution indices are reduced into the palette range at the source so
    # a grown occupant ordinal can never make messageOccupantAccent throw.
    assert (
        "return laneOccupantOrdinal(host, item.threadId) % MESSAGE_ACCENT_SLOT_COUNT;"
        in app_stream
    )
    # Accent is decoupled from message identity: it is NOT in the render
    # fingerprint (so a composer reorder never re-renders a card) and is
    # applied by a style-only recolor pass instead.
    assert "accentSlot: laneMessageAccentIndex(lane, item)," not in app_stream
    assert "attributed: laneShouldAttributeMessages(lane)," in app_stream
    assert "function applyMessageAccentsIfChanged(lane)" in app_stream
    assert "const accentSlot = laneMessageAccentIndex(lane, item);" in app_render
    assert "if (item.threadId && laneShouldAttributeMessages(lane))" not in app_render
    assert "if (laneShouldAttributeMessages(lane))" in app_render
    assert "article.dataset.accentSlot = String(accentSlot);" in app_render
    assert "messageOccupantAccent(accentSlot)" in app_render


def test_static_message_accent_palette_names_all_six_team_slots():
    css = _serve_css_text()
    app_render = (STATIC_ROOT / "app.render.js").read_text(encoding="utf-8")

    assert "--team-teal-accent: #007c89;" in css
    assert "--team-plum-accent: #8a4fbf;" in css
    assert "--team-teal-accent: #5fd6d0;" in css
    assert "--team-plum-accent: #d1a3ff;" in css
    assert '"var(--team-teal-accent)",' in app_render
    assert '"var(--team-plum-accent)",' in app_render
    assert "if (index < messageOccupantAccentPalette.length)" in app_render
    assert "return messageOccupantAccentPalette[index];" in app_render
    assert (
        'throw new Error("team slot accent requires one of six team slots");'
        in app_render
    )
    assert "generatedMessageAccentHueStep" not in app_render
    assert "oklch(72% 0.14 " not in app_render
    assert (
        "messageOccupantAccentPalette[index % messageOccupantAccentPalette.length]"
        not in app_render
    )


def test_static_agent_names_use_accent_colors_without_bold_weight():
    css = _serve_css_text()
    app_lanes = (STATIC_ROOT / "app.lanes.js").read_text(encoding="utf-8")

    message_name_start = css.rindex(".message-agent-name {")
    message_name_rule = css[message_name_start : css.index("}", message_name_start)]
    compaction_label_start = css.index(".compaction-label {")
    compaction_label_rule = css[
        compaction_label_start : css.index("}", compaction_label_start)
    ]
    target_name_start = css.index(".target-choice-name {")
    target_name_rule = css[target_name_start : css.index("}", target_name_start)]

    assert "var(--message-occupant-accent, var(--muted)) 70%" in message_name_rule
    assert "font-weight: 400;" in message_name_rule
    assert "var(--compaction-accent, var(--fg)) 70%" in compaction_label_rule
    assert "font-weight: 400;" in compaction_label_rule
    assert "var(--target-choice-name-accent, var(--fg)) 70%" in target_name_rule
    assert "font-weight: 400;" in target_name_rule
    # The accent rides a class, so the name has to stay its own classed span
    # inside the copy wrapper rather than a bare <strong>. The button is built
    # node by node, so the structure is asserted through the builder calls.
    assert 'const copy = serveSpanWithClass("target-choice-copy");' in app_lanes
    assert 'const nameSpan = serveSpanWithClass("target-choice-name");' in app_lanes
    assert (
        'copy.append(nameSpan, serveSpanWithClass("target-choice-meta"));' in app_lanes
    )
    assert "function syncTargetChoiceNameAccent(button, target)" in app_lanes
    assert (
        'button.style.setProperty("--target-choice-name-accent", accent);' in app_lanes
    )
