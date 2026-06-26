"""The durable filesystem inbox: operator steering an agent must ACK.

Items live under `.spice/inbox/*.txt`, one file per message, named by a
UTC-microsecond timestamp key. Publish is atomic (tmp + fsync + hardlink +
directory fsync); collisions increment a suffix. Reads never clear items. ACK
is the only normal retirement path: ACKed items are recorded in
`spiceacks.sqlite3` with their text and durable attachment references, then
removed from pending input. Items older than 24 hours expire in place.
"""

from __future__ import annotations

import contextlib
import os
import re
import uuid
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Sequence
from urllib.parse import quote

from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    ack_state_database_path,
    ack_state_records,
)
from spice.mail.attachments import (
    InboxAttachment,
    InboxAttachmentInput,
    archive_inbox_attachments,
    attachment_text_path,
    collect_inbox_attachments,
    inbox_attachment_dir,
    remove_inbox_attachment_dir,
    shared_attachment_display_path,
    write_inbox_attachments,
)
from spice.locking import exclusive_lock
from spice.paths import STATE_DIRNAME, fsync_directory

INBOX_DIRNAME = "inbox"
INBOX_ARCHIVE_DIRNAME = "archive"
INBOX_DEADLETTER_DIRNAME = "deadletter"
INBOX_CREDIT_FAILURE_DEADLETTER_THRESHOLD = 1
INBOX_ARCHIVE_PREVIEW_LIMIT = 120
INBOX_ARCHIVE_DEFAULT_LIMIT = 6
INBOX_COLLISION_MAX = 1000
INBOX_PUBLISH_LOCK_NAME = ".publish.lock"
_PREVIEW_ELLIPSIS_CHARS = 3
SECONDS_PER_MINUTE = 60
INBOX_MAX_ITEM_AGE_SECONDS = 24 * 60 * 60
INBOX_DIRECT_STEERING_ROW = "Direct operator steering: read before planning."
INBOX_STEERING_ROW = "Inbox steering: read before planning; retire only after ACK."
INBOX_RESPONSE_ROW = (
    "Real-time ACK loop: put a plain-text ACK header near the start of each "
    "working assistant message: "
    "ACK <key> [<key> ...]: <what changed or was captured>; acknowledged "
    "keys clear once processed. Do not bury ACKs mid-message or save them for "
    "final response."
)
INBOX_ACK_REMINDER_SECONDS = 15
INBOX_ACK_ESCALATED_SECONDS = 60
INBOX_ACK_OVERDUE_SECONDS = 5 * 60
INBOX_TASK_HINT_ROW = (
    "Task offload: capture in the moment with a standalone TASK line: "
    "`TASK title=... | project=<stem.child> | acceptance=...`; if ACKing "
    "steering, put ACK prose first and then the TASK line on its own line. "
    "Use the same task-add batch format, or use spice task add; then resume "
    "allocator flow."
)
INBOX_PEEK_PERSISTENCE_ROW = (
    "Persistence: acknowledged keys clear once processed; unACKed keys "
    "redisplay after 15s; bare reads never clear. Do not bury ACKs mid-message "
    "or save them for final response."
)

# Trailing note that tells the receiver whether this message is routine
# continuation steering or a completion request.
INBOX_CONTINUE_NOTE = "CONTINUE COMPLETING ASKS"
INBOX_GRACEFUL_NOTE = "SEEK GRACEFUL COMPLETION"
INBOX_CONTROL_DRAIN_QUEUE = "drive-drain-queue"
INBOX_CONTROL_READOUT_ROWS = {
    INBOX_CONTROL_DRAIN_QUEUE: (
        "control=drive-drain-queue: DRAIN QUEUE ASAP: spice task next"
    ),
}
AUTOMATED_GUIDANCE_PRIORITIES = frozenset({"maxim", "review"})

PRIORITY_RANK = {
    "reminder": 0,
    "later": 1,
    "normal": 2,
    "urgent": 3,
    "critical": 4,
    "maxim": 5,
    "review": 5,
}


@dataclass(frozen=True)
class InboxItem:
    source_path: Path
    archive_path: Path
    name: str
    text: str
    attachments: tuple[InboxAttachment, ...] = ()
    disposition: str = ""


