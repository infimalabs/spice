"""Incremental assembly of typed transcript facts into semantic message spans.

This is the policy-free middle of the transcript stack.  Driver adapters and
the public reader produce typed events; this module groups adjacent facts from
one source locus and classifies their assistant-facing meaning.  Consumers
remain responsible for presentation, paging, turn folding, starvation policy,
and every other interpretation above these factual spans.

The public reducer accepts only the closed :class:`TranscriptEvent` union.  It
does not know the historical canonical-dictionary seam.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from enum import StrEnum

from spice.mail.ackstate import ACK_DISPOSITION_ACKED, ACK_DISPOSITION_REFUSED
from spice.mail.ackgrammar import (
    iter_control_lines,
    split_keyed_response,
    task_directive_fields,
    trim_blank_lines,
)
from spice.transcript.events import (
    AssistantText,
    CommandExecution,
    Compaction,
    ContextUsage,
    FailureSignal,
    Image,
    Provenance,
    Reasoning,
    ToolCall,
    ToolOutput,
    TranscriptEvent,
    TurnBoundary,
    Unknown,
    UserMessage,
    WebSearch,
    WorkingDirectory,
)

__all__ = [
    "AssembledMessage",
    "AssembledMessageReducer",
    "ClassifiedSpan",
    "DirectiveKind",
    "SpanKind",
    "span_disposition",
]

_APP_DIRECTIVE_LINE_RE = re.compile(r"^\s*::[a-z][a-z0-9-]*\{.*\}\s*$")
_EVENT_TYPES = (
    AssistantText,
    Reasoning,
    ToolCall,
    ToolOutput,
    CommandExecution,
    WorkingDirectory,
    Image,
    UserMessage,
    TurnBoundary,
    Compaction,
    WebSearch,
    ContextUsage,
    FailureSignal,
    Unknown,
)


class SpanKind(StrEnum):
    """The factual assistant-message meanings consumers can project."""

    PROSE = "prose"
    ACK = "ack"
    NACK = "nack"
    TOOL = "tool"
    REASONING = "reasoning"
    IMAGE = "image"
    FINAL_ANSWER = "final_answer"
    COMPACTION = "compaction"
    FAILURE = "failure"


class DirectiveKind(StrEnum):
    """Control-line families removed from assistant-visible prose."""

    TASK = "task"
    APP = "app"


@dataclass(frozen=True, slots=True)
class ClassifiedSpan:
    """One ordered semantic span backed by its original typed event.

    `kind` always names the run the span sits in, so a control line carries the
    polarity of the response that contains it rather than a kind of its own. A
    consumer that must re-join a run therefore reads the polarity off any of its
    spans instead of inferring one for the lines it cannot classify.
    `directive_kind` is what marks a span as a control line and names its family.
    `response_index` is which keyed response of its own source event the span
    came from, unset for preamble prose, and it is what keeps two responses
    that agree on both polarity and keys from re-joining into one run. It
    counts from zero per event, so a consumer spanning events pairs it with the
    event identity.
    """

    kind: SpanKind
    at: Provenance
    event: TranscriptEvent
    text: str = ""
    keys: tuple[str, ...] = ()
    directive_kind: DirectiveKind | None = None
    response_index: int | None = None


@dataclass(frozen=True, slots=True)
class AssembledMessage:
    """All classified assistant facts emitted at one transcript locus."""

    at: Provenance
    spans: tuple[ClassifiedSpan, ...]


@dataclass(frozen=True, slots=True)
class _Directive:
    kind: DirectiveKind
    text: str


MessageLocus = tuple[str, int, str | None, int | None, str | None]


class AssembledMessageReducer:
    """Incrementally fold a typed event stream into assembled messages."""

    def __init__(self) -> None:
        self._locus: MessageLocus | None = None
        self._at: Provenance | None = None
        self._spans: list[ClassifiedSpan] = []
        self._final_event: AssistantText | None = None

    def push(self, event: TranscriptEvent) -> tuple[AssembledMessage, ...]:
        """Accept one typed fact and emit a completed prior-locus message."""
        _require_typed_event(event)
        locus = _message_locus(event.at)
        emitted: tuple[AssembledMessage, ...] = ()
        if self._locus is not None and locus != self._locus:
            emitted = self._flush()

        spans = _event_spans(event)
        final_event = (
            event if isinstance(event, AssistantText) and event.final else None
        )
        if spans or final_event is not None:
            if self._locus is None:
                self._locus = locus
                self._at = event.at
            self._spans.extend(spans)
            if final_event is not None:
                self._final_event = final_event
        return emitted

    def finish(self) -> tuple[AssembledMessage, ...]:
        """Emit the final pending message and reset the reducer."""
        return self._flush()

    def _flush(self) -> tuple[AssembledMessage, ...]:
        if self._locus is None or self._at is None:
            return ()
        spans = list(self._spans)
        if self._final_event is not None:
            spans.append(
                ClassifiedSpan(
                    kind=SpanKind.FINAL_ANSWER,
                    at=self._final_event.at,
                    event=self._final_event,
                )
            )
        message = AssembledMessage(at=self._at, spans=tuple(spans))
        self._locus = None
        self._at = None
        self._spans = []
        self._final_event = None
        return (message,)


def span_disposition(kind: SpanKind) -> str:
    """The ACK-state disposition a keyed span's polarity already decided.

    Classification happens once, here, when the span is built; consumers that
    put the polarity on a wire ask for its name rather than re-reading the
    header or comparing kinds themselves.
    """
    if kind is SpanKind.ACK:
        return ACK_DISPOSITION_ACKED
    if kind is SpanKind.NACK:
        return ACK_DISPOSITION_REFUSED
    raise ValueError(f"span kind {kind} has no ACK disposition")


def _event_spans(event: TranscriptEvent) -> tuple[ClassifiedSpan, ...]:
    if isinstance(event, AssistantText):
        return _assistant_text_spans(event)
    if isinstance(event, (ToolCall, ToolOutput, WebSearch)):
        return (
            ClassifiedSpan(
                kind=SpanKind.TOOL,
                at=event.at,
                event=event,
                text=_tool_text(event),
            ),
        )
    if isinstance(event, Reasoning):
        return (
            ClassifiedSpan(
                kind=SpanKind.REASONING,
                at=event.at,
                event=event,
                text=event.summary,
            ),
        )
    if isinstance(event, Image):
        return (
            ClassifiedSpan(
                kind=SpanKind.IMAGE,
                at=event.at,
                event=event,
                text=event.url,
            ),
        )
    if isinstance(event, Compaction):
        return (
            ClassifiedSpan(
                kind=SpanKind.COMPACTION,
                at=event.at,
                event=event,
            ),
        )
    if isinstance(event, FailureSignal):
        return (
            ClassifiedSpan(
                kind=SpanKind.FAILURE,
                at=event.at,
                event=event,
                text=event.kind,
            ),
        )
    if isinstance(
        event,
        (
            CommandExecution,
            UserMessage,
            TurnBoundary,
            ContextUsage,
            WorkingDirectory,
            Unknown,
        ),
    ):
        return ()
    raise TypeError(f"unsupported transcript event: {type(event).__name__}")


def _assistant_text_spans(event: AssistantText) -> tuple[ClassifiedSpan, ...]:
    masked, directives = _mask_directives(event.text)
    preamble, responses = split_keyed_response(
        masked,
        drop_task_directives=False,
    )
    spans = list(
        _segment_spans(
            preamble,
            event,
            kind=SpanKind.PROSE,
            directives=directives,
        )
    )
    for response_index, response in enumerate(responses):
        kind = (
            SpanKind.NACK
            if response.disposition == ACK_DISPOSITION_REFUSED
            else SpanKind.ACK
        )
        response_spans = _segment_spans(
            response.content,
            event,
            kind=kind,
            directives=directives,
            keys=response.keys,
            response_index=response_index,
        )
        spans.extend(
            response_spans
            or (
                ClassifiedSpan(
                    kind=kind,
                    at=event.at,
                    event=event,
                    keys=response.keys,
                    response_index=response_index,
                ),
            )
        )
    return tuple(spans)


def _segment_spans(
    content: str,
    event: AssistantText,
    *,
    kind: SpanKind,
    directives: dict[str, _Directive],
    keys: tuple[str, ...] = (),
    response_index: int | None = None,
) -> tuple[ClassifiedSpan, ...]:
    spans: list[ClassifiedSpan] = []
    pending: list[str] = []

    def flush_pending() -> None:
        text = trim_blank_lines("\n".join(pending))
        pending.clear()
        if text:
            spans.append(
                ClassifiedSpan(
                    kind=kind,
                    at=event.at,
                    event=event,
                    text=text,
                    keys=keys,
                    response_index=response_index,
                )
            )

    for line in content.splitlines():
        directive = directives.get(line)
        if directive is None:
            pending.append(line)
            continue
        flush_pending()
        spans.append(
            ClassifiedSpan(
                kind=kind,
                at=event.at,
                event=event,
                text=directive.text,
                keys=keys,
                directive_kind=directive.kind,
                response_index=response_index,
            )
        )
    flush_pending()
    return tuple(spans)


def _mask_directives(text: str) -> tuple[str, dict[str, _Directive]]:
    marker_prefix = "\0spice-directive:"
    while marker_prefix in text:
        marker_prefix = f"\0{marker_prefix}"
    masked: list[str] = []
    directives: dict[str, _Directive] = {}
    # A directive that is only being shown -- fenced, quoted, indented, or in
    # rendered source context -- must survive as prose. Masking it here would
    # strip it to a marker, and every reader downstream would then see a bare
    # directive with no way to tell it was an example.
    for line, suppressed in iter_control_lines(text):
        directive_kind = None if suppressed else _directive_kind(line)
        if directive_kind is None:
            masked.append(line)
            continue
        marker = f"{marker_prefix}{len(directives)}\0"
        directives[marker] = _Directive(
            kind=directive_kind,
            text=line.strip(),
        )
        masked.append(marker)
    return "\n".join(masked), directives


def _directive_kind(line: str) -> DirectiveKind | None:
    if _APP_DIRECTIVE_LINE_RE.match(line) is not None:
        return DirectiveKind.APP
    if task_directive_fields(line) is not None:
        return DirectiveKind.TASK
    return None


def _tool_text(event: ToolCall | ToolOutput | WebSearch) -> str:
    if isinstance(event, ToolCall):
        return event.name
    if isinstance(event, ToolOutput):
        return event.content
    return event.query or event.url or event.pattern or event.action_type or ""


def _message_locus(at: Provenance) -> MessageLocus:
    return (
        at.source,
        at.line,
        at.timestamp,
        at.offset,
        at.source_actor,
    )


def _require_typed_event(event: TranscriptEvent) -> None:
    if not isinstance(event, _EVENT_TYPES):
        raise TypeError(
            "assembled-message reducer requires a typed TranscriptEvent, "
            f"got {type(event).__name__}"
        )
