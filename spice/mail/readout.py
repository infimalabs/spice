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
    if displayed_keys is not None:
        fresh = []
        for item in items:
            key = inbox.inbox_item_key(item.name)
            if key in displayed_keys:
                continue
            displayed_keys.add(key)
            fresh.append(item)
        items = fresh
    shown = [inbox.inbox_item_key(item.name) for item in items]
    renewal_rows = renewal_wind_down_rows(repo_root, thread_id=ambient_thread_id())
    if quiet and not items and not renewal_rows:
        return shown
    print("Inbox Steering", file=out)
    if not quiet:
        print(f"  {inbox.INBOX_DIRECT_STEERING_ROW}", file=out)
        print(f"  repo_root={repo_root or '-'}", file=out)
    for row in renewal_rows:
        print(f"  {row}", file=out)
    if renewal_rows:
        return shown
    if not items:
        print("  pending=none", file=out)
        return shown
    for item in items:
        for line in inbox.inbox_item_readout_rows(item):
            print(f"  {line}", file=out)
    print(f"  {inbox.INBOX_RESPONSE_ROW}", file=out)
    print(f"  {inbox.inbox_ack_format_hint_row(items)}", file=out)
    if inbox.inbox_items_need_task_hint(items):
        print(f"  {inbox.INBOX_TASK_HINT_ROW}", file=out)
    print(f"  {inbox.INBOX_PEEK_PERSISTENCE_ROW}", file=out)
    return shown
