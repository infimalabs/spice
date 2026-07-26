"""Supervise agent stdout: archive ACKs and police prose against maxims.

The supervisor tees the agent's `exec` stdout into the log while a scanner
keyed on the driver's section markers reassembles each assistant message.
Every message gets two treatments:

* ACK'd inbox keys are archived immediately (the operator sees inbox items retire
  the moment the agent acknowledges it);
* the assistant-authored prose (clipped at generated tool-output boundaries)
  is trigger-scanned against the configured maxims and, on a hit, published
  back into the agent's inbox as `[MAXIM]` reminders, at most once per
  content-derived reminder key per compaction epoch, with self-echo suppressed.

By default the publish is judge-free: a matched trigger bag publishes directly,
accepting more false positives and needing no local judge. An installation that
wants fewer false positives opts into adjudication (`[judge] enabled`), which
gates each hit through the local two-judge verdict before publishing.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from threading import Condition, Thread
from typing import Any, Callable, Protocol, TextIO, cast

from spice.agent.driver import AgentDriver, driver_for
from spice.agent.identity import ambient_thread_id
from spice.config.values import maxim_adjudication_enabled
from spice.agent.maxims import (
    MaximBag,
    evaluate_maxim_any_violation,
    triggered_maxims,
)
from spice.agent.maximmetrics import (
    MAXIM_EVENT_FIRE,
    MAXIM_EVENT_GATE_SUPPRESSED,
    MAXIM_EVENT_JUDGED_CONFIRMED,
    MAXIM_EVENT_JUDGED_REJECTED,
    MAXIM_EVENT_PUBLISHED,
    MaximMetricEventWrite,
    record_maxim_metric_events,
)
from spice.agent.sidechannelnotify import publish_side_channel_feedback
from spice.mail.ackarchive import summarize_ack_archival, summarize_nack_archival
from spice.mail.ackgrammar import (
    extract_ack_keys_from_text,
    extract_task_batch_lines_from_text,
)
from spice.mail.inbox import (
    discard_inbox_items,
    notify_inbox_changed,
    write_inbox_item,
)
from spice.process.groups import popen_new_process_group_kwargs
from spice.tasks import config as task_config
from spice.tasks.create import TaskAddResult
from spice.transcript.decode import first_text

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

    The same reminder key publishes at most once until the agent's context
    compacts; after a compaction the agent may have lost the earlier reminder,
    so it becomes eligible again. The compaction index never changes the
    reminder key or body: content determines text, compaction only resets
    eligibility. The key is derived from the triggered maxim bags before
    judging, while cleanup stores the final inbox body separately so it only
    discards reminders whose file text still matches this supervisor's rendered
    reminder.
    """

    def __init__(self) -> None:
        self._compaction_index = 0
        self._sent: dict[str, int] = {}
        self._published: dict[Path, tuple[str, str]] = {}

    def note_compaction(self) -> None:
        self._compaction_index += 1

    def should_publish(self, reminder_key: str) -> bool:
        return self._sent.get(reminder_key) != self._compaction_index

    def mark_sent(self, reminder_key: str, path: Path, expected_text: str) -> None:
        self._sent[reminder_key] = self._compaction_index
        self._published[path] = (reminder_key, expected_text)

    def published_reminders(self) -> tuple[tuple[Path, str], ...]:
        return tuple(
            (path, expected_text)
            for path, (_, expected_text) in self._published.items()
        )

    def forget_published(self, paths: set[Path]) -> None:
        for path in paths:
            self._published.pop(path, None)