def inbox_dir(repo_root: Path | str) -> Path:
    return Path(repo_root) / STATE_DIRNAME / INBOX_DIRNAME


def collect_inbox_items(repo_root: str | Path | None) -> list[InboxItem]:
    if not repo_root:
        return []
    root = Path(repo_root)
    prune_stale_inbox_artifacts(repo_root)
    directory = inbox_dir(root)
    if not directory.is_dir():
        return []
    archive_dir = directory / INBOX_ARCHIVE_DIRNAME
    items: list[InboxItem] = []
    for path in sorted(_file_paths(directory), key=lambda item: item.name):
        if path.name.endswith(".tmp") or path.suffix != ".txt":
            continue
        try:
            text = path.read_text(errors="replace")
        except FileNotFoundError:
            continue
        items.append(
            InboxItem(
                source_path=path,
                archive_path=archive_dir / path.name,
                name=path.name,
                text=text,
                attachments=collect_inbox_attachments(path, repo_root=root),
            )
        )
    return items


def collect_acked_inbox_items(
    repo_root: str | Path | None, *, limit: int = INBOX_ARCHIVE_DEFAULT_LIMIT
) -> list[InboxItem]:
    """Return consumed operator steering from ACK state, not archive files."""
    return _collect_ack_state_inbox_items(
        repo_root, limit=limit, disposition=ACK_DISPOSITION_ACKED
    )


def collect_refused_inbox_items(
    repo_root: str | Path | None, *, limit: int = INBOX_ARCHIVE_DEFAULT_LIMIT
) -> list[InboxItem]:
    """Return refused operator steering from ACK state, not archive files."""
    return _collect_ack_state_inbox_items(
        repo_root, limit=limit, disposition=ACK_DISPOSITION_REFUSED
    )


def _collect_ack_state_inbox_items(
    repo_root: str | Path | None,
    *,
    limit: int,
    disposition: str | None,
) -> list[InboxItem]:
    if not repo_root:
        return []
    prune_stale_inbox_artifacts(repo_root)
    state_path = ack_state_database_path(repo_root)
    items = [
        InboxItem(
            source_path=state_path,
            archive_path=state_path,
            name=record.inbox_name,
            text=record.text,
            attachments=_ack_state_record_attachments(record),
            disposition=record.disposition,
        )
        for record in ack_state_records(repo_root)
        if disposition is None or record.disposition == disposition
    ]
    return items[: max(0, limit)]


def collect_deadlettered_inbox_items(
    repo_root: str | Path | None, *, limit: int = INBOX_ARCHIVE_DEFAULT_LIMIT
) -> list[InboxItem]:
    if not repo_root:
        return []
    prune_stale_inbox_artifacts(repo_root)
    return _collect_consumed_inbox_items(
        inbox_dir(repo_root) / INBOX_DEADLETTER_DIRNAME,
        repo_root=Path(repo_root),
        limit=limit,
    )


def _collect_consumed_inbox_items(
    directory: Path, *, repo_root: Path, limit: int
) -> list[InboxItem]:
    if not directory.is_dir():
        return []
    paths = sorted(
        (
            path
            for path in _file_paths(directory)
            if path.suffix == ".txt" and inbox_path_is_fresh(path)
        ),
        key=lambda path: (_path_mtime(path), path.name),
        reverse=True,
    )[: max(0, limit)]
    items: list[InboxItem] = []
    for path in paths:
        try:
            text = path.read_text(errors="replace")
        except FileNotFoundError:
            continue
        items.append(
            InboxItem(
                source_path=path,
                archive_path=path,
                name=path.name,
                text=text,
                attachments=collect_inbox_attachments(path, repo_root=repo_root),
            )
        )
    return items


def pending_inbox_count(repo_root: str | Path | None) -> int:
    if not repo_root:
        return 0
    prune_stale_inbox_artifacts(repo_root)
    directory = inbox_dir(repo_root)
    if not directory.is_dir():
        return 0
    return sum(
        1
        for path in _file_paths(directory)
        if path.name != INBOX_ARCHIVE_DIRNAME and path.suffix == ".txt"
    )


