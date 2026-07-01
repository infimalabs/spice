"""Driver transcript fidelity for future assistant-output marker aliases."""

from __future__ import annotations

import json
import unicodedata

from spice.agent.driver import CLAUDE_DRIVER, CODEX_DRIVER
from spice.agent.watchdog import AgentStdoutMessageScanner, JsonStdoutScanner


def _marker_fidelity_cases() -> tuple[tuple[str, str], ...]:
    nfc_marker = unicodedata.normalize("NFC", "🌶️🧾 title=Valid cafe\u0301 marker")
    return (
        ("nfc", nfc_marker),
        ("variation-selector-16", "🌶️✅ 20260701T041358000000Z: done"),
        ("zwj-sequence", "🌶️🧑‍💻 status=working"),
        (
            "literal-emoji-pair",
            "🌶️📋 title=Validate markers | project=lifecycle.protocol | "
            "acceptance=transcript fidelity holds",
        ),
    )


def test_codex_stdout_scanner_preserves_marker_fidelity_cases():
    for label, expected in _marker_fidelity_cases():
        captured: list[str] = []
        scanner = AgentStdoutMessageScanner(
            CODEX_DRIVER,
            captured.append,
            on_compaction=lambda: None,
        )

        scanner.process_line(f"{CODEX_DRIVER.stdout_assistant_marker}\n")
        for line in expected.splitlines():
            scanner.process_line(f"{line}\n")
        scanner.process_line("exec\n")
        scanner.close()

        assert captured == [expected], _fidelity_failure(
            "codex", label, expected, captured
        )


def test_claude_json_stdout_scanner_preserves_marker_fidelity_cases():
    for label, expected in _marker_fidelity_cases():
        captured: list[str] = []
        scanner = JsonStdoutScanner(
            captured.append,
            CLAUDE_DRIVER.normalize_transcript_line,
            on_compaction=lambda: None,
        )

        scanner.process_line(
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": expected}],
                    },
                },
                ensure_ascii=False,
            )
        )
        scanner.close()

        assert captured == [expected], _fidelity_failure(
            "claude", label, expected, captured
        )


def _fidelity_failure(
    driver_name: str, label: str, expected: str, captured: list[str]
) -> str:
    return (
        f"{driver_name} mutated {label} marker during transcript reconstruction\n"
        f"expected: {expected!r}\n"
        f"expected codepoints: {_codepoints(expected)}\n"
        f"actual: {captured!r}\n"
        f"actual codepoints: {_captured_codepoints(captured)}"
    )


def _captured_codepoints(captured: list[str]) -> str:
    if not captured:
        return "<no assistant message captured>"
    return " | ".join(_codepoints(item) for item in captured)


def _codepoints(text: str) -> str:
    return " ".join(f"U+{ord(char):04X}" for char in text)