class AgentStartupSignal:
    """Notify the supervisor when first activity arrives or the process exits.

    A resumed thread can spend minutes compacting an oversized transcript
    before it can act at all. Compaction is liveness without readiness: it
    swings the stall deadline out to the compacting window so the supervisor
    does not terminate a process that is working -- terminating it aborts the
    compaction, leaves the transcript exactly as oversized as before, and
    relaunches into the identical kill forever -- while still leaving the lane
    `starting` rather than ready, because nothing has been produced yet.

    Each phase change restarts the wait against the window that now applies,
    so callers block on the real start/settle events instead of polling.
    """

    def __init__(self) -> None:
        self._condition = Condition()
        self._activity = False
        self._finished = False
        self._compacting = False
        self._phase_generation = 0

    def note_activity(self) -> None:
        with self._condition:
            self._activity = True
            self._condition.notify_all()

    def note_finished(self) -> None:
        with self._condition:
            self._finished = True
            self._condition.notify_all()

    def note_compaction_active(self, active: bool) -> None:
        with self._condition:
            self._compacting = active
            self._phase_generation += 1
            self._condition.notify_all()

    def wait(self, timeout_seconds: float, *, compacting_seconds: float) -> str:
        with self._condition:
            while True:
                generation = self._phase_generation
                window = compacting_seconds if self._compacting else timeout_seconds
                self._condition.wait_for(
                    lambda: (
                        self._activity
                        or self._finished
                        or self._phase_generation != generation
                    ),
                    timeout=max(0.0, window),
                )
                if self._activity:
                    return "activity"
                if self._finished:
                    return "finished"
                if self._phase_generation == generation:
                    return "compacting-timeout" if self._compacting else "timeout"


def startup_signal_for_supervised_thread(thread: Thread) -> AgentStartupSignal:
    """The readiness signal owned by one supervised stdout thread."""
    signal = getattr(thread, "startup_signal", None)
    if isinstance(signal, AgentStartupSignal):
        return signal
    signal = AgentStartupSignal()
    setattr(thread, "startup_signal", signal)
    return signal


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
    startup_signal = AgentStartupSignal()
    stdout_thread = supervise_agent_stdout(
        typed,
        repo_root=cwd,
        log_path=log_path,
        on_activity=startup_signal.note_activity,
        on_compaction_active=startup_signal.note_compaction_active,
    )
    setattr(stdout_thread, "startup_signal", startup_signal)
    return typed, stdout_thread


def supervise_agent_stdout(
    process: subprocess.Popen[str],
    *,
    repo_root: Path,
    log_path: Path,
    on_activity: Callable[[], None] | None = None,
    on_compaction_active: Callable[[bool], None] | None = None,
) -> Thread:
    thread = Thread(
        target=_tee_agent_stdout,
        args=(process, repo_root, log_path, on_activity, on_compaction_active),
        name=f"spice-agent-stdout-{process.pid}",
        daemon=True,
    )
    thread.start()
    return thread


