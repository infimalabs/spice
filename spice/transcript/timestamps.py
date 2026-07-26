"""One reading of a timestamp for every dialect, record, and consumer.

Both dialects stamp lines with ISO-8601 text carrying a ``Z`` or numeric offset,
with or without fractional seconds, and consumers want either the instant or the
canonical string.  Reading that text lives here once, so a serve ordering
comparison and a rendered sessions timeline cannot disagree about what a line's
timestamp means.  Unreadable text is absent rather than fatal: a malformed
timestamp on one line must not end a scan over the rest of the transcript.

Spice's own records — artifact ledger entries, launch outcomes, lifecycle
bindings, the context meter cache, serve submissions — carry the same ISO-8601
vocabulary and read through here too, so zoneless text resolves as UTC
everywhere instead of following whichever machine happened to write it.  The
readers that stay outside this module do so because their vocabulary genuinely
differs, and each names that rule where it lives: Taskwarrior's compact form and
strict queue-age origins in ``tasks/readiness``, the local wall clock the task
graphs subtract against in ``tasks/graphs/derive``, ``task calc``'s naive local
answers in ``tasks/markdown/apply``, and the zoneless-is-ambiguous rule for
worktree bindings in ``serve/worktree/bindings``.
"""

from __future__ import annotations

from datetime import UTC, datetime


def parse_timestamp(raw: str | None) -> datetime | None:
    """Read any ISO-8601 timestamp shape as one aware UTC instant."""
    if not isinstance(raw, str) or not raw:
        return None
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)
    return parsed.astimezone(UTC)


def normalize_timestamp(raw: str | None) -> str | None:
    """Render an ISO-8601 timestamp in the canonical millisecond Zulu form."""
    parsed = parse_timestamp(raw)
    if parsed is None:
        return None
    return parsed.isoformat(timespec="milliseconds").replace("+00:00", "Z")
