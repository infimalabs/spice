"""Synthetic canonical directive facts for Serve projection tests."""

from __future__ import annotations

import time
from pathlib import Path

from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_PENDING,
    AckStateWrite,
    DirectivePublicationWrite,
    directive_history_records_from_database,
    record_acked_inbox_items_to_database,
    record_directive_publications_to_database,
)


def publish_directive_fact(
    path: str | Path,
    directive_key: str,
    *,
    agent_id: str,
    team_id: str,
    sent_at: float | None = None,
    text: str | None = None,
) -> None:
    body = directive_key if text is None else text
    record_directive_publications_to_database(
        path,
        [
            DirectivePublicationWrite(
                key=directive_key,
                inbox_name=f"{directive_key}.txt",
                text=body,
                target_actor=agent_id,
                team_id=team_id,
                sent_at=time.time() if sent_at is None else sent_at,
            )
        ],
    )


def complete_directive_fact(
    path: str | Path,
    directive_key: str,
    *,
    acked_at: float | None = None,
    disposition: str = ACK_DISPOSITION_ACKED,
    ack_text: str = "",
    ack_content: str = "",
) -> bool:
    record = next(
        (
            item
            for item in directive_history_records_from_database(path)
            if item.key == directive_key
        ),
        None,
    )
    if record is None or record.disposition != ACK_DISPOSITION_PENDING:
        return False
    recorded_ack_text = ack_text or f"ACK {directive_key}: replayed canonical ACK"
    recorded_ack_content = ack_content or "replayed canonical ACK"
    record_acked_inbox_items_to_database(
        path,
        [
            AckStateWrite(
                key=record.key,
                inbox_name=record.inbox_name,
                text=record.text,
                attachments=record.attachments,
                lineage=record.lineage,
                ack_text=recorded_ack_text,
                ack_content=recorded_ack_content,
                disposition=disposition,
            )
        ],
        now=time.time() if acked_at is None else acked_at,
    )
    return True
