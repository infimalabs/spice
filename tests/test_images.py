"""Transcript image extraction: markdown refs, embedded decode, pairing."""

import base64
import json

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER
from spice.serve.images import (
    image_markdown,
    markdown_image_reference,
    rollout_image_from_offset,
    view_image_markdown,
    worktree_file_image_url,
)
from spice.serve.markdown import render_message_html
from spice.serve.messages import read_assistant_messages
from spice.transcript.events import Image, Provenance, ToolCall

PNG_BYTES = b"\x89PNG\r\n\x1a\nfakepixels"
PNG_DATA_URL = "data:image/png;base64," + base64.b64encode(PNG_BYTES).decode("ascii")
AT = Provenance(source="rollout.jsonl", line=0, ordinal=0, timestamp=None)


def _tool_output_payload() -> dict:
    return {
        "type": "function_call_output",
        "output": [{"type": "input_image", "image_url": PNG_DATA_URL}],
    }


def test_view_image_call_becomes_image_markdown():
    call = ToolCall(
        at=AT,
        call_id="call-1",
        name="view_image",
        arguments=json.dumps({"path": "shots/login screen.png"}),
    )
    assert view_image_markdown(call) == "![view_image](shots/login%20screen.png)"


def test_tool_output_embedded_image_routes_through_api():
    image = Image(
        at=AT,
        url=PNG_DATA_URL,
        content_type="input_image",
        tool_output_type="function_call_output",
    )
    markdown = image_markdown([image], worktree_id="wt", source_offset=17)
    assert (
        markdown == "![input_image](/api/work/trees/wt/messages/image?offset=17&item=0)"
    )


def test_assistant_message_image_content_becomes_markdown():
    image = Image(at=AT, url=PNG_DATA_URL, content_type="input_image", role="assistant")
    markdown = image_markdown([image], worktree_id="wt", source_offset=3)
    assert (
        markdown == "![input_image](/api/work/trees/wt/messages/image?offset=3&item=0)"
    )


def test_each_image_on_a_line_keeps_its_own_item_index():
    """The embedded URL addresses a picture by its place among typed images."""
    second = "data:image/gif;base64," + base64.b64encode(b"gif").decode("ascii")
    images = [
        Image(at=AT, url=PNG_DATA_URL, content_type="input_image"),
        Image(at=AT, url=second, content_type="input_image"),
    ]
    markdown = image_markdown(images, worktree_id="wt", source_offset=9)
    assert markdown == (
        "![input_image](/api/work/trees/wt/messages/image?offset=9&item=0)\n\n"
        "![input_image](/api/work/trees/wt/messages/image?offset=9&item=1)"
    )


def test_rollout_image_decodes_from_line_offset(tmp_path):
    first = json.dumps({"type": "response_item", "payload": {"type": "reasoning"}})
    second = json.dumps({"type": "response_item", "payload": _tool_output_payload()})
    rollout = tmp_path / "rollout.jsonl"
    rollout.write_text(f"{first}\n{second}\n", encoding="utf-8")
    offset = len(first.encode("utf-8")) + 1
    result = rollout_image_from_offset(
        rollout, offset=offset, item_index=0, driver=CODEX_DRIVER
    )
    assert result == (PNG_BYTES, "image/png")


def test_claude_image_decodes_from_transcript_owner(tmp_path, monkeypatch):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "claude"))
    raw = {
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "content": [
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": base64.b64encode(PNG_BYTES).decode("ascii"),
                            },
                        }
                    ],
                }
            ],
        },
    }
    line = json.dumps(raw)
    transcript = (
        tmp_path
        / "claude"
        / "projects"
        / "-private-tmp-spice-sup"
        / "11111111-2222-3333-4444-555555555555.jsonl"
    )
    transcript.parent.mkdir(parents=True)
    transcript.write_text(f"{line}\n", encoding="utf-8")

    assert rollout_image_from_offset(
        transcript, offset=0, item_index=0, driver=CLAUDE_DRIVER
    ) == (
        PNG_BYTES,
        "image/png",
    )


def test_markdown_reference_percent_encodes_delimiters():
    assert (
        markdown_image_reference("shot", "a (1) <b>.png")
        == "![shot](a%20%281%29%20%3Cb%3E.png)"
    )


def test_render_html_inlines_worktree_image():
    html = render_message_html("![shot](shots/a.png)", worktree_id="wt")
    assert '<p class="message-image-stack">' in html
    assert '<a class="message-image" ' in html
    assert (
        'href="/api/work/trees/wt/files/image?path=shots/a.png&amp;'
        'missing=placeholder"' in html
    )
    assert (
        '<img src="/api/work/trees/wt/files/image?path=shots/a.png&amp;'
        'missing=placeholder"' in html
    )
    assert 'target="_blank" rel="noopener"' in html


def test_render_html_inlines_api_image_directly():
    url = "/api/work/trees/wt/messages/image?offset=17&item=0"
    html = render_message_html(f"![tool]({url})")
    assert '<a class="message-image" ' in html
    assert 'href="/api/work/trees/wt/messages/image?offset=17&amp;item=0"' in html
    assert '<img src="/api/work/trees/wt/messages/image?offset=17&amp;item=0"' in html
    assert 'alt="tool"' in html
    assert 'target="_blank" rel="noopener"' in html


def test_paired_view_image_call_collapses_into_its_output(tmp_path):
    view_call = json.dumps(
        {
            "timestamp": "2026-06-10T12:00:00.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "view_image",
                "arguments": json.dumps({"path": "shot.png"}),
            },
        }
    )
    output = json.dumps(
        {
            "timestamp": "2026-06-10T12:00:01.000Z",
            "type": "response_item",
            "payload": _tool_output_payload(),
        }
    )
    rollout = tmp_path / "rollout.jsonl"
    rollout.write_text(f"{view_call}\n{output}\n", encoding="utf-8")
    messages = read_assistant_messages(rollout, worktree_id="wt")
    assert [message.source_kind for message in messages] == ["tool_output_image"]
    assert messages[0].image_only is True
    assert messages[0].preview == "image"
    assert 'href="/api/work/trees/wt/messages/image?offset=' in messages[0].display_html


def test_worktree_file_image_url_encodes_target():
    assert (
        worktree_file_image_url("lane one", ".spice/shots/x.png")
        == "/api/work/trees/lane%20one/files/image?path=.spice/shots/x.png&missing=placeholder"
    )
