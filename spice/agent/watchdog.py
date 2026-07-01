"""Supervise agent stdout: archive ACKs and police prose against maxims.

The supervisor tees the agent's `exec` stdout into the log while a scanner
keyed on the driver's section markers reassembles each assistant message.
Every message gets two treatments:

* ACK'd inbox keys are archived immediately (the operator sees inbox items retire
  the moment the agent acknowledges it);
* the assistant-authored prose (clipped at generated tool-output boundaries)
  is trigger-scanned against the configured maxims and, on a hit, adjudicated
  by the local judge — violations are published back into the agent's inbox
  as `[MAXIM]` reminders, at most once per compaction epoch, with self-echo
  suppressed.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from threading import Thread
from typing import Callable, Protocol, TextIO, cast

from spice.agent.driver import AgentDriver, driver_for
from spice.agent.identity import ambient_thread_id
from spice.agent.maxims import (
    MaximBag,
    evaluate_maxim_any_violation,
    triggered_maxims,
)
from spice.agent.sidechannelnotify import publish_side_channel_feedback
from spice.mail.acks import (
    extract_task_batch_lines_from_text,
    summarize_ack_archival,
    summarize_nack_archival,
)
from spice.mail.inbox import (
    discard_inbox_items,
    notify_inbox_changed,
    write_inbox_item,
)
from spice.procs import popen_new_process_group_kwargs
from spice.sessions.util import first_text
from spice.tasks import config as task_config
from spice.tasks.create import TaskAddResult

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
    it becomes eligible again. The key is the final inbox body rather than an
    individual maxim id, so a new combined reminder can publish even if one of
    its maxims appeared in an earlier reminder.
    """

    def __init__(self) -> None:
        self._compaction_index = 0
        self._sent: dict[str, int] = {}
        self._published: dict[Path, str] = {}

    def note_compaction(self) -> None:
        self._compaction_index += 1

    def should_publish(self, reminder_key: str) -> bool:
        return self._sent.get(reminder_key) != self._compaction_index

    def mark_sent(self, reminder_key: str, path: Path) -> None:
        self._sent[reminder_key] = self._compaction_index
        self._published[path] = reminder_key

    def published_reminders(self) -> tuple[tuple[Path, str], ...]:
        return tuple(self._published.items())

    def forget_published(self, paths: set[Path]) -> None:
        for path in paths:
            self._published.pop(path, None)


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
        scanner = make_stdout_scanner(
            driver_for(repo_root),
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
            try:
                discarded = discard_pending_maxim_reminders(repo_root, reminder_gate)
            except Exception as exc:  # pragma: no cover - defensive supervisor logging
                log_handle.write(f"spice maxim supervisor cleanup error: {exc}\n")
                log_handle.flush()
            else:
                if discarded:
                    keys = " ".join(path.stem for path in discarded)
                    log_handle.write(
                        f"spice maxim supervisor cleanup discarded inbox: {keys}\n"
                    )
                    log_handle.flush()


def process_supervised_assistant_message(
    repo_root: Path,
    message_text: str,
    log_handle: TextIO,
    reminder_gate: MaximReminderGate,
) -> None:
    # Archival hits the ack-store (SQLite + git common dir). In production
    # repo_root is a real worktree, but a locked or corrupt store or a full
    # disk must not crash supervised-message processing — so each archival pass
    # runs inside the same surface-and-survive guard as the blocks below.
    _publish_nack_feedback(repo_root, message_text, log_handle)
    _publish_ack_feedback(repo_root, message_text, log_handle)
    try:
        results = create_inline_tasks(repo_root, message_text, log_handle)
        if results:
            publish_supervisor_feedback(
                repo_root,
                log_handle,
                "task.created",
                handles=[result.handle for result in results],
                projects=[result.project for result in results],
                routes=[result.route_feedback for result in results],
                **{"allowed-project-stems": task_config.assignable_stems()},
            )
            publish_supervisor_feedback(
                repo_root,
                log_handle,
                "task.backlog-note",
                message=INLINE_TASK_BACKLOG_NOTE,
            )
    except Exception as exc:  # supervisor-visible task failure
        log_handle.write(f"spice inline task supervisor error: {exc}\n")
        log_handle.flush()
        publish_supervisor_feedback(
            repo_root,
            log_handle,
            "task.error",
            error=str(exc),
            **{"allowed-project-stems": task_config.assignable_stems()},
        )
    try:
        record_supervised_lane_metrics(repo_root)
    except Exception as exc:  # supervisor-visible metric failure
        log_handle.write(f"spice metrics supervisor error: {exc}\n")
        log_handle.flush()
    try:
        publish_maxim_hits_as_inbox(
            repo_root, message_text, reminder_gate=reminder_gate
        )
    except Exception as exc:  # defensive supervisor logging
        log_handle.write(f"spice maxim supervisor error: {exc}\n")
        log_handle.flush()


def _publish_nack_feedback(
    repo_root: Path, message_text: str, log_handle: TextIO
) -> None:
    try:
        nack_summary = summarize_nack_archival(repo_root, message_text)
    except Exception as exc:  # surface-and-survive: archival must not crash the loop
        log_handle.write(f"spice nack archival supervisor error: {exc}\n")
        log_handle.flush()
        return
    for kind, keys in (
        ("nack.refused", nack_summary.refused),
        ("nack.already-refused", nack_summary.already_refused),
        ("nack.already-acked", nack_summary.already_acked),
        ("nack.unmatched", nack_summary.unmatched),
        ("nack.reason-required", nack_summary.reasonless),
    ):
        if keys:
            publish_supervisor_feedback(repo_root, log_handle, kind, keys=keys)


def _publish_ack_feedback(
    repo_root: Path, message_text: str, log_handle: TextIO
) -> None:
    try:
        ack_summary = summarize_ack_archival(repo_root, message_text)
    except Exception as exc:  # surface-and-survive: archival must not crash the loop
        log_handle.write(f"spice ack archival supervisor error: {exc}\n")
        log_handle.flush()
        return
    for kind, keys in (
        ("ack.archived", ack_summary.archived),
        ("ack.already-acked", ack_summary.already_acked),
        ("ack.unmatched", ack_summary.unmatched),
    ):
        if keys:
            publish_supervisor_feedback(repo_root, log_handle, kind, keys=keys)
    if ack_summary.noop:
        publish_supervisor_feedback(
            repo_root,
            log_handle,
            "ack.noop",
            message=ACK_NOOP_MESSAGE,
        )


def publish_supervisor_feedback(
    repo_root: Path, log_handle: TextIO, kind: str, **fields: object
) -> None:
    try:
        publish_side_channel_feedback(repo_root, kind, **fields)
    except Exception as exc:  # best-effort stderr feedback
        log_handle.write(f"spice side-channel feedback error: {exc}\n")
        log_handle.flush()


# An inline-created task lands on the backlog and is not the creator's to work.
# Phrased "unless" (not "until") so agents drop it rather than wait.
INLINE_TASK_BACKLOG_NOTE = (
    "inline tasks above are on the backlog, not yours — move on "
    "unless the allocator assigns one back via spice task next"
)
ACK_NOOP_MESSAGE = (
    'Run spice task add --project <stem.child> --title "..." '
    '--acceptance "..." to capture non-inbox work; ACK ignored: no inbox key found'
)


def create_inline_tasks(
    repo_root: Path, message_text: str, log_handle: TextIO
) -> list[TaskAddResult]:
    batch_lines = extract_task_batch_lines_from_text(message_text)
    if not batch_lines:
        return []
    empty = [index for index, line in enumerate(batch_lines, start=1) if not line]
    if empty:
        raise RuntimeError(f"inline TASK directive missing payload at line {empty[0]}")
    actor = _supervised_inline_task_actor(repo_root)
    from spice.tasks import create

    results = create.add_batch_results(
        batch_lines,
        actor_override=actor,
        creation_surface=task_config.TASK_CREATION_SURFACE_CLI,
    )
    if results:
        log_handle.write(
            "spice inline task created: " + _inline_task_result_text(results) + "\n"
        )
        log_handle.flush()
    return results


def _supervised_inline_task_actor(repo_root: Path) -> str:
    from spice.agent.lifecycle import agent_status

    return agent_status(repo_root).thread_id or ambient_thread_id() or ""


def _inline_task_result_text(results: list[TaskAddResult]) -> str:
    stems = _allowed_project_stems_text()
    return " ".join(
        f"{result.handle}({result.route_feedback};{stems})" for result in results
    )


def _allowed_project_stems_text() -> str:
    return "allowed-project-stems=" + ",".join(task_config.assignable_stems())


def record_supervised_lane_metrics(repo_root: Path) -> None:
    from spice.agent.lifecycle import agent_status
    from spice.serve.messages import resolve_thread_transcript
    from spice.serve.metrics import record_transcript_metrics_for_agent
    from spice.serve.team.ids import thread_actor_id
    from spice.serve.team.store import ServeTeamStore

    thread_id = agent_status(repo_root).thread_id
    if not thread_id:
        raise RuntimeError(f"could not resolve supervised agent id for {repo_root}")
    transcript = resolve_thread_transcript(thread_id, repo_root)
    if transcript is None:
        raise RuntimeError(f"could not resolve transcript for {thread_id}")
    record_transcript_metrics_for_agent(
        ServeTeamStore(),
        agent_id=thread_actor_id(thread_id),
        transcript_path=transcript.path,
    )


class StdoutScanner(Protocol):
    def process_line(self, line: str) -> None: ...

    def close(self) -> None: ...


def make_stdout_scanner(
    driver: AgentDriver,
    on_message: Callable[[str], None],
    *,
    on_compaction: Callable[[], None],
) -> StdoutScanner:
    """Pick the scanner matching this worktree's driver's stdout format."""
    if driver.stdout_format == "json":
        return JsonStdoutScanner(
            on_message,
            driver.normalize_transcript_line,
            on_compaction=on_compaction,
        )
    return AgentStdoutMessageScanner(driver, on_message, on_compaction=on_compaction)


class JsonStdoutScanner:
    """Reassemble assistant messages from a stream-json `exec` stdout.

    Each stdout line is one transcript event; the injected normalizer turns an
    assistant-message line into canonical prose, which feeds ACK archiving and
    maxim judging exactly as the marker scanner's reassembled blocks do.
    """

    def __init__(
        self,
        on_message: Callable[[str], None],
        normalize: Callable[[dict], dict | None],
        *,
        on_compaction: Callable[[], None] | None = None,
    ) -> None:
        self.on_message = on_message
        self._normalize = normalize
        self._on_compaction = on_compaction or (lambda: None)

    def process_line(self, line: str) -> None:
        try:
            raw = json.loads(line)
        except json.JSONDecodeError:
            return
        if not isinstance(raw, dict):
            return
        event = self._normalize(raw)
        if event is None:
            return
        if event.get("type") == "compacted":
            self._on_compaction()
            return
        payload = event.get("payload") or {}
        if payload.get("type") != "message" or payload.get("role") != "assistant":
            return
        text = first_text(payload.get("content"))
        if text and text.strip():
            self.on_message(text.strip())

    def close(self) -> None:
        return


class AgentStdoutMessageScanner:
    """Reassemble assistant messages out of the driver's `exec` stdout.

    The driver prints a marker line before each assistant block and distinct
    marker lines for other sections; everything between an assistant marker
    and the next section marker is one message.
    """

    def __init__(
        self,
        driver: AgentDriver,
        on_message: Callable[[str], None],
        *,
        on_compaction: Callable[[], None] | None = None,
    ) -> None:
        self._driver = driver
        self.on_message = on_message
        self._on_compaction = on_compaction or (lambda: None)
        self._capturing = False
        self._message_lines: list[str] = []

    def process_line(self, line: str) -> None:
        marker = line.rstrip("\r\n")
        if marker == self._driver.stdout_assistant_marker:
            self._flush()
            self._capturing = True
            return
        if marker in self._driver.stdout_section_markers:
            self._flush()
            if marker == self._driver.stdout_compaction_marker:
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
    reminder_gate: MaximReminderGate,
) -> list[Path]:
    statement_text = watchdog_judge_statement(message_text)
    if not statement_text:
        return []
    if any(prefix in statement_text for prefix in REMINDER_SUPPRESSION_PREFIXES):
        return []
    hits = triggered_maxims([statement_text], repo_root=repo_root)
    if not hits:
        return []
    violations = [
        hit
        for hit in hits
        if not evaluate_maxim_any_violation(hit.message, statement_text).agrees
    ]
    if not violations:
        return []
    body = _maxim_inbox_body(violations)
    if not reminder_gate.should_publish(body):
        return []
    path = write_inbox_item(repo_root, None, body)
    reminder_gate.mark_sent(body, path)
    paths = [path]
    return paths


def discard_pending_maxim_reminders(
    repo_root: Path, reminder_gate: MaximReminderGate
) -> list[Path]:
    """Discard still-pending maxim reminders authored by this supervisor."""
    items: list[dict[str, str]] = []
    discarded: list[Path] = []
    forget: set[Path] = set()
    for path, expected_text in reminder_gate.published_reminders():
        try:
            current_text = path.read_text(encoding="utf-8", errors="replace")
        except FileNotFoundError:
            forget.add(path)
            continue
        except OSError:
            continue
        if current_text != expected_text:
            continue
        items.append({"source_path": str(path)})
        discarded.append(path)
        forget.add(path)
    if items:
        discard_inbox_items(items)
        notify_inbox_changed(repo_root)
    if forget:
        reminder_gate.forget_published(forget)
    return discarded


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


def _maxim_inbox_body(hits: list[MaximBag]) -> str:
    reminders = dict.fromkeys(_one_line_maxim(hit.message) for hit in hits)
    return " ".join([WATCHDOG_REMINDER_PREFIX, *reminders]) + "\n"


def _one_line_maxim(maxim: str) -> str:
    sentence = " ".join(maxim.split())
    return sentence if sentence.endswith((".", "!", "?")) else f"{sentence}."
