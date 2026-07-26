"""Every timestamp reader in the tree agrees on the instant a stamp names."""

from __future__ import annotations

from datetime import UTC, datetime

from spice.agent import launchhistory, lifecyclebinding, wrap
from spice.serve import submissions
from spice.serve.messagepresentation import AssistantMessage
from spice.serve.payload import message
from spice.tasks import artifacts, effort
from spice.transcript.timestamps import normalize_timestamp, parse_timestamp

# Both dialects stamp lines with millisecond Zulu text; these two are copied off a
# live Claude Code transcript and a live Codex rollout.
CLAUDE_LINE_TIMESTAMP = "2026-07-25T19:30:09.260Z"
CODEX_LINE_TIMESTAMP = "2026-04-21T22:27:37.177Z"
# The remaining shapes all name the same instant as the Codex line: spice's own
# lifecycle records stamp microsecond Zulu, ``datetime.isoformat`` hands back a
# numeric offset, and a bare local stamp carries no zone at all.
LIFECYCLE_TIMESTAMP = "2026-04-21T22:27:37.177000Z"
EASTERN_OFFSET_TIMESTAMP = "2026-04-21T18:27:37.177-04:00"
UTC_OFFSET_TIMESTAMP = "2026-04-21T22:27:37.177+00:00"
NAIVE_TIMESTAMP = "2026-04-21T22:27:37.177"
SAME_INSTANT_SHAPES = (
    CODEX_LINE_TIMESTAMP,
    LIFECYCLE_TIMESTAMP,
    EASTERN_OFFSET_TIMESTAMP,
    UTC_OFFSET_TIMESTAMP,
    NAIVE_TIMESTAMP,
)
WHOLE_SECOND_TIMESTAMP = "2026-04-21T22:27:37Z"
SUB_MILLISECOND_TIMESTAMP = "2026-04-21T22:27:37.177999Z"

CODEX_INSTANT = datetime(2026, 4, 21, 22, 27, 37, 177000, tzinfo=UTC)
WINDOW_STEP_SECONDS = 0.001

# Record vocabularies outside the transcript — artifact ledger entries, launch
# outcomes, lifecycle bindings, the context meter cache, and serve submissions —
# read their stamps through the same primitive, so zoneless text resolves as UTC
# there too rather than as whatever the machine's offset happens to be.
RECORD_TIMESTAMP_SHAPES = (
    "2026-04-21T22:27:37.177000Z",
    "2026-04-21T22:27:37.177000",
    "2026-04-21T18:27:37.177000-04:00",
)
LATER_RECORD_TIMESTAMP = "2026-04-21T22:27:37.178000Z"
RECORD_READER_NAMES = (
    "artifact-ledger",
    "launch-history",
    "lifecycle-binding",
    "context-meter-cache",
    "serve-submission",
)


def _assistant_message(timestamp: str, *, index: int) -> AssistantMessage:
    return AssistantMessage(
        key=f"{timestamp}#{index}",
        index=index,
        timestamp=timestamp,
        text="hello",
        display_text="hello",
        display_html="<p>hello</p>",
        ack_count=0,
        ack_keys=[],
        ack_utterances=[],
        kind="assistant",
    )


def _window(*, started_at: float, ended_at: float) -> effort.PhaseEffortWindow:
    return effort.PhaseEffortWindow(
        task_id="timestamp-task-uuid",
        handle="READER-00000001",
        title="Read one instant",
        phase="todo",
        phase_index=0,
        actor_id="agent-a",
        thread_id="thread-a",
        team_id="team-a",
        driver="codex",
        model="gpt-5.5",
        effort="xhigh",
        started_at=started_at,
        ended_at=ended_at,
    )


def _serve_ordering_epoch(timestamp: str) -> float:
    epoch, _index, _key = message._message_sort_key(
        _assistant_message(timestamp, index=0)
    )
    return epoch


def _forensic_window_placement(timestamp: str, epoch: float) -> tuple[bool, bool]:
    """Bracket a stamp against a window opening exactly on ``epoch``."""
    return (
        effort._timestamp_in_window(
            timestamp,
            _window(started_at=epoch, ended_at=epoch + WINDOW_STEP_SECONDS),
        ),
        effort._timestamp_in_window(
            timestamp,
            _window(
                started_at=epoch + WINDOW_STEP_SECONDS,
                ended_at=epoch + WINDOW_STEP_SECONDS + WINDOW_STEP_SECONDS,
            ),
        ),
    )