def pending_operator_inbox_items(repo_root: str | Path | None) -> list[InboxItem]:
    """Pending items that justify resurrecting an idle agent.

    Automated guidance (maxim and friends) is fully synthesized — it does not
    come from the operator — so it is informational at launch and must never
    start an off agent on its own. Only genuine operator steering resurrects an
    idle lane; this gates the respawn path so automated guidance cannot drive a
    restart storm on an agent that is out of credits or otherwise down.
    """
    if not repo_root:
        return []
    return [
        item
        for item in collect_inbox_items(repo_root)
        if not inbox_item_is_automated_guidance(item)
    ]


def inbox_payload_rows(
    items: Sequence[InboxItem],
    *,
    include_steering_row: bool = True,
    include_persistence_row: bool = False,
) -> list[str]:
    if not items:
        return []
    rows: list[str] = [INBOX_STEERING_ROW] if include_steering_row else []
    for item in items:
        rows.extend(inbox_item_readout_rows(item))
    rows.append(INBOX_RESPONSE_ROW)
    rows.append(inbox_ack_format_hint_row(items))
    if inbox_items_need_task_hint(items):
        rows.append(INBOX_TASK_HINT_ROW)
    if include_persistence_row:
        rows.append(INBOX_PEEK_PERSISTENCE_ROW)
    return rows


def inbox_items_need_task_hint(items: Sequence[InboxItem]) -> bool:
    return any(not inbox_item_is_automated_guidance(item) for item in items)


def inbox_item_is_automated_guidance(item: InboxItem) -> bool:
    return parse_inbox_payload(item.text).priority in AUTOMATED_GUIDANCE_PRIORITIES


def inbox_ack_format_hint_row(items: Sequence[InboxItem]) -> str:
    keys = " ".join(inbox_item_key(item.name) for item in items)
    example = f"ACK {keys}: <what changed or was captured>"
    age_seconds = max((_inbox_item_age_seconds(item) for item in items), default=0.0)
    if age_seconds >= INBOX_ACK_OVERDUE_SECONDS:
        return (
            "ACK required now: "
            f"pending for {format_relative_seconds(age_seconds)}; include an ACK "
            "header near the start of the next working assistant message, "
            f"e.g. `{example}`."
        )
    if age_seconds >= INBOX_ACK_ESCALATED_SECONDS:
        return (
            "ACK reminder: "
            f"pending for {format_relative_seconds(age_seconds)}; include an ACK "
            "header near the start of your next working assistant message, "
            f"e.g. `{example}`."
        )
    if age_seconds >= INBOX_ACK_REMINDER_SECONDS:
        return (
            "ACK hint: "
            "this will keep redisplaying until an assistant message includes "
            f"an ACK header near the start, like `{example}`."
        )
    return (
        "ACK example: lead the next working assistant message with a concise "
        f"ACK response, e.g. `{example}`."
    )


def _inbox_item_age_seconds(item: InboxItem) -> float:
    try:
        return inbox_path_age_seconds(item.source_path)
    except OSError:
        return 0.0


def inbox_item_readout_rows(item: InboxItem) -> list[str]:
    rows = [
        f"key={inbox_item_key(item.name)}: age={relative_time_for_path(item.source_path)}"
    ]
    payload = parse_inbox_payload(item.text)
    if payload.priority:
        rows.append(f"  priority={payload.priority}")
    rows.extend(
        f"  {inbox_control_readout_row(control)}" for control in payload.controls
    )
    rows.extend(f"  {line}" for line in (payload.body.splitlines() or [""]))
    rows.append(
        f"  note={INBOX_GRACEFUL_NOTE if payload.is_stop else INBOX_CONTINUE_NOTE}"
    )
    if item.attachments:
        rows.append(f"  attachments={len(item.attachments)}")
        for index, attachment in enumerate(item.attachments, start=1):
            target = quote(
                inbox_attachment_readout_path(item, attachment).as_posix(),
                safe="/:",
            )
            rows.append(
                f"  attachment {index}: [{attachment.name}]({target}) "
                f"({attachment.content_type}, {attachment.size} bytes)"
            )
    return rows


