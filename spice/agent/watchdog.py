"""Supervise agent stdout: archive ACKs and police prose against maxims.

The supervisor tees the agent's `exec` stdout into the log while a scanner
keyed on the driver's section markers reassembles each assistant message.
Every message gets two treatments:

* ACK'd inbox keys are archived immediately (the operator sees inbox items retire
  the moment the agent acknowledges it);
* the assistant-authored prose (clipped at generated tool-output boundaries)
  is trigger-scanned against the built-in maxims and, on a hit, adjudicated
  by the local judge — violations are published back into the agent's inbox
  as `[MAXIM]` reminders, at most once per compaction epoch, with self-echo
  suppressed.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from threading import Thread
from typing import Callable, TextIO, cast

from spice.agent.driver import DRIVER
from spice.agent.maxims import (
    BUILTIN_MAXIMS,
    evaluate_maxim_any_violation,
    triggered_maxims,
)
from spice.mail.acks import archive_ackd_inbox_items_from_assistant_message
from spice.mail.inbox import write_inbox_item
from spice.procs import popen_new_process_group_kwargs

LEGACY_REMINDER_PREFIX = "WATCHDOG:"
WATCHDOG_REMINDER_PREFIX = "[MAXIM]"
REMINDER_SUPPRESSION_PREFIXES = (WATCHDOG_REMINDER_PREFIX, LEGACY_REMINDER_PREFIX)
GENERATED_TOOL_OUTPUT_BOUNDARY_EXACT = frozenset({"apply patch"})
GENERATED_TOOL_OUTPUT_BOUNDARY_PREFIXES = (
    "patch:",
    "diff --git ",
    "index ",
    "--- a/",
    "+++ b/",
    "@@ ",
)


class MaximReminderGate:
    """Dedupe reminders within one compaction epoch.

    The same violation body publishes at most once until the agent's context
    compacts; after a compaction the agent has lost the earlier reminder, so
    it becomes eligible again.
    """

    def __init__(self) -> None:
        self._compaction_index = 0
        self._sent: dict[str, int] = {}

    def note_compaction(self) -> None:
        self._compaction_index += 1

    def should_publish(self, reminder_key: str) -> bool:
        return self._sent.get(reminder_key) != self._compaction_index

    def mark_sent(self, reminder_key: str) -> None:
        self._sent[reminder_key] = self._compaction_index


def spawn_supervised_agent(
    command: list[str], *, cwd: Path, log_path: Path, env: dict[str, str]
) -> tuple[subprocess.Popen[str], Thread]:
    process = subprocess.Popen(
        command,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
        bufsize=1,
        env=env,
        **popen_new_process_group_kwargs(),
    )
    typed = cast(subprocess.Popen[str], process)
    stdout_thread = supervise_agent_stdout(typed, repo_root=cwd, log_path=log_path)
    return typed, stdout_thread


def supervise_agent_stdout(
    process: subprocess.Popen[str], *, repo_root: Path, log_path: Path
) -> Thread:
    thread = Thread(
        target=_tee_agent_stdout,
        args=(process, repo_root, log_path),
        name=f"spice-agent-stdout-{process.pid}",
        daemon=True,
    )
    thread.start()
    return thread


def _tee_agent_stdout(
    process: subprocess.Popen[str], repo_root: Path, log_path: Path
) -> None:
    stdout = process.stdout
    if stdout is None:
        return
    with log_path.open("a", encoding="utf-8", errors="replace") as log_handle:
        reminder_gate = MaximReminderGate()
        scanner = AgentStdoutMessageScanner(
            lambda text: process_supervised_assistant_message(
                repo_root, text, log_handle, reminder_gate
            ),
            on_compaction=reminder_gate.note_compaction,
        )
        try:
            for line in stdout:
                log_handle.write(line)
                log_handle.flush()
                scanner.process_line(line)
        finally:
            scanner.close()


def process_supervised_assistant_message(
    repo_root: Path,
    message_text: str,
    log_handle: TextIO,
    reminder_gate: MaximReminderGate,
) -> None:
    archive_ackd_inbox_items_from_assistant_message(repo_root, message_text)
    try:
        publish_maxim_hits_as_inbox(
            repo_root, message_text, reminder_gate=reminder_gate
        )
    except Exception as exc:  # pragma: no cover - defensive supervisor logging
        log_handle.write(f"spice maxim supervisor error: {exc}\n")
        log_handle.flush()


class AgentStdoutMessageScanner:
    """Reassemble assistant messages out of the driver's `exec` stdout.

    The driver prints a marker line before each assistant block and distinct
    marker lines for other sections; everything between an assistant marker
    and the next section marker is one message.
    """

    def __init__(
        self,
        on_message: Callable[[str], None],
        *,
        on_compaction: Callable[[], None] | None = None,
    ) -> None:
        self.on_message = on_message
        self._on_compaction = on_compaction or (lambda: None)
        self._capturing = False
        self._message_lines: list[str] = []

    def process_line(self, line: str) -> None:
        marker = line.rstrip("\r\n")
        if marker == DRIVER.stdout_assistant_marker:
            self._flush()
            self._capturing = True
            return
        if marker in DRIVER.stdout_section_markers:
            self._flush()
            if marker == DRIVER.stdout_compaction_marker:
                self._on_compaction()
            return
        if self._capturing:
            self._message_lines.append(line.rstrip("\r\n"))

    def close(self) -> None:
        self._flush()

    def _flush(self) -> None:
        if not self._capturing:
            return
        text = "\n".join(self._message_lines).strip()
        self._capturing = False
        self._message_lines = []
        if text:
            self.on_message(text)


def publish_maxim_hits_as_inbox(
    repo_root: Path,
    message_text: str,
    *,
    reminder_gate: MaximReminderGate | None = None,
) -> list[Path]:
    statement_text = watchdog_judge_statement(message_text)
    if not statement_text:
        return []
    if any(prefix in statement_text for prefix in REMINDER_SUPPRESSION_PREFIXES):
        return []
    hits = triggered_maxims([statement_text])
    if not hits:
        return []
    violations = [
        hit
        for hit in hits
        if not evaluate_maxim_any_violation(BUILTIN_MAXIMS[hit], statement_text).agrees
    ]
    if not violations:
        return []
    body = _maxim_inbox_body(violations)
    if reminder_gate is not None and not reminder_gate.should_publish(body):
        return []
    paths = [write_inbox_item(repo_root, None, body)]
    if reminder_gate is not None:
        reminder_gate.mark_sent(body)
    return paths


def watchdog_judge_statement(message_text: str) -> str:
    """Return the assistant-authored text eligible for local maxim judging."""
    kept: list[str] = []
    for line in message_text.splitlines():
        if _is_generated_tool_output_boundary(line):
            break
        kept.append(line)
    return "\n".join(kept).strip()


def _is_generated_tool_output_boundary(line: str) -> bool:
    stripped = line.strip()
    if stripped in GENERATED_TOOL_OUTPUT_BOUNDARY_EXACT:
        return True
    return stripped.startswith(GENERATED_TOOL_OUTPUT_BOUNDARY_PREFIXES)


def _maxim_inbox_body(hits: list[frozenset[str]]) -> str:
    reminders = dict.fromkeys(_one_line_maxim(BUILTIN_MAXIMS[hit]) for hit in hits)
    return " ".join([WATCHDOG_REMINDER_PREFIX, *reminders]) + "\n"


def _one_line_maxim(maxim: str) -> str:
    sentence = " ".join(maxim.split())
    return sentence if sentence.endswith((".", "!", "?")) else f"{sentence}."
