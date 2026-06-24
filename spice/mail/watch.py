"""Tail an agent transcript for an ACK of a specific inbox key.

Used by the inbox send ACK path: after the publish returns, the watcher seeks
to EOF of the receiving agent's transcript JSONL and counts *user-facing*
assistant messages (each `response_item` with `payload.type=='message'` and
`role=='assistant'` is one prose block in the operator's UI). After three such
messages elapse without `ACK <our-key>: …` matching the canonical detector in
`spice.mail.acks`, the watcher republishes the inbox item under a fresh key —
the receiving agent sees it again on its next mailbox peek. The cycle repeats
until our key is ACK'd or the operator interrupts.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

from spice.agent.driver import AgentDriver, driver_for
from spice.mail.acks import extract_ack_segments_from_text
from spice.mail.inbox import inbox_dir, resend_inbox_item
from spice.sessions.util import first_text

MESSAGE_BUDGET = 3
POLL_INTERVAL_SECONDS = 0.25

# Inbox keys are `%Y%m%dT%H%M%S%fZ` — `\d{8}T\d{12}` in UTC with a trailing
# `Z`. Agents transcribing an ACK sometimes drop the `Z`; the digits alone are
# unique to the microsecond, so we match on the stem and let the `Z` be
# present or not.
_Z_SUFFIX_RE = re.compile(r"Z$")


def _key_stem(inbox_key: str) -> str:
    return _Z_SUFFIX_RE.sub("", inbox_key)


@dataclass(frozen=True)
class AckWatchOutcome:
    acked: bool
    assistant_messages_seen: int
    resends: int


def watch_for_ack(
    *,
    transcript_path: Path,
    inbox_key: str,
    original_text: str,
    target_repo_root: Path,
    on_ack: Callable[[str, str], None] | None = None,
    quiet: bool,
) -> AckWatchOutcome:
    """Block until `inbox_key` (or its latest resend) is ACK'd.

    Reads from EOF of `transcript_path`; counts user-facing assistant
    messages; re-issues the send (new timestamp key, escalated priority)
    every `MESSAGE_BUDGET` messages until an ACK is observed.

    `on_ack` (when provided) is called as `on_ack(text, key)` the moment our
    ACK is detected — pass an inline `say` for one-shot use, or a queue
    enqueue for multi-key drivers.
    """
    state = AckWatchState(
        inbox_key=inbox_key,
        original_text=original_text,
        target_repo_root=target_repo_root,
        quiet=quiet,
        on_ack=on_ack,
    )
    try:
        with transcript_path.open() as handle:
            handle.seek(0, os.SEEK_END)
            while not state.acked:
                state.check_archive()
                line = handle.readline()
                if not line:
                    time.sleep(POLL_INTERVAL_SECONDS)
                    continue
                state.process_line(line)
    except KeyboardInterrupt:
        state.note_interrupt()
    return state.outcome()


class AckWatchState:
    def __init__(
        self,
        *,
        inbox_key: str,
        original_text: str,
        target_repo_root: Path,
        quiet: bool,
        on_ack: Callable[[str, str], None] | None = None,
    ) -> None:
        self.original_key = inbox_key
        self.current_key = inbox_key
        self.original_text = original_text
        self.target_repo_root = target_repo_root
        self.quiet = quiet
        self.messages_since_resend = 0
        self.total_messages = 0
        self.resends = 0
        self.acked = False
        self._archived_keys: set[str] = set()
        self.on_ack = on_ack

    def process_line(self, line: str) -> None:
        text = extract_assistant_text(line, driver_for(self.target_repo_root))
        if text is None:
            return
        self.total_messages += 1
        self.messages_since_resend += 1
        if self._text_contains_our_ack(text):
            self.acked = True
            self._log(
                f"ACK observed in assistant message #{self.total_messages} "
                f"(key={self.current_key})"
            )
            if self.on_ack is not None:
                self.on_ack(text, self.current_key)
            return
        self._log(
            f"assistant message #{self.total_messages} without ACK "
            f"({self.messages_since_resend}/{MESSAGE_BUDGET})"
        )
        if self.messages_since_resend >= MESSAGE_BUDGET:
            self._resend()
            self.messages_since_resend = 0

    def _text_contains_our_ack(self, text: str) -> bool:
        return extract_owned_ack_utterance(text, self.current_key) is not None

    def check_archive(self) -> None:
        """Note when the current pending entry has left the inbox.

        A pending inbox item is archived only when the receiver verifiably
        ACKs it. Logging the transition tells the operator the message was
        acknowledged and retired; the resend cadence itself is driven by the
        message budget, not by this transition.
        """
        key = self.current_key
        if key in self._archived_keys:
            return
        pending_path = inbox_dir(self.target_repo_root) / f"{key}.txt"
        if pending_path.exists():
            return
        self._archived_keys.add(key)
        self._log(f"archived (key={key}); eligible for resurfacing")

    def _resend(self) -> None:
        attempt = self.resends + 1
        new_path = resend_inbox_item(
            self.target_repo_root,
            original_key=self.original_key,
            original_text=self.original_text,
            attempt=attempt,
            messages_elapsed=self.messages_since_resend,
        )
        self.resends = attempt
        self.current_key = new_path.stem
        self._log(
            f"resent as {new_path.name} (attempt {attempt}); "
            f"now watching key={self.current_key}"
        )

    def _log(self, message: str) -> None:
        if self.quiet:
            return
        print(f"  ack watcher: {message}", file=sys.stderr, flush=True)

    def note_interrupt(self) -> None:
        self._log(
            f"interrupted before ACK (key={self.current_key}, "
            f"messages={self.total_messages}, resends={self.resends})"
        )

    def outcome(self) -> AckWatchOutcome:
        return AckWatchOutcome(
            acked=self.acked,
            assistant_messages_seen=self.total_messages,
            resends=self.resends,
        )


def _line_might_carry_assistant_message(line: str) -> bool:
    return '"role":"assistant"' in line and '"message"' in line


def _safe_loads(line: str) -> dict[str, Any] | None:
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return None
    return obj if isinstance(obj, dict) else None


def extract_assistant_text(line: str, driver: AgentDriver) -> str | None:
    """Return the assistant prose carried by a transcript JSONL `line`, or None.

    Cheap substring prefilter first: an overwhelming majority of transcript
    lines are tool calls / function results that we can reject without a JSON
    parse. Only the lines that COULD be an assistant message reach
    `json.loads` — then we validate shape and pull the first text frame.
    """
    if not _line_might_carry_assistant_message(line):
        return None
    obj = _safe_loads(line)
    if obj is None:
        return None
    event = driver.normalize_transcript_line(obj)
    if event is None:
        return None
    payload = event.get("payload") or {}
    if event.get("type") != "response_item":
        return None
    if payload.get("type") != "message" or payload.get("role") != "assistant":
        return None
    text = first_text(payload.get("content"))
    return text or None


_APP_DIRECTIVE_LINE_RE = re.compile(r"^\s*::[a-z][a-z0-9-]*\{.*\}\s*$")

_ACK_FALLBACK_UTTERANCE = "ACK"


def line_might_carry_ack_directive(line: str) -> bool:
    """Cheap prefilter: transcript line could carry an `ACK …` directive."""
    if not _line_might_carry_assistant_message(line):
        return False
    return "ACK" in line


def extract_owned_ack_utterance(message_text: str, inbox_key: str) -> str | None:
    """Return the response for OUR key's ACK in `message_text`, or None.

    Used by the single-shot send/ACK/say path where the watcher only cares
    about its own key. Matches on the key stem so a transcribed ACK that
    drops the trailing `Z` still counts, and returns the matching ACK
    segment's content (or `"ACK"` for a body-less acknowledgment). The first
    owned segment wins.
    """
    stem = _key_stem(inbox_key)
    for segment in extract_ack_segments_from_text(message_text):
        if any(_key_stem(key) == stem for key in segment.keys):
            return segment.content or _ACK_FALLBACK_UTTERANCE
    return None


def strip_app_directive_lines(text: str) -> str:
    """Remove app control directives from assistant-facing prose.

    Directives such as `::git-stage{...}` and `::git-commit{...}` are meant
    for the host app, not for the steering transcript or audible speech.
    """
    lines = [line for line in text.splitlines() if not _is_app_directive_line(line)]
    return "\n".join(lines).rstrip()


def _is_app_directive_line(line: str) -> bool:
    return _APP_DIRECTIVE_LINE_RE.match(line) is not None