def inbox_item_summary_row(item: InboxItem) -> str:
    """One compact line for an item whose full body was already shown.

    Used when a fresh readout is triggered (e.g. a new key arrived) while this
    item is still inside its repeat-suppression window: the key stays visible
    and ACKable without re-dumping its full body into the agent's context.
    """
    return (
        f"key={inbox_item_key(item.name)}: "
        f"age={relative_time_for_path(item.source_path)} "
        "(shown earlier; ACK to clear)"
    )


def inbox_attachment_readout_path(item: InboxItem, attachment: InboxAttachment) -> Path:
    repo_root = _repo_root_for_inbox_path(item.source_path)
    if repo_root is not None:
        display_path = shared_attachment_display_path(
            attachment.path, repo_root=repo_root
        )
        if display_path is not None:
            return display_path
    return inbox_attachment_dir(item.archive_path) / attachment.path.name


def _repo_root_for_inbox_path(path: Path) -> Path | None:
    for parent in path.parents:
        if parent.name == STATE_DIRNAME:
            return parent.parent
    return None


def inbox_ack_state_context_rows(items: Sequence[InboxItem]) -> list[str]:
    if not items:
        return []
    rows = ["source=ack_state; status=already_consumed_operator_steering; store=sqlite"]
    for item in items:
        payload = parse_inbox_payload(item.text)
        text = one_line_preview(payload.body, limit=INBOX_ARCHIVE_PREVIEW_LIMIT)
        priority = f" priority={payload.priority}" if payload.priority else ""
        attachments = (
            f" attachments={len(item.attachments)}" if item.attachments else ""
        )
        label = (
            "refused_inbox"
            if item.disposition == ACK_DISPOSITION_REFUSED
            else "acked_inbox"
        )
        rows.append(
            f"{label} key={inbox_item_key(item.name)} "
            f"age={relative_time_for_path(item.source_path)}{priority}"
            f"{attachments} text={text or '-'}"
        )
    return rows


def _ack_state_record_attachments(record: Any) -> tuple[InboxAttachment, ...]:
    attachments: list[InboxAttachment] = []
    for item in record.attachments:
        path = Path(str(item.get("path") or ""))
        if not path.is_file():
            continue
        try:
            size = int(item.get("size") or path.stat().st_size)
        except (OSError, TypeError, ValueError):
            try:
                size = path.stat().st_size
            except OSError:
                continue
        attachments.append(
            InboxAttachment(
                path=path,
                name=str(item.get("name") or path.name),
                content_type=str(item.get("content_type") or "image/*"),
                size=size,
            )
        )
    return tuple(attachments)


def inbox_deadletter_context_rows(items: Sequence[InboxItem]) -> list[str]:
    if not items:
        return []
    rows = [
        "source=inbox_deadletter; status=parked_operator_steering; "
        "requeue=spice agent requeue-deadletter <key>"
    ]
    for item in items:
        payload = parse_inbox_payload(item.text)
        text = one_line_preview(payload.body, limit=INBOX_ARCHIVE_PREVIEW_LIMIT)
        priority = f" priority={payload.priority}" if payload.priority else ""
        attachments = (
            f" attachments={len(item.attachments)}" if item.attachments else ""
        )
        rows.append(
            f"deadlettered_inbox key={inbox_item_key(item.name)} "
            f"age={relative_time_for_path(item.source_path)}{priority}"
            f"{attachments} text={text or '-'}"
        )
    return rows


def inbox_item_key(name: str) -> str:
    path = Path(name)
    return path.stem or path.name


def inbox_item_key_aliases(name: str) -> set[str]:
    # Keys are UTC `…Z`; agents transcribing an ACK sometimes drop the `Z`, so
    # the stem without it is an accepted alias.
    key = inbox_item_key(name)
    aliases = {key}
    if key.endswith("Z"):
        aliases.add(key[:-1])
    return aliases


