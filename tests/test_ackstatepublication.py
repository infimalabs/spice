"""Canonical directive publication joins against durable ACK state."""

import json

import pytest

from spice.errors import SpiceError
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    DIRECTIVE_PROVENANCE_ARCHIVE_ONLY,
    AckStateWrite,
    DirectivePublicationWrite,
    directive_history_records_from_database,
    record_acked_inbox_items_to_database,
    record_directive_publications_to_database,
)
from spice.sqliteconnection import sqlite_connection


KEY = "1kArchiveFirst"
INBOX_NAME = f"{KEY}.txt"
TEXT = "inspect the attached artifact"
ACK_TEXT = f"ACK {KEY}: inspected the artifact"
ACK_CONTENT = "inspected the artifact"
FIRST_ACKNOWLEDGED_AT = 20.0
RETRY_ACKNOWLEDGED_AT = 30.0
ATTACHMENT = {
    "path": "/shared/attachment.png",
    "name": "attachment.png",
    "content_type": "image/png",
    "size": 123,
}


def _archive_first(path, *, attachment=ATTACHMENT) -> None:
    record_acked_inbox_items_to_database(
        path,
        [
            AckStateWrite(
                key=KEY,
                inbox_name=INBOX_NAME,
                text=TEXT,
                attachments=(attachment,),
                ack_text=ACK_TEXT,
                ack_content=ACK_CONTENT,
            )
        ],
        now=FIRST_ACKNOWLEDGED_AT,
    )


def _publication(*, attachment=ATTACHMENT) -> DirectivePublicationWrite:
    return DirectivePublicationWrite(
        key=KEY,
        inbox_name=INBOX_NAME,
        text=TEXT,
        target_actor="thread:actor-a",
        team_id="team-a",
        sent_at=10.0,
        attachments=(attachment,),
    )


def test_archive_first_matching_attachments_join_publication_provenance(tmp_path):
    path = tmp_path / "acks.sqlite3"
    _archive_first(path)
    equivalent_attachment = {
        "size": 123,
        "content_type": "image/png",
        "name": "attachment.png",
        "path": "/shared/attachment.png",
    }

    record_directive_publications_to_database(
        path, [_publication(attachment=equivalent_attachment)]
    )

    record = directive_history_records_from_database(path)[0]
    assert (
        record.key,
        record.target_actor,
        record.team_id,
        record.sent_at,
        record.disposition,
        record.acknowledged_at,
    ) == (KEY, "thread:actor-a", "team-a", 10.0, ACK_DISPOSITION_ACKED, 20.0)
    assert record.attachments == (ATTACHMENT,)
    assert record.ack_text == ACK_TEXT
    assert record.ack_content == ACK_CONTENT


def test_archive_first_attachment_collision_leaves_archive_unmodified(tmp_path):
    path = tmp_path / "acks.sqlite3"
    _archive_first(path)
    mismatched_attachment = {**ATTACHMENT, "name": "different.png"}

    with pytest.raises(
        SpiceError,
        match="archived steering content or attachments do not match",
    ):
        record_directive_publications_to_database(
            path, [_publication(attachment=mismatched_attachment)]
        )

    with sqlite_connection(path) as connection:
        row = connection.execute(
            "SELECT attachments_json, target_actor, team_id, sent_at, provenance "
            "FROM acked_inbox_items WHERE key = ?",
            (KEY,),
        ).fetchone()
    attachments_json, target_actor, team_id, sent_at, provenance = row
    assert json.loads(str(attachments_json)) == [ATTACHMENT]
    assert (target_actor, team_id, sent_at, provenance) == (
        "",
        "",
        None,
        DIRECTIVE_PROVENANCE_ARCHIVE_ONLY,
    )


