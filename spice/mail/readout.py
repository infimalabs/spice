"""Pending inbox readout for wrapper and side-channel injection."""

from __future__ import annotations

import sys
from pathlib import Path
from typing import TextIO

from spice.mail import inbox


def print_inbox_readout(
    repo_root: Path | None,
    *,
    quiet: bool = False,
    displayed_keys: set[str] | None = None,
    file: TextIO | None = None,
) -> list[str]:
    """Print pending inbox steering; return the keys displayed.

    `displayed_keys` suppresses items already shown and is updated in place.
    The agent wrapper uses it for repeat-with-suppression injection.
    """
    from spice.agent.identity import ambient_thread_id
    from spice.agent.renewal import renewal_wind_down_rows

    out = file or sys.stdout
    items = inbox.collect_inbox_items(str(repo_root) if repo_root else None)

    # An item is rendered full the first time it is shown and recorded in
    # `displayed_keys`; while it stays pending inside its suppression window a
    # later readout (e.g. one triggered by a brand-new key) collapses it to a
    # single compact line instead of re-dumping its body. Keys absent from
    # `displayed_keys` always render full, so real-time delivery is preserved.
    def _is_summary(item: inbox.InboxItem) -> bool:
        return (
            displayed_keys is not None
            and inbox.inbox_item_key(item.name) in displayed_keys
        )

    shown_full = [
        inbox.inbox_item_key(item.name) for item in items if not _is_summary(item)
    ]
    renewal_rows = renewal_wind_down_rows(repo_root, thread_id=ambient_thread_id())
    if quiet and not items and not renewal_rows:
        return shown_full
    print("Inbox Steering", file=out)
    if not quiet:
        print(f"  {inbox.INBOX_DIRECT_STEERING_ROW}", file=out)
        print(f"  repo_root={repo_root or '-'}", file=out)
    for row in renewal_rows:
        print(f"  {row}", file=out)
    if renewal_rows:
        return shown_full
    if not items:
        print("  pending=none", file=out)
        return shown_full
    for item in items:
        if _is_summary(item):
            print(f"  {inbox.inbox_item_summary_row(item)}", file=out)
            continue
        if displayed_keys is not None:
            displayed_keys.add(inbox.inbox_item_key(item.name))
        for line in inbox.inbox_item_readout_rows(item):
            print(f"  {line}", file=out)
    print(f"  {inbox.INBOX_RESPONSE_ROW}", file=out)
    print(f"  {inbox.inbox_ack_format_hint_row(items)}", file=out)
    if inbox.inbox_items_need_task_hint(items):
        print(f"  {inbox.INBOX_TASK_HINT_ROW}", file=out)
    print(f"  {inbox.INBOX_PEEK_PERSISTENCE_ROW}", file=out)
    return shown_full