def _tee_agent_stdout(
    process: subprocess.Popen[str],
    repo_root: Path,
    log_path: Path,
    on_activity: Callable[[], None] | None = None,
    on_compaction_active: Callable[[bool], None] | None = None,
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
            on_text_starvation=lambda count: publish_supervisor_feedback(
                repo_root,
                log_handle,
                "prose.starved",
                count=count,
                message=TEXT_STARVATION_NUDGE,
            ),
            on_activity=on_activity,
            on_compaction_active=on_compaction_active,
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
        publish_supervisor_feedback(
            repo_root,
            log_handle,
            "ack.error",
            keys=list(dict.fromkeys(extract_ack_keys_from_text(message_text))),
            error=str(exc),
        )
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
    if ack_summary.archived:
        try:
            _annotate_active_task_with_acks(
                repo_root, message_text, ack_summary.archived, log_handle
            )
        except Exception as exc:  # surface-and-survive: retirement already landed
            log_handle.write(f"spice ack annotate supervisor error: {exc}\n")
            log_handle.flush()


# Steering routinely amends the acceptance criteria or the very understanding
# of the task in flight; the retired acknowledgment lands on the active task
# record so that drift is durable and reviewable.
ACK_ANNOTATION_CONTENT_LIMIT = 500


def _annotate_active_task_with_acks(
    repo_root: Path,
    message_text: str,
    archived_keys: list[str],
    log_handle: TextIO,
) -> None:
    from spice.mail.ackgrammar import ack_content_by_key, extract_ack_segments_from_text
    from spice.mail.inbox import (
        AUTOMATED_GUIDANCE_PRIORITIES,
        inbox_item_key,
        parse_inbox_payload,
    )
    from spice.tasks import identity as task_identity
    from spice.tasks import claimstate, tw

    thread_id = _supervised_inline_task_actor(repo_root)
    if not thread_id:
        return
    claim = claimstate.active_claim(tw.canonical_actor(thread_id))
    if claim is None:
        log_handle.write(
            "spice ack annotate: no active claim; retired ack not mirrored\n"
        )
        log_handle.flush()
        return
    uuid = task_identity.uuid_of(claim)
    content_map = ack_content_by_key(extract_ack_segments_from_text(message_text))
    records = _acked_state_records_by_key(repo_root, archived_keys)
    for key in archived_keys:
        record = records.get(key)
        payload = parse_inbox_payload(record.text) if record is not None else None
        # The mirror captures operator steering only. Review feedback already
        # lives on the task (review_* UDAs and annotations) and maxim
        # reminders are ambient policy, not task-scoped amendments.
        if payload is not None and payload.priority in AUTOMATED_GUIDANCE_PRIORITIES:
            continue
        content = content_map.get(inbox_item_key(key), "")
        if not content:
            content = payload.body.strip() if payload is not None else ""
        content = " ".join(content.split())[:ACK_ANNOTATION_CONTENT_LIMIT]
        claimstate.annotate(uuid, f"ack {key}: {content or '(acknowledged)'}")


def _acked_state_records_by_key(
    repo_root: Path, archived_keys: list[str]
) -> dict[str, Any]:
    from spice.mail.ackstate import ack_state_records
    from spice.mail.inbox import inbox_item_key

    wanted = {inbox_item_key(key): key for key in archived_keys}
    found: dict[str, Any] = {}
    for record in ack_state_records(repo_root):
        key = wanted.get(inbox_item_key(record.key))
        if key is not None and key not in found:
            found[key] = record
    return found


def publish_supervisor_feedback(
    repo_root: Path, log_handle: TextIO, kind: str, **fields: object
) -> None:
    try:
        publish_side_channel_feedback(repo_root, kind, **fields)
    except Exception as exc:  # best-effort stderr feedback
        log_handle.write(f"spice side-channel feedback error: {exc}\n")
        log_handle.flush()


# The lane believes it is narrating, so name the symptom concretely: its last
# many assistant responses reached the wire with tool calls but zero text.
TEXT_STARVATION_NUDGE = (
    "your recent responses carried tool calls but NO text — your narration "
    "and ACK headers are not materializing as visible prose. Lead your next "
    "response with a short plain-text status line (ACKs first) before any "
    "tool call."
)


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
        default_origin=_inline_task_default_origin(message_text),
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


def _inline_task_default_origin(message_text: str) -> str | None:
    """The ack origin an inline TASK inherits from its own message.

    The capture idiom is `ACK <key>: ...` followed by a TASK line, so the
    acknowledged key IS the provenance of the captured work. Explicit
    origin= fields in the batch line win; a message that ACKs nothing
    provides no default and the batch's own origin requirement applies.
    """
    from spice.mail.ackgrammar import extract_ack_keys_from_text

    keys = list(extract_ack_keys_from_text(message_text))
    return f"ack:{keys[0]}" if keys else None


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
    on_text_starvation: Callable[[int], None] | None = None,
    on_activity: Callable[[], None] | None = None,
    on_compaction_active: Callable[[bool], None] | None = None,
) -> StdoutScanner:
    """Pick the scanner matching this worktree's driver's stdout format.

    `on_compaction_active` reaches the json scanner only: a marker stream names
    a compaction that already finished, and its driver already counts that
    marker as activity, so there is no in-flight phase for it to report.
    """
    if driver.stdout_format == "json":
        return JsonStdoutScanner(
            on_message,
            driver.normalize_transcript_line,
            on_compaction=on_compaction,
            on_text_starvation=on_text_starvation,
            on_activity=on_activity,
            on_compaction_active=on_compaction_active,
        )
    return AgentStdoutMessageScanner(
        driver,
        on_message,
        on_compaction=on_compaction,
        on_activity=on_activity,
    )