def inbox_payload_items(items: Sequence[InboxItem]) -> list[dict[str, str]]:
    return [
        {
            "source_path": str(item.source_path),
            "archive_dir": str(item.archive_path.parent),
            "attachment_source_dir": str(inbox_attachment_dir(item.source_path)),
            "attachment_archive_dir": str(inbox_attachment_dir(item.archive_path)),
        }
        for item in items
    ]


def consume_inbox_items(items: Sequence[dict[str, Any]]) -> None:
    for item in items:
        source = Path(str(item.get("source_path") or ""))
        archive_dir = Path(str(item.get("archive_dir") or ""))
        if not archive_dir.name and item.get("archive_path"):
            archive_dir = Path(str(item.get("archive_path"))).parent
        if not archive_dir.name:
            continue
        try:
            source_bytes = source.read_bytes()
        except FileNotFoundError:
            continue
        archive = archive_dir / source.name
        archive_dir.mkdir(parents=True, exist_ok=True)
        try:
            with archive.open("xb") as handle:
                handle.write(source_bytes)
        except FileExistsError:
            # Inbox items are operator steering already shown to the agent. Once
            # the archive name exists, the pending copy is stale and must not
            # alter the wrapped command outcome.
            pass
        with contextlib.suppress(FileNotFoundError):
            source.unlink()
        archive_inbox_attachments(source, archive)


def discard_inbox_items(items: Sequence[dict[str, Any]]) -> None:
    for item in items:
        source = Path(str(item.get("source_path") or ""))
        with contextlib.suppress(FileNotFoundError):
            source.unlink()
        remove_inbox_attachment_dir(inbox_attachment_dir(source))


def deadletter_inbox_item(
    repo_root: str | Path | None,
    inbox_key: str,
) -> str | None:
    if not repo_root or not inbox_key:
        return None
    wanted = inbox_item_key_aliases(inbox_key)
    for item in collect_inbox_items(repo_root):
        if not (inbox_item_key_aliases(item.name) & wanted):
            continue
        consume_inbox_items(
            [
                {
                    "source_path": str(item.source_path),
                    "archive_dir": str(inbox_dir(repo_root) / INBOX_DEADLETTER_DIRNAME),
                    "attachment_source_dir": str(
                        inbox_attachment_dir(item.source_path)
                    ),
                    "attachment_archive_dir": str(
                        inbox_attachment_dir(
                            inbox_dir(repo_root) / INBOX_DEADLETTER_DIRNAME / item.name
                        )
                    ),
                }
            ]
        )
        notify_inbox_changed(Path(repo_root))
        return inbox_item_key(item.name)
    return None


def requeue_deadlettered_inbox_item(
    repo_root: str | Path | None,
    inbox_key: str,
) -> Path | None:
    if not repo_root or not inbox_key:
        return None
    wanted = inbox_item_key_aliases(inbox_key)
    for item in collect_deadlettered_inbox_items(repo_root, limit=INBOX_COLLISION_MAX):
        if not (inbox_item_key_aliases(item.name) & wanted):
            continue
        attachments: list[InboxAttachmentInput] = []
        for attachment in item.attachments:
            try:
                data = attachment.path.read_bytes()
            except OSError:
                continue
            attachments.append(
                InboxAttachmentInput(
                    name=attachment.name,
                    content_type=attachment.content_type,
                    data=data,
                )
            )
        written = write_inbox_item(
            Path(repo_root),
            item.name,
            item.text,
            attachments=attachments,
        )
        with contextlib.suppress(FileNotFoundError):
            item.source_path.unlink()
        remove_inbox_attachment_dir(inbox_attachment_dir(item.source_path))
        return written
    return None


