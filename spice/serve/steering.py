"""Operator steering submitted from the UI lands as ordinary inbox items."""

from __future__ import annotations

from dataclasses import dataclass
from http import HTTPStatus
from pathlib import Path
from typing import Any, Sequence

from spice.agent.renewal import strip_renewal_handoff_request_suffix
from spice.mail.attachments import InboxAttachment, prepare_inbox_attachments
from spice.mail.inbox import (
    compose_inbox_text,
    collect_inbox_attachments,
    default_inbox_name,
    inbox_item_key,
    inbox_request_body,
    inbox_request_controls,
    write_inbox_item,
)


@dataclass(frozen=True)
class SentSteeringMessage:
    key: str
    path: Path
    text: str
    request_text: str
    request_controls: tuple[str, ...]
    no_say: bool
    attachments: tuple[InboxAttachment, ...] = ()


def submit_steering_message(
    *,
    text: str,
    priority: str | None,
    stop: bool,
    no_say: bool = False,
    attachments: Any = None,
    controls: Sequence[str] = (),
    target_repo_root: Path | None,
) -> SentSteeringMessage:
    if target_repo_root is None:
        raise RuntimeError("No target worktree is selected.")
    name = default_inbox_name()
    body = text if text.endswith("\n") else f"{text}\n"
    composed = compose_inbox_text(
        body=body,
        priority=priority,
        stop=stop,
        controls=controls,
    )
    prepared_attachments = prepare_inbox_attachments(attachments)
    path = write_inbox_item(
        target_repo_root,
        name,
        composed,
        attachments=prepared_attachments,
        dedupe_pending_text=True,
    )
    key = inbox_item_key(path.name)
    return SentSteeringMessage(
        key=key,
        path=path,
        text=composed,
        request_text=strip_renewal_handoff_request_suffix(inbox_request_body(composed)),
        request_controls=inbox_request_controls(composed),
        no_say=no_say,
        attachments=collect_inbox_attachments(path, repo_root=target_repo_root),
    )


def steering_submit_error_status(exc: Exception) -> HTTPStatus:
    if isinstance(exc, ValueError):
        return HTTPStatus.BAD_REQUEST
    return HTTPStatus.INTERNAL_SERVER_ERROR