# Consecutive tool-calling assistant events with no text before the supervisor
# flags the lane as text-starved. Long turns have been observed to stop
# materializing prose entirely (thinking + tool_use only) while the agent
# believes it is narrating; ~12 tool calls of pure silence is far beyond the
# normal narrate-every-step cadence and cheap to nudge.
TEXT_STARVATION_THRESHOLD = 12


class JsonStdoutScanner:
    """Reassemble assistant messages from a stream-json `exec` stdout.

    Each stdout line is one transcript event; the injected normalizer turns an
    assistant-message line into canonical prose, which feeds ACK archiving and
    maxim judging exactly as the marker scanner's reassembled blocks do.

    The scanner also watches for text starvation: canonical assistant events
    that keep calling tools while emitting zero text blocks. Once the streak
    reaches `TEXT_STARVATION_THRESHOLD` the starvation callback fires (once per
    streak) so the supervisor can nudge the lane; any real text resets it.

    Compaction events report on their own callback rather than as activity: a
    compacting agent is alive but has produced nothing, so it must hold the
    startup deadline open without being mistaken for a ready lane.
    """

    def __init__(
        self,
        on_message: Callable[[str], None],
        normalize: Callable[[dict], dict | None],
        *,
        on_compaction: Callable[[], None] | None = None,
        on_text_starvation: Callable[[int], None] | None = None,
        on_activity: Callable[[], None] | None = None,
        on_compaction_active: Callable[[bool], None] | None = None,
    ) -> None:
        self.on_message = on_message
        self._normalize = normalize
        self._on_compaction = on_compaction or (lambda: None)
        self._on_text_starvation = on_text_starvation or (lambda _count: None)
        self._on_activity = on_activity or (lambda: None)
        self._on_compaction_active = on_compaction_active or (lambda _active: None)
        self._textless_streak = 0
        self._starvation_fired = False

    def process_line(self, line: str) -> None:
        try:
            raw = json.loads(line)
        except json.JSONDecodeError:
            return
        if not isinstance(raw, dict):
            return
        self._track_text_starvation(raw)
        event = self._normalize(raw)
        if event is None:
            return
        if event.get("type") == "compacting":
            payload = event.get("payload") or {}
            self._on_compaction_active(bool(payload.get("active")))
            return
        if event.get("type") == "compacted":
            self._on_compaction()
            # A boundary is the compaction's own completion notice, so the
            # startup deadline goes back to waiting on real first activity.
            self._on_compaction_active(False)
            return
        payload = event.get("payload") or {}
        if payload.get("role") == "assistant" or payload.get("type") in {
            "function_call",
            "tool_use",
        }:
            self._on_activity()
        if payload.get("type") != "message" or payload.get("role") != "assistant":
            return
        text = first_text(payload.get("content"))
        if text and text.strip():
            self.on_message(text.strip())

    def _track_text_starvation(self, raw: dict) -> None:
        if raw.get("type") != "assistant":
            return
        message = raw.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            return
        blocks = [item.get("type") for item in content if isinstance(item, dict)]
        text = first_text(content)
        if text and text.strip():
            self._textless_streak = 0
            self._starvation_fired = False
            return
        if "tool_use" not in blocks:
            return
        self._textless_streak += 1
        if self._starvation_fired:
            return
        if self._textless_streak >= TEXT_STARVATION_THRESHOLD:
            self._starvation_fired = True
            self._on_text_starvation(self._textless_streak)

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
        on_activity: Callable[[], None] | None = None,
    ) -> None:
        self._driver = driver
        self.on_message = on_message
        self._on_compaction = on_compaction or (lambda: None)
        self._on_activity = on_activity or (lambda: None)
        self._capturing = False
        self._message_lines: list[str] = []

    def process_line(self, line: str) -> None:
        marker = line.rstrip("\r\n")
        if marker == self._driver.stdout_assistant_marker:
            self._flush()
            self._on_activity()
            self._capturing = True
            return
        if marker in self._driver.stdout_section_markers:
            self._flush()
            if marker in self._driver.stdout_activity_markers:
                self._on_activity()
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
    driver = driver_for(repo_root)
    hits = triggered_maxims(
        [statement_text],
        repo_root=repo_root,
        driver_name=driver.name,
    )
    if not hits:
        return []
    reminder_key = _maxim_reminder_key(hits)
    _record_maxim_metrics(
        repo_root,
        hits,
        event_type=MAXIM_EVENT_FIRE,
        statement=statement_text,
    )
    if not reminder_gate.should_publish(reminder_key):
        _record_maxim_metrics(
            repo_root,
            hits,
            event_type=MAXIM_EVENT_GATE_SUPPRESSED,
            statement=statement_text,
            reminder_key=reminder_key,
            reminder_body=_maxim_inbox_body(hits),
        )
        return []
    violations = _publishable_maxim_hits(repo_root, hits, statement_text)
    if not violations:
        return []
    body = _maxim_inbox_body(violations)
    path = write_inbox_item(repo_root, None, body)
    reminder_gate.mark_sent(reminder_key, path, body)
    _record_maxim_metrics(
        repo_root,
        violations,
        event_type=MAXIM_EVENT_PUBLISHED,
        reminder_key=path.stem,
        reminder_body=body,
    )
    paths = [path]
    return paths