def write_inbox_item(
    repo_root: Path | None,
    name: str | None,
    text: str,
    *,
    attachments: Sequence[InboxAttachmentInput] = (),
    dedupe_pending_text: bool = False,
) -> Path:
    if repo_root is None:
        raise RuntimeError("Unable to resolve git repo root for inbox send")
    target_name = name or default_inbox_name()
    if not valid_inbox_name(target_name):
        raise RuntimeError("Inbox item name must be a direct child name, not a path")
    directory = inbox_dir(repo_root)
    directory.mkdir(parents=True, exist_ok=True)
    tmp_path = directory / f"{target_name}.{os.getpid()}.{uuid.uuid4().hex}.tmp"
    try:
        with exclusive_lock(directory / INBOX_PUBLISH_LOCK_NAME):
            if dedupe_pending_text and not attachments:
                existing_path = _pending_inbox_path_with_text(directory, text)
                if existing_path is not None:
                    return existing_path
            with tmp_path.open("w", encoding="utf-8") as handle:
                handle.write(text)
                handle.flush()
                os.fsync(handle.fileno())
            target_path = _atomic_publish_inbox_item(tmp_path, directory / target_name)
            write_inbox_attachments(target_path, attachments, repo_root=repo_root)
            notify_inbox_changed(repo_root)
    finally:
        with contextlib.suppress(FileNotFoundError):
            tmp_path.unlink()
    return target_path


def _pending_inbox_path_with_text(directory: Path, text: str) -> Path | None:
    for path in sorted(_file_paths(directory), key=lambda item: item.name):
        if path.suffix != ".txt":
            continue
        if inbox_attachment_dir(path).is_dir():
            continue
        try:
            if path.read_text(encoding="utf-8", errors="replace") == text:
                return path
        except OSError:
            continue
    return None


def notify_inbox_changed(repo_root: Path | None) -> None:
    from spice.agent.sidechannelnotify import notify_agent_side_channel

    notify_agent_side_channel(repo_root)


def resend_inbox_item(
    repo_root: Path,
    *,
    original_key: str,
    original_text: str,
    attempt: int,
    messages_elapsed: int,
) -> Path:
    """Re-publish an unACK'd send as a fresh inbox item.

    Resurrecting the original bytes under the original key lets an agent that
    already ignored them keep ignoring them. Instead this re-parses the
    previous payload, escalates the priority (`urgent` on the first resend,
    `critical` thereafter), preserves the stop-signal note, and writes a
    brand-new item with a fresh timestamp key.
    """
    del messages_elapsed
    parsed = parse_inbox_payload(original_text)
    new_priority = _escalate_resend_priority(parsed.priority, attempt=attempt)
    composed = compose_inbox_text(
        body=parsed.body,
        priority=new_priority,
        stop=parsed.is_stop,
        controls=parsed.controls,
    )
    original_path = inbox_dir(repo_root) / f"{original_key}.txt"
    original_attachments: list[InboxAttachmentInput] = []
    for attachment in collect_inbox_attachments(original_path, repo_root=repo_root):
        try:
            data = attachment.path.read_bytes()
        except OSError:
            continue
        original_attachments.append(
            InboxAttachmentInput(
                name=attachment.name,
                content_type=attachment.content_type,
                data=data,
            )
        )
    return write_inbox_item(repo_root, None, composed, attachments=original_attachments)


@dataclass(frozen=True)
class InboxPayload:
    priority: str | None
    body: str
    is_stop: bool
    controls: tuple[str, ...] = ()


_PRIORITY_PREFIX_RE = re.compile(r"^\[(?P<priority>[A-Z]+)\]\s+")
_STOP_SUFFIX_RE = re.compile(r"\s+\((?P<note>[^()]+)\)\s*$")
_PRIORITY_HEADER_RE = re.compile(r"^Priority:\s*(?P<priority>[A-Za-z]+)\s*$")
_CONTROL_HEADER_RE = re.compile(r"^Control:\s*(?P<control>[A-Za-z0-9_.:-]+)\s*$")
_NOTE_TRAILER_RE = re.compile(r"^Note:\s*(?P<note>.+?)\s*$")