def _record_reader_epochs(text: str) -> dict[str, float | None]:
    """One epoch per record reader that now shares the primitive."""
    created = artifacts._created_at({"created_at": text})
    return dict(
        zip(
            RECORD_READER_NAMES,
            (
                created.timestamp() if created is not None else None,
                launchhistory._epoch_seconds(text),
                lifecyclebinding._timestamp_epoch(text),
                wrap._iso_timestamp_seconds(text),
                submissions._optional_timestamp_epoch(text),
            ),
            strict=True,
        )
    )


def test_every_dialect_timestamp_shape_reads_one_instant():
    parsed = [parse_timestamp(shape) for shape in SAME_INSTANT_SHAPES]

    assert parsed == [CODEX_INSTANT] * len(SAME_INSTANT_SHAPES)
    assert parse_timestamp(WHOLE_SECOND_TIMESTAMP) == CODEX_INSTANT.replace(
        microsecond=0
    )
    assert parse_timestamp(SUB_MILLISECOND_TIMESTAMP) != CODEX_INSTANT
    assert parse_timestamp(CLAUDE_LINE_TIMESTAMP) != CODEX_INSTANT


def test_every_former_call_site_path_derives_the_same_instant():
    epoch = CODEX_INSTANT.timestamp()

    ordering_epochs = [_serve_ordering_epoch(shape) for shape in SAME_INSTANT_SHAPES]
    window_placements = [
        _forensic_window_placement(shape, epoch) for shape in SAME_INSTANT_SHAPES
    ]
    rendered = [normalize_timestamp(shape) for shape in SAME_INSTANT_SHAPES]

    assert ordering_epochs == [epoch] * len(SAME_INSTANT_SHAPES)
    assert window_placements == [(True, False)] * len(SAME_INSTANT_SHAPES)
    assert rendered == [CODEX_LINE_TIMESTAMP] * len(SAME_INSTANT_SHAPES)


def test_serve_orders_mixed_shape_timestamps_by_instant():
    items = [
        _assistant_message(CLAUDE_LINE_TIMESTAMP, index=0),
        _assistant_message(EASTERN_OFFSET_TIMESTAMP, index=1),
        _assistant_message(WHOLE_SECOND_TIMESTAMP, index=2),
    ]

    ordered = sorted(items, key=message._message_sort_key)

    assert [item.timestamp for item in ordered] == [
        WHOLE_SECOND_TIMESTAMP,
        EASTERN_OFFSET_TIMESTAMP,
        CLAUDE_LINE_TIMESTAMP,
    ]


def test_rendering_truncates_to_milliseconds_while_parsing_keeps_the_microseconds():
    assert normalize_timestamp(SUB_MILLISECOND_TIMESTAMP) == CODEX_LINE_TIMESTAMP
    assert parse_timestamp(SUB_MILLISECOND_TIMESTAMP) == CODEX_INSTANT.replace(
        microsecond=177999
    )


def test_every_record_reader_agrees_on_one_instant():
    epoch = CODEX_INSTANT.timestamp()

    readings = {
        shape: _record_reader_epochs(shape) for shape in RECORD_TIMESTAMP_SHAPES
    }

    assert readings == {
        shape: dict.fromkeys(RECORD_READER_NAMES, epoch)
        for shape in RECORD_TIMESTAMP_SHAPES
    }


def test_record_readers_keep_neighbouring_instants_apart():
    zulu_shape, naive_shape, offset_shape = RECORD_TIMESTAMP_SHAPES

    later = _record_reader_epochs(LATER_RECORD_TIMESTAMP)

    assert later != _record_reader_epochs(zulu_shape)
    assert _record_reader_epochs(naive_shape) == _record_reader_epochs(offset_shape)


def test_unreadable_timestamp_text_leaves_every_scan_running():
    epoch = CODEX_INSTANT.timestamp()
    unreadable = "t"

    assert (parse_timestamp(unreadable), normalize_timestamp(unreadable)) == (
        None,
        None,
    )
    assert _forensic_window_placement(unreadable, epoch) == (False, False)
    assert _serve_ordering_epoch(unreadable) == 0.0