def test_publication_first_duplicate_and_ack_completion_remain_idempotent(tmp_path):
    path = tmp_path / "acks.sqlite3"
    publication = _publication()
    record_directive_publications_to_database(path, [publication])
    record_directive_publications_to_database(path, [publication])
    acknowledgement = AckStateWrite(
        key=KEY,
        inbox_name=INBOX_NAME,
        text=TEXT,
        attachments=(ATTACHMENT,),
        ack_text=ACK_TEXT,
        ack_content=ACK_CONTENT,
    )
    record_acked_inbox_items_to_database(
        path, [acknowledgement], now=FIRST_ACKNOWLEDGED_AT
    )
    record_acked_inbox_items_to_database(
        path, [acknowledgement], now=RETRY_ACKNOWLEDGED_AT
    )

    record = directive_history_records_from_database(path)[0]
    assert (record.disposition, record.acknowledged_at) == (
        ACK_DISPOSITION_ACKED,
        FIRST_ACKNOWLEDGED_AT,
    )


def test_same_delivery_retry_keeps_first_auditable_ack(tmp_path):
    path = tmp_path / "acks.sqlite3"
    record_directive_publications_to_database(path, [_publication()])
    first = AckStateWrite(
        key=KEY,
        inbox_name=INBOX_NAME,
        text=TEXT,
        attachments=(ATTACHMENT,),
        ack_text=ACK_TEXT,
        ack_content=ACK_CONTENT,
    )
    retry = AckStateWrite(
        key=KEY,
        inbox_name=INBOX_NAME,
        text=TEXT,
        attachments=(ATTACHMENT,),
        ack_text=f"ACK {KEY}: retry after restart",
        ack_content="retry after restart",
    )

    record_acked_inbox_items_to_database(path, [first], now=FIRST_ACKNOWLEDGED_AT)
    record_acked_inbox_items_to_database(path, [retry], now=RETRY_ACKNOWLEDGED_AT)

    record = directive_history_records_from_database(path)[0]
    assert record.ack_text == ACK_TEXT
    assert record.ack_content == ACK_CONTENT
    assert record.acknowledged_at == FIRST_ACKNOWLEDGED_AT


def test_consumed_key_still_refuses_different_delivery(tmp_path):
    path = tmp_path / "acks.sqlite3"
    record_directive_publications_to_database(path, [_publication()])
    first = AckStateWrite(
        key=KEY,
        inbox_name=INBOX_NAME,
        text=TEXT,
        attachments=(ATTACHMENT,),
        ack_text=ACK_TEXT,
        ack_content=ACK_CONTENT,
    )
    record_acked_inbox_items_to_database(path, [first], now=FIRST_ACKNOWLEDGED_AT)

    with pytest.raises(SpiceError, match="directive ACK collision"):
        record_acked_inbox_items_to_database(
            path,
            [
                AckStateWrite(
                    key=KEY,
                    inbox_name=INBOX_NAME,
                    text="different operator directive",
                    attachments=(ATTACHMENT,),
                    ack_text=f"ACK {KEY}: unrelated",
                    ack_content="unrelated",
                )
            ],
            now=RETRY_ACKNOWLEDGED_AT,
        )

    record = directive_history_records_from_database(path)[0]
    assert record.text == TEXT
    assert record.ack_text == ACK_TEXT


def test_same_delivery_still_refuses_opposite_disposition(tmp_path):
    path = tmp_path / "acks.sqlite3"
    record_directive_publications_to_database(path, [_publication()])
    first = AckStateWrite(
        key=KEY,
        inbox_name=INBOX_NAME,
        text=TEXT,
        attachments=(ATTACHMENT,),
        ack_text=ACK_TEXT,
        ack_content=ACK_CONTENT,
    )
    record_acked_inbox_items_to_database(path, [first], now=FIRST_ACKNOWLEDGED_AT)

    with pytest.raises(SpiceError, match="directive ACK collision"):
        record_acked_inbox_items_to_database(
            path,
            [
                AckStateWrite(
                    key=KEY,
                    inbox_name=INBOX_NAME,
                    text=TEXT,
                    attachments=(ATTACHMENT,),
                    ack_text=f"NACK {KEY}: cannot comply",
                    ack_content="cannot comply",
                    disposition=ACK_DISPOSITION_REFUSED,
                )
            ],
            now=RETRY_ACKNOWLEDGED_AT,
        )

    record = directive_history_records_from_database(path)[0]
    assert record.disposition == ACK_DISPOSITION_ACKED
    assert record.ack_text == ACK_TEXT