def parse_inbox_payload(text: str) -> InboxPayload:
    """Reverse of :func:`compose_inbox_text` for an inbox payload."""
    candidate = text.strip()
    priority: str | None = None
    controls: list[str] = []
    is_stop = False
    lines = candidate.splitlines()
    if lines:
        note_match = _NOTE_TRAILER_RE.match(lines[-1].strip())
        if note_match:
            note = note_match.group("note").strip()
            if note in {INBOX_CONTINUE_NOTE, INBOX_GRACEFUL_NOTE}:
                is_stop = note == INBOX_GRACEFUL_NOTE
                lines = lines[:-1]
                candidate = "\n".join(lines).strip()
    if lines:
        priority_match = _PRIORITY_HEADER_RE.match(lines[0].strip())
        if priority_match:
            parsed_priority = priority_match.group("priority").lower()
            if parsed_priority in PRIORITY_RANK:
                priority = parsed_priority
                lines = lines[1:]
                candidate = "\n".join(lines).strip()
    while lines:
        control_match = _CONTROL_HEADER_RE.match(lines[0].strip())
        if not control_match:
            break
        control = control_match.group("control").strip()
        if control not in INBOX_CONTROL_READOUT_ROWS:
            break
        controls.append(control)
        lines = lines[1:]
        candidate = "\n".join(lines).strip()
    priority_match = _PRIORITY_PREFIX_RE.match(candidate)
    if priority is None and priority_match:
        parsed_priority = priority_match.group("priority").lower()
        if parsed_priority in PRIORITY_RANK:
            priority = parsed_priority
            candidate = candidate[priority_match.end() :]
    suffix_match = _STOP_SUFFIX_RE.search(candidate)
    if suffix_match:
        note = suffix_match.group("note").strip()
        if note in {INBOX_CONTINUE_NOTE, INBOX_GRACEFUL_NOTE}:
            is_stop = note == INBOX_GRACEFUL_NOTE
            candidate = candidate[: suffix_match.start()]
    return InboxPayload(
        priority=priority,
        body=candidate.strip(),
        is_stop=is_stop,
        controls=tuple(controls),
    )


def inbox_request_body(text: str) -> str:
    return parse_inbox_payload(text).body


def inbox_request_priority(text: str) -> str | None:
    return parse_inbox_payload(text).priority


def inbox_request_controls(text: str) -> tuple[str, ...]:
    return parse_inbox_payload(text).controls


def normalize_inbox_controls(controls: Sequence[str] = ()) -> tuple[str, ...]:
    normalized: list[str] = []
    seen: set[str] = set()
    for control in controls:
        value = str(control or "").strip()
        if value not in INBOX_CONTROL_READOUT_ROWS:
            raise ValueError(f"unknown inbox control: {value or '-'}")
        if value in seen:
            continue
        normalized.append(value)
        seen.add(value)
    return tuple(normalized)


def inbox_control_readout_row(control: str) -> str:
    return INBOX_CONTROL_READOUT_ROWS[control]


def _escalate_resend_priority(current: str | None, *, attempt: int) -> str:
    if attempt >= 2:
        return "critical"
    if current and PRIORITY_RANK.get(current, 0) > PRIORITY_RANK["urgent"]:
        return current
    return "urgent"


def _atomic_publish_inbox_item(tmp_path: Path, target_path: Path) -> Path:
    candidate = target_path
    for index in range(1, INBOX_COLLISION_MAX):
        try:
            os.link(tmp_path, candidate)
            fsync_directory(candidate.parent)
            return candidate
        except FileExistsError:
            candidate = _inbox_collision_path(target_path, index + 1)
    raise RuntimeError(f"Unable to allocate inbox item path for {target_path}")


def _inbox_collision_path(target_path: Path, index: int) -> Path:
    stem = target_path.stem
    suffix = target_path.suffix
    parts = stem.split(".")
    if len(parts) > 1:
        name = f"{parts[0]}-{index}.{'.'.join(parts[1:])}{suffix}"
    else:
        name = f"{stem}-{index}{suffix}"
    return target_path.with_name(name)


def format_relative_seconds(seconds: float) -> str:
    total = max(0, int(seconds))
    if total < SECONDS_PER_MINUTE:
        return f"{total}s ago"
    minutes, _ = divmod(total, SECONDS_PER_MINUTE)
    if minutes < SECONDS_PER_MINUTE:
        return f"{minutes}m ago"
    hours, minute = divmod(minutes, SECONDS_PER_MINUTE)
    if minute:
        return f"{hours}h{minute:02d}m ago"
    return f"{hours}h ago"


def relative_time_for_path(path: Path) -> str:
    try:
        return format_relative_seconds(inbox_path_age_seconds(path))
    except OSError:
        return "unknown"


