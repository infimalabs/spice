"""The closed typed event vocabulary every transcript consumer decodes into.

This module is the substrate under the driver seam: it owns *what a transcript
line means*, expressed as a small closed set of frozen typed events rather than
untyped payload dictionaries. Driver adapters decode their own dialect into
these events; every plane above (serve, sessions, mail, the watchdog) consumes
them without knowing which agent produced the line.

Two deliberate properties make the vocabulary a substrate rather than another
plane:

Multiplicity. One raw line yields zero, one, or many events. An assistant line
carrying prose plus a tool call plus reasoning decodes to three events, ordered
by their within-line ordinal, so no consumer has to reach back below the seam to
recover a block that a one-item-per-line collapse dropped.

Plane neutrality. Nothing here imports a consumer plane, and timestamps are
carried exactly as the source wrote them, unparsed. Interpreting a timestamp is
a reader concern with its own consolidation underway; keeping the raw string
here means the substrate never becomes the home of yet another date parser.

The set is closed but not frozen for all time: a kind is added when a decoder
actually emits it, never speculatively. Codex-only facts (a provider-side web
search, and context-usage accounting that is still a driver side channel today)
join when the Codex adapter lands and settles them.
"""

from __future__ import annotations

from dataclasses import dataclass

# The dict seam that predates this substrate hands its decoder one raw line with
# no idea which file or offset it came from. Events decoded through that legacy
# path carry this source rather than a fabricated path; real loci arrive with the
# reader engine, which iterates files and knows both.
UNLOCATED_SOURCE = "<unlocated>"


@dataclass(slots=True, frozen=True)
class Provenance:
    """Where one event came from: which line, and where within that line."""

    source: str
    line: int
    ordinal: int
    timestamp: str | None


@dataclass(slots=True, frozen=True)
class AssistantText:
    """Prose the agent addressed to the operator.

    `final` marks text the driver reported as ending the turn, a line-level fact
    every text block on that line shares.
    """

    at: Provenance
    text: str
    final: bool


@dataclass(slots=True, frozen=True)
class Reasoning:
    """Agent thinking the driver exposed, as a summary rather than raw chain."""

    at: Provenance
    summary: str


@dataclass(slots=True, frozen=True)
class ToolCall:
    """A tool invocation the agent requested."""

    at: Provenance
    call_id: str
    name: str
    arguments: str


@dataclass(slots=True, frozen=True)
class ToolOutput:
    """The result handed back for an earlier `ToolCall`."""

    at: Provenance
    call_id: str
    content: str
    failed: bool


@dataclass(slots=True, frozen=True)
class Image:
    """An image block, carried as the resolved URL consumers render.

    Base64 blocks arrive as a `data:` URL, so the media type and payload stay
    recoverable from the single field without a second encoding step.
    """

    at: Provenance
    url: str


@dataclass(slots=True, frozen=True)
class UserMessage:
    """Input addressed to the agent, whether typed by an operator or injected.

    `prompt_id` is present only on a real operator prompt, which is what makes a
    turn boundary distinguishable from an injected tool-result line.
    """

    at: Provenance
    text: str
    prompt_id: str | None


@dataclass(slots=True, frozen=True)
class Compaction:
    """A compaction the driver reported, as one of two distinguishable facts.

    `active` tracks a compaction *running*: a resumed thread compacts before the
    agent can act, and a supervisor needs that to tell a busy process from a
    wedged one. `boundary` marks a compaction that *completed and produced new
    context*, which only ever arrives afterward.
    """

    at: Provenance
    active: bool
    boundary: bool


@dataclass(slots=True, frozen=True)
class Unknown:
    """A line or block the decoder could not type, kept rather than dropped.

    Malformed JSON, an unrecognized block shape, and a dialect extension all land
    here with the reason recorded, so an unreadable fragment stays visible as a
    fact instead of vanishing between the decoder and its consumers.
    """

    at: Provenance
    reason: str
    raw_type: str | None


TranscriptEvent = (
    AssistantText
    | Reasoning
    | ToolCall
    | ToolOutput
    | Image
    | UserMessage
    | Compaction
    | Unknown
)


@dataclass(slots=True)
class LineStamper:
    """Hand out ascending within-line ordinals while decoding one raw line.

    A decoder builds one stamper per line and calls `stamp()` as it emits each
    event, so ordinals follow emission order without the decoder counting blocks
    up front — it cannot know the count in advance when some blocks decode to
    nothing.
    """

    source: str
    line: int
    timestamp: str | None
    ordinal: int = 0

    def stamp(self) -> Provenance:
        provenance = Provenance(
            source=self.source,
            line=self.line,
            ordinal=self.ordinal,
            timestamp=self.timestamp,
        )
        self.ordinal += 1
        return provenance
