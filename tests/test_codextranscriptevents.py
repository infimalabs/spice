"""Codex transcript decoding onto the closed typed event vocabulary."""

from __future__ import annotations

import pytest

from spice.agent.codextranscript import codex_line_events, project_codex_events
from spice.agent.driver import CODEX_DRIVER
from spice.transcript.events import (
    AssistantText,
    Compaction,
    ContextUsage,
    Image,
    Reasoning,
    ToolCall,
    ToolOutput,
    Unknown,
    UserMessage,
    WebSearch,
)

SOURCE = "/transcripts/codex.jsonl"
LINE = 17
TIMESTAMP = "2026-07-25T22:55:39.378Z"
TURN_ID = "019f9b24-3312-7a81-9ebb-67fe39537e28"
METADATA = {"turn_id": TURN_ID}
IMAGE_URL = "data:image/png;base64,iVBORw0KGgo="
LAST_TOTAL_TOKENS = 20_578
CUMULATIVE_TOTAL_TOKENS = 41_156
MODEL_CONTEXT_WINDOW = 258_400


def _response_item(payload: dict) -> dict:
    return {"timestamp": TIMESTAMP, "type": "response_item", "payload": payload}


CANONICAL_LINES = {
    "message": _response_item(
        {
            "type": "message",
            "id": "msg-1",
            "role": "assistant",
            "content": [
                {"type": "output_text", "text": "first"},
                {"type": "output_text", "text": "second"},
            ],
            "phase": "commentary",
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    ),
    "function_call": _response_item(
        {
            "type": "function_call",
            "id": "fc-1",
            "name": "exec_command",
            "namespace": "functions",
            "arguments": '{"cmd":"pwd"}',
            "call_id": "call-1",
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    ),
    "function_call_output": _response_item(
        {
            "type": "function_call_output",
            "id": "fco-1",
            "call_id": "call-1",
            "output": [
                {"type": "input_text", "text": "workspace"},
                {
                    "type": "input_image",
                    "image_url": IMAGE_URL,
                    "detail": "original",
                },
            ],
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    ),
    "custom_tool_call_output": _response_item(
        {
            "type": "custom_tool_call_output",
            "call_id": "call-2",
            "output": [
                {"type": "input_text", "text": "patch applied"},
                {
                    "type": "input_image",
                    "image_url": IMAGE_URL,
                    "detail": "original",
                },
            ],
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    ),
    "custom_tool_call": _response_item(
        {
            "type": "custom_tool_call",
            "id": "ctc-1",
            "status": "completed",
            "call_id": "call-2",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** End Patch\n",
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    ),
    "reasoning": _response_item(
        {
            "type": "reasoning",
            "id": "rs-1",
            "summary": [
                {"type": "summary_text", "text": "inspect"},
                {"type": "summary_text", "text": "decide"},
            ],
            "content": None,
            "encrypted_content": "opaque",
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    ),
    "web_search_call": _response_item(
        {
            "type": "web_search_call",
            "status": "completed",
            "action": {
                "type": "search",
                "query": "typed transcript events",
                "queries": ["typed transcript events", "event provenance"],
            },
        }
    ),
    "compacted": {"timestamp": TIMESTAMP, "type": "compacted", "payload": {}},
}


@pytest.mark.parametrize(("family", "raw"), CANONICAL_LINES.items())
def test_every_dispatched_family_has_an_exact_typed_projection(
    family: str, raw: dict
) -> None:
    events = codex_line_events(raw, source=SOURCE, line=LINE)
    assert events, family
    assert project_codex_events(events, raw["timestamp"]) == raw
    assert CODEX_DRIVER.normalize_transcript_line(raw) == raw
    assert all(event.at.source == SOURCE for event in events)
    assert all(event.at.line == LINE for event in events)
    assert [event.at.ordinal for event in events] == list(range(len(events)))
    assert all(event.at.timestamp == TIMESTAMP for event in events)


def test_dispatched_families_decode_to_their_semantic_event_kinds() -> None:
    expected = {
        "message": [AssistantText, AssistantText],
        "function_call": [ToolCall],
        "function_call_output": [ToolOutput, Image],
        "custom_tool_call_output": [ToolOutput, Image],
        "custom_tool_call": [ToolCall],
        "reasoning": [Reasoning, Reasoning],
        "web_search_call": [WebSearch],
        "compacted": [Compaction],
    }
    for family, raw in CANONICAL_LINES.items():
        assert [type(event) for event in codex_line_events(raw)] == expected[family]


def test_user_message_text_and_image_keep_source_order_and_role() -> None:
    raw = _response_item(
        {
            "type": "message",
            "id": "msg-user",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "inspect this"},
                {
                    "type": "input_image",
                    "image_url": IMAGE_URL,
                    "detail": "high",
                },
            ],
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    )
    events = codex_line_events(raw)
    assert [type(event) for event in events] == [UserMessage, Image]
    assert events[0].role == "user"
    assert events[1].role == "user"
    assert project_codex_events(events, TIMESTAMP) == raw


def test_plain_string_tool_output_projects_exactly() -> None:
    raw = _response_item(
        {
            "type": "function_call_output",
            "call_id": "call-plain",
            "output": "plain stdout",
        }
    )
    events = codex_line_events(raw)
    assert [type(event) for event in events] == [ToolOutput]
    assert events[0].output_is_list is False
    assert project_codex_events(events, TIMESTAMP) == raw


def test_real_shape_custom_tool_output_projects_without_driver_identity() -> None:
    raw = _response_item(
        {
            "type": "custom_tool_call_output",
            "call_id": "call_YZZKsKummmhWMWobmf9paEje",
            "output": [
                {
                    "type": "input_text",
                    "text": "Script completed\nWall time 1.2 seconds\nOutput:\n",
                },
                {"type": "input_text", "text": "spice/agent/lifecycle.py\n"},
            ],
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    )
    events = codex_line_events(raw, source=SOURCE, line=LINE)
    assert [type(event) for event in events] == [ToolOutput, ToolOutput]
    outputs = [event for event in events if isinstance(event, ToolOutput)]
    assert [event.content for event in outputs] == [
        "Script completed\nWall time 1.2 seconds\nOutput:\n",
        "spice/agent/lifecycle.py\n",
    ]
    assert all(event.tool_output_type == "custom_tool_call_output" for event in outputs)
    assert project_codex_events(events, TIMESTAMP) == raw


def test_custom_tool_output_images_carry_the_projection_discriminator() -> None:
    events = codex_line_events(CANONICAL_LINES["custom_tool_call_output"])
    assert [type(event) for event in events] == [ToolOutput, Image]
    output, image = events
    assert isinstance(output, ToolOutput)
    assert isinstance(image, Image)
    assert output.tool_output_type == "custom_tool_call_output"
    assert image.tool_output_type == "custom_tool_call_output"
    assert (
        project_codex_events(events, TIMESTAMP)
        == CANONICAL_LINES["custom_tool_call_output"]
    )


def test_image_only_custom_tool_output_projects_its_exact_family() -> None:
    raw = _response_item(
        {
            "type": "custom_tool_call_output",
            "call_id": "call-image",
            "output": [{"type": "input_image", "image_url": IMAGE_URL}],
        }
    )
    events = codex_line_events(raw)
    assert [type(event) for event in events] == [Image]
    image = events[0]
    assert isinstance(image, Image)
    assert image.tool_output_type == "custom_tool_call_output"
    assert project_codex_events(events, TIMESTAMP) == raw


def test_empty_reasoning_summary_remains_an_exact_reasoning_event() -> None:
    raw = _response_item(
        {
            "type": "reasoning",
            "id": "rs-empty",
            "summary": [],
            "encrypted_content": "opaque",
            "internal_chat_message_metadata_passthrough": METADATA,
        }
    )
    events = codex_line_events(raw)
    assert [type(event) for event in events] == [Reasoning]
    assert events[0].summary_type is None
    assert project_codex_events(events, TIMESTAMP) == raw


@pytest.mark.parametrize(
    "action",
    [
        {"type": "open_page", "url": "https://example.test/docs"},
        {
            "type": "find_in_page",
            "url": "https://example.test/docs",
            "pattern": "provenance",
        },
    ],
)
def test_web_search_action_variants_project_exactly(action: dict) -> None:
    raw = _response_item(
        {"type": "web_search_call", "status": "completed", "action": action}
    )
    assert project_codex_events(codex_line_events(raw), TIMESTAMP) == raw


def test_status_only_web_search_projects_without_inventing_an_action() -> None:
    raw = _response_item({"type": "web_search_call", "status": "completed"})
    assert project_codex_events(codex_line_events(raw), TIMESTAMP) == raw


def test_context_usage_is_typed_by_the_driver_usage_hook() -> None:
    last = {
        "input_tokens": 20_401,
        "cached_input_tokens": 300,
        "cache_write_input_tokens": 0,
        "output_tokens": 177,
        "reasoning_output_tokens": 73,
        "total_tokens": LAST_TOTAL_TOKENS,
    }
    cumulative = {
        "input_tokens": 40_802,
        "cached_input_tokens": 600,
        "cache_write_input_tokens": 0,
        "output_tokens": 354,
        "reasoning_output_tokens": 146,
        "total_tokens": CUMULATIVE_TOTAL_TOKENS,
    }
    raw = {
        "timestamp": TIMESTAMP,
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {
                "last_token_usage": last,
                "total_token_usage": cumulative,
                "model_context_window": MODEL_CONTEXT_WINDOW,
            },
            "rate_limits": {"primary": {"used_percent": 7.0}},
        },
    }
    events = CODEX_DRIVER.transcript_line_events(raw, source=SOURCE, line=LINE)
    assert [type(event) for event in events] == [ContextUsage]
    usage = events[0]
    assert usage.last.total_tokens == LAST_TOTAL_TOKENS
    assert usage.cumulative is not None
    assert usage.cumulative.total_tokens == CUMULATIVE_TOTAL_TOKENS
    assert usage.model_context_window == MODEL_CONTEXT_WINDOW

    fields = CODEX_DRIVER.context_snapshot_fields(raw)
    assert fields is not None
    assert fields.last == usage.last
    assert fields.cumulative == usage.cumulative
    assert fields.model_context_window == MODEL_CONTEXT_WINDOW
    # The extra rate-limit fact cannot be projected from ContextUsage, so the
    # exactness gate preserves the original line rather than partially rewriting it.
    assert CODEX_DRIVER.normalize_transcript_line(raw) is raw


def test_unrecognized_response_item_survives_as_unknown_and_identity() -> None:
    raw = _response_item({"type": "future_provider_item", "fact": 7})
    events = codex_line_events(raw)
    assert [type(event) for event in events] == [Unknown]
    assert events[0].raw_type == "future_provider_item"
    assert project_codex_events(events, TIMESTAMP) is None
    assert CODEX_DRIVER.normalize_transcript_line(raw) is raw