def inbox_path_age_seconds(path: Path) -> float:
    return datetime.now().astimezone().timestamp() - path.stat().st_mtime


def _path_mtime(path: Path) -> float:
    try:
        return path.stat().st_mtime
    except OSError:
        return 0.0


def inbox_path_is_fresh(path: Path) -> bool:
    try:
        return inbox_path_age_seconds(path) <= INBOX_MAX_ITEM_AGE_SECONDS
    except OSError:
        return False


def prune_stale_inbox_artifacts(repo_root: str | Path | None) -> None:
    if not repo_root:
        return
    directory = inbox_dir(repo_root)
    for candidate in (
        directory,
        directory / INBOX_ARCHIVE_DIRNAME,
        directory / INBOX_DEADLETTER_DIRNAME,
    ):
        if not candidate.is_dir():
            continue
        for path in _file_paths(candidate):
            if path.name == INBOX_PUBLISH_LOCK_NAME:
                continue
            if not inbox_path_is_fresh(path):
                with contextlib.suppress(FileNotFoundError):
                    path.unlink()
        for path in _attachment_dirs(candidate):
            text_path = attachment_text_path(path)
            if not text_path.is_file() or not inbox_path_is_fresh(text_path):
                remove_inbox_attachment_dir(path)


def _file_paths(directory: Path) -> list[Path]:
    """Return file paths with the scandir handle closed before callers inspect them."""
    paths: list[Path] = []
    with os.scandir(directory) as entries:
        for entry in entries:
            try:
                if entry.is_file():
                    paths.append(Path(entry.path))
            except OSError:
                continue
    return paths


def _attachment_dirs(directory: Path) -> list[Path]:
    paths: list[Path] = []
    with os.scandir(directory) as entries:
        for entry in entries:
            try:
                if entry.is_dir() and entry.name.endswith(".attachments"):
                    paths.append(Path(entry.path))
            except OSError:
                continue
    return paths


def one_line_preview(text: str, *, limit: int = INBOX_ARCHIVE_PREVIEW_LIMIT) -> str:
    # Archive/readout rows stay compact even when the stored request body keeps
    # operator-authored internal line breaks.
    preview = " ".join(text.split())
    if len(preview) <= limit:
        return preview
    return f"{preview[: max(0, limit - _PREVIEW_ELLIPSIS_CHARS)]}..."


def compose_inbox_text(
    *,
    body: str,
    priority: str | None,
    stop: bool,
    controls: Sequence[str] = (),
) -> str:
    """Render the canonical inbox payload.

    Shape: ``Priority: urgent\\nControl: control-name\\nbody\\nNote: stop-signal-note\\n``

    * ``Priority:`` is emitted only when set and not ``normal``, so receivers
      see urgency at a glance without parsing.
    * ``Control:`` rows carry host/supervisor instructions outside the
      operator-authored body.
    * The body keeps operator-authored internal line breaks so ACK quote
      context preserves its visible structure.
    * The trailing ``Note:`` line is always present — either
      :data:`INBOX_CONTINUE_NOTE` or :data:`INBOX_GRACEFUL_NOTE` — so the
      receiver always has an unambiguous stop-signal answer.
    """
    request_body = (body or "").strip()
    lines: list[str] = []
    if priority and priority != "normal":
        lines.append(f"Priority: {priority}")
    for control in normalize_inbox_controls(controls):
        lines.append(f"Control: {control}")
    if request_body:
        lines.append(request_body)
    note = INBOX_GRACEFUL_NOTE if stop else INBOX_CONTINUE_NOTE
    lines.append(f"Note: {note}")
    return "\n".join(lines) + "\n"


def default_inbox_name() -> str:
    return f"{inbox_timestamp()}.txt"


def inbox_timestamp() -> str:
    return datetime.now(UTC).strftime("%Y%m%dT%H%M%S%fZ")


def valid_inbox_name(name: str) -> bool:
    path = Path(name)
    if not name or name in {".", "..", INBOX_ARCHIVE_DIRNAME, INBOX_DEADLETTER_DIRNAME}:
        return False
    return path.name == name
