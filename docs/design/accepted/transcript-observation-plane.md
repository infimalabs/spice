# Transcript observation plane

Status: implemented contract, 2026-07-26.

## Decision

The immutable driver transcript is the sole stored truth. Spice does not write
a normalized transcript, event mirror, index copy, or consumer-owned cache of
decoded history.

Every file-backed consumer crosses the same three layers:

1. `spice/transcript/reader.py` opens plain or gzip JSONL and owns forward,
   bounded, reverse-window, timestamp-tail, cursor, truncation, and rotation
   behavior.
2. `spice/transcript/decode.py` hands each parsed object to its owning driver
   exactly once. The driver adapter emits zero or more immutable events from
   `spice/transcript/events.py`, retaining source, byte offset, timestamp, actor,
   and within-line ordinal.
3. `spice/transcript/assembly.py` groups typed facts at a source locus and
   classifies prose, ACK/NACK, directives, tools, reasoning, images,
   compactions, final answers, and failures once. Product planes project those
   spans or the lower typed facts without interpreting driver JSON.

The deleted `AgentDriver.normalize_transcript_line` dictionary seam and the
Claude/Codex reverse projections are not compatibility APIs. They were lossy
or duplicate interpreters and have no replacement above the typed stream.

## Public access contract

`TranscriptEventReader(path, driver, source_actor).read(...)` is the only
production file-to-event entry point.

| Mode | Contract |
| --- | --- |
| `forward` | Resume from a locked `TranscriptCursor`; hold an incomplete tail; reset on shrink or filesystem-identity change. |
| `bounded` | Decode the requested byte-offset range in source order; optionally align a partial start to the next record. |
| `reverse` | Decode one bounded tail window ending at a stable offset, chronologically. |
| `since` | Page the reverse engine until a timestamp boundary and retain only the requested prefix context. |

One `TranscriptEventRead` contains one immutable event tuple plus the exact
access offsets, file identity, error, and physical `file_opens`, `bytes_read`,
and `lines_parsed` counters. Integration instrumentation wraps the engine's
open and parse seams and aggregates those per-read facts for a complete caller
operation. A read can dispatch the same event objects to multiple projections;
consumers do not reparse the source or copy raw payload dictionaries.

The driver layer has three documented dialect hooks:

- `transcript_line_events` emits the lossless typed vocabulary;
- `context_snapshot_fields` emits provider usage fields;
- `stream_failure_fields` emits structural terminal failure facts.

Claude's recorded `cwd` is no longer found by a private JSONL loop. The common
driver wrapper types it as `WorkingDirectory`, and resumability reads one
bounded 64 KiB head window through `TranscriptEventReader`. Driver-owned stdout
marker framing remains separate because it consumes a live process stream, not
a transcript file.

## Production projections

| Plane | Projection |
| --- | --- |
| Serve history and live delivery | `spice/serve/messages.py` uses reverse, bounded, and cursor-forward reads, then the assembled-message reducer. |
| Embedded images | `spice/serve/images.py` performs one bounded typed read at the requested source offset. |
| Serve activity and effort | `spice/serve/metrics.py` and `spice/tasks/effort.py` fold typed context, turn, and compaction facts. |
| Session forensics | `spice/sessions/records.py`, `analysis.py`, and `commandrecords.py` project typed facts; briefing turn and compaction rows share one `since` read. |
| Recovery slices | `spice/sessions/slices.py` pages typed reverse windows and reduces compaction spans. |
| Supervision and ACK archival | `spice/agent/watchdog.py` decodes live stdout with the same driver adapter and reducer; persisted replay uses the reader. |
| Launch history | `spice/agent/launchhistory.py` reads typed transcript events and assembled messages. |
| Mail ACK presentation | The former `spice/mail/watch.py` scanner is deleted; ACK/NACK presentation consumes reducer-classified spans. |

There is no production caller of the deleted canonical-dictionary seam, no
consumer-local transcript `json.loads` loop, and no second payload-family
classifier outside the two driver adapters. Plain startup-log head/tail reads
and stdout framing are transport operations, not transcript semantic readers.

## Physical proof

`tests/test_transcriptconvergence.py` generates equivalent live-shaped Codex
and Claude fixtures, while the crossing suites replay the recorded corpus for
both drivers. The test's deterministic Codex source is 1,591 bytes and 9
records.

| Operation | Before this terminal pass | Implemented result |
| --- | ---: | ---: |
| Initial Serve history page | 3 file opens, 1,591 bytes decoded, 9 lines parsed | 1 open, 1,591 bytes decoded, 9 lines parsed |
| Unchanged Serve append poll | 1 size open, 0 bytes decoded, 0 lines parsed | unchanged: 1 size open, 0 bytes decoded, 0 lines parsed |
| Session briefing (horizon plus turns and compactions) | 3 opens, 4,773 bytes decoded, 27 lines parsed | 2 opens, 3,182 bytes decoded, 18 lines parsed |

The Serve reduction removes two redundant size opens before the initial reverse
read. The briefing reduction feeds turns and compactions from one typed `since`
read instead of decoding that range independently for each projection. The
horizon reverse pass remains distinct: it determines the start offset of the
subsequent projection pass.

`tests/test_servemessages.py` additionally forces sparse 256-byte reverse pages
and counts every source record and envelope projection exactly once. The reader
probe test proves a second forward read at an unchanged cursor opens the source
to observe EOF but reads zero bytes and parses zero lines.

## Equivalence and recovery

The shared recorded corpus and fresh two-driver fixtures compare:

- forward against split bounded delivery;
- Serve reverse-window envelopes against live cursor delivery;
- forensic turns across whole and split passes;
- user-visible ACK text against classified reducer spans;
- stdout watchdog judgments against persisted replay;
- launch event narratives against assembled-message narratives;
- embedded-image selection through a bounded public read;
- effort facts through the same typed event stream.

Reader fault tests cover plain and gzip sources, malformed complete records,
partial tails completed in two writes, empty files, oversized records,
concurrent append behind a fixed page boundary, shrink, inode rotation, cursor
loss, and page boundaries. A malformed complete line becomes `Unknown`; a live
partial tail remains behind the cursor until complete; a replaced source resets
the cursor; failures return an empty typed read with an explicit error. No
recovery path creates a durable copy.