def _publishable_maxim_hits(
    repo_root: Path, hits: list[MaximBag], statement_text: str
) -> list[MaximBag]:
    """Return the triggered hits that should publish as reminders.

    Judge-free is the deterministic default: every gated trigger hit publishes
    directly, accepting more false positives and needing no judge subprocess.
    The request's shorthand that a default judge "always answers YES" is
    inverted against the protocol — YES means the statement AGREES with the
    maxim and is therefore not a violation (suppressed) — so direct publishing
    is modeled as no adjudication at all, never as an assumed YES verdict that
    would suppress every reminder. When an install opts into adjudication
    (``[judge] enabled``), each hit is confirmed by the local two-judge gate
    first: a YES verdict agrees and is dropped, a NO verdict disagrees and
    publishes.
    """
    if not maxim_adjudication_enabled(repo_root):
        return list(hits)
    violations: list[MaximBag] = []
    for hit in hits:
        verdict = evaluate_maxim_any_violation(hit.message, statement_text)
        if verdict.agrees:
            _record_maxim_metrics(
                repo_root,
                [hit],
                event_type=MAXIM_EVENT_JUDGED_REJECTED,
                statement=statement_text,
            )
            continue
        _record_maxim_metrics(
            repo_root,
            [hit],
            event_type=MAXIM_EVENT_JUDGED_CONFIRMED,
            statement=statement_text,
        )
        violations.append(hit)
    return violations


def _record_maxim_metrics(
    repo_root: Path,
    hits: list[MaximBag],
    *,
    event_type: str,
    statement: str = "",
    reminder_key: str = "",
    reminder_body: str = "",
) -> None:
    if not hits:
        return
    driver_name = driver_for(repo_root).name
    thread_id = ambient_thread_id() or ""
    record_maxim_metric_events(
        repo_root,
        [
            MaximMetricEventWrite(
                event_type=event_type,
                bag_name=hit.name,
                driver_name=driver_name,
                thread_id=thread_id,
                trigger_family=hit.name,
                statement=statement,
                reminder_key=reminder_key,
                reminder_body=reminder_body,
            )
            for hit in hits
        ],
    )


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


def _maxim_reminder_key(hits: list[MaximBag]) -> str:
    return json.dumps(
        [(hit.name, hit.message) for hit in hits],
        ensure_ascii=True,
        separators=(",", ":"),
    )


def _one_line_maxim(maxim: str) -> str:
    sentence = " ".join(maxim.split())
    return sentence if sentence.endswith((".", "!", "?")) else f"{sentence}."
