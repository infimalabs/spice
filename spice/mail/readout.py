"""Pending inbox readout for wrapper and side-channel injection."""

from __future__ import annotations

from pathlib import Path

from spice.mail import inbox


def print_inbox_readout(
    repo_root: Path | None,
    *,
    quiet: bool = False,
    displayed_keys: set[str] | None = None,
) -> list[str]:
    """Print pending inbox steering; return the keys displayed.

    `displayed_keys` suppresses items already shown and is updated in place.
    The agent wrapper uses it for repeat-with-suppression injection.
    """
    from spice.agent.identity import ambient_thread_id
    from spice.agent.renewal import renewal_wind_down_rows

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
    print("Inbox Steering")
    if not quiet:
        print(f"  {inbox.INBOX_DIRECT_STEERING_ROW}")
        print(f"  repo_root={repo_root or '-'}")
    for row in renewal_rows:
        print(f"  {row}")
    if renewal_rows:
        return shown
    if not items:
        print("  pending=none")
        return shown
    for item in items:
        for line in inbox.inbox_item_readout_rows(item):
            print(f"  {line}")
    print(f"  {inbox.INBOX_RESPONSE_ROW}")
    print(f"  {inbox.inbox_ack_format_hint_row(items)}")
    if inbox.inbox_items_need_task_hint(items):
        print(f"  {inbox.INBOX_TASK_HINT_ROW}")
    print(f"  {inbox.INBOX_PEEK_PERSISTENCE_ROW}")
    return shown
