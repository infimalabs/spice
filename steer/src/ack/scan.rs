//! Recognizing explicit protocol packets in assistant prose.
//!
//! Acknowledgment is a narrow protocol at the assistant-output boundary, not an
//! interpretation of what a turn seems to have meant. A clause retires a
//! directive only when it spells a marker in authored prose, names a key-shaped
//! token, and that key belongs to a directive still live. Everything else —
//! unknown, malformed, quoted, or historical — alters nothing and is reported
//! by name, because a silent non-recognition and a refusal look identical to an
//! operator waiting on an answer.
//!
//! The same authored-context grammar reads [`message`] packets: `SYN` headers
//! that name their routes and own a body hunk but never retire anything. One
//! visitor finds every packet, so a placed header bounds the body before it.
//!
//! The scanner performs no I/O. Standing arrives from the caller's reduced
//! facts, so the same prose read against the same standing always recognizes
//! the same outcomes, whether it runs at the door or during replay.

use std::fmt;
use std::ops::{ControlFlow, Range};

use wire::Disposition;

use crate::ack::keys::{Key, MARKERS, Marker};

pub mod message;

pub use message::{Audience, Message, MessageRefusal, Publication, Route};

/// Where in the scanned prose a recognition sits.
///
/// Both parts are one-based, and the column is a byte offset within its line.
/// The pair is what the disposition fact carries as its transcript locus, so a
/// retirement can always be pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Locus {
    /// Line the recognition sits on.
    pub line: usize,
    /// Byte offset the recognized token begins at.
    pub column: usize,
}

impl Locus {
    /// Where one byte offset of `prose` sits, on the lines the scanner reads.
    #[must_use]
    pub fn at(prose: &str, offset: usize) -> Self {
        let before = &prose.as_bytes()[..offset];
        let start = before
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |newline| newline + 1);
        Self {
            line: before.iter().filter(|byte| **byte == b'\n').count() + 1,
            column: offset - start + 1,
        }
    }
}

impl fmt::Display for Locus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.line, self.column)
    }
}

/// Why a clause named a key without retiring anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Miss {
    /// The clause sat inside quoting, code, or rendered source context, where a
    /// key is shown rather than spoken.
    Quoted,
    /// The token is not spelled as a key.
    Malformed,
    /// No directive carries the key.
    Unknown,
    /// The directive carrying the key already retired.
    Historical,
}

impl Miss {
    /// The word this miss is reported under.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quoted => "quoted",
            Self::Malformed => "malformed",
            Self::Unknown => "unknown",
            Self::Historical => "historical",
        }
    }
}

impl fmt::Display for Miss {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What one token a clause named actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The key retired its directive into this disposition.
    Retired {
        /// Packet marker that authored the transition.
        marker: Marker,
        /// Key the clause named.
        key: Key,
        /// Disposition the marker retires into.
        disposition: Disposition,
    },
    /// The token altered nothing, for this reason.
    Missed {
        /// Token exactly as it was written.
        token: String,
        /// Why it altered nothing.
        miss: Miss,
    },
}

impl Outcome {
    /// The token exactly as the clause named it.
    #[must_use]
    pub fn token(&self) -> &str {
        match self {
            Self::Retired { key, .. } => key.as_str(),
            Self::Missed { token, .. } => token,
        }
    }

    /// Whether the clause naming this token was authored protocol prose.
    ///
    /// A retirement is always placed, and so is a miss judged on the key
    /// itself; quoted clauses do not speak.
    #[must_use]
    pub const fn placed(&self) -> bool {
        !matches!(
            self,
            Self::Missed {
                miss: Miss::Quoted,
                ..
            }
        )
    }
}

/// One token a clause named, and where it was named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognition {
    /// Where the token sits in the scanned prose.
    pub locus: Locus,
    /// What the token did.
    pub outcome: Outcome,
}

/// What the scanner must ask before it can retire anything.
///
/// Keeping this a question rather than a lookup is what holds the scanner to
/// zero I/O: the caller has already reduced the facts, and the scanner only
/// consults the result.
pub trait Standing {
    /// The disposition the directive carrying this key sits in, or `None` when
    /// no directive carries it.
    fn disposition(&self, key: &Key) -> Option<Disposition>;
}

/// One structured ACK, FIN, or NAK packet in assistant prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acknowledgement {
    /// Zero-based byte offset where the marker begins in the complete prose.
    pub start: usize,
    /// Zero-based byte offset immediately after the complete header.
    pub end: usize,
    /// Optional punctuation separating the key list from response prose.
    pub separator: Option<char>,
    /// Outcomes for every token named by this one header.
    pub recognitions: Vec<Recognition>,
}

/// Reads every acknowledgment outcome in one assistant turn.
pub fn scan(prose: &str, standing: &impl Standing) -> Vec<Recognition> {
    scan_acknowledgements(prose, standing)
        .into_iter()
        .flat_map(|acknowledgement| acknowledgement.recognitions)
        .collect()
}

/// The recognition that decides `token` in one assistant prose.
///
/// An authored clause outranks a quoted mention of the same token wherever the
/// two sit in the prose. The header is the marker an operator reads; a later
/// citation of it in backticks is prose about the header, not a retraction of
/// it. Between clauses of the same rank the later one decides. `None` when
/// nothing in the prose names the token.
#[must_use]
pub fn decisive(prose: &str, token: &str, standing: &impl Standing) -> Option<Recognition> {
    scan(prose, standing)
        .into_iter()
        .enumerate()
        .filter(|(_, recognition)| recognition.outcome.token() == token)
        .max_by_key(|(index, recognition)| (recognition.outcome.placed(), *index))
        .map(|(_, recognition)| recognition)
}

/// Whether an ACK response is a seen reaction rather than peer payload.
///
/// These exact values are grammar, shared by transcript scanning and typed
/// line controls.
#[must_use]
pub fn ack_reaction(clause: Option<&str>) -> bool {
    clause.is_none_or(|clause| {
        let clause = clause.trim();
        if clause.is_empty() {
            return true;
        }
        let clause = clause.trim_end_matches(['.', ',', ':', ';', '!']);
        clause.eq_ignore_ascii_case("ok")
            || clause.eq_ignore_ascii_case("noted")
            || clause.eq_ignore_ascii_case("thanks")
            || clause == "+1"
    })
}

/// The byte range of the response prose the placed header that retires
/// `token` gives it: from the end of the header to the next placed packet or
/// the end of the prose. Surrounding whitespace is trimmed off the range.
///
/// A payload-free ACK or FIN ends at its first following blank line. Prose
/// after that line is ordinary operator-facing prose, not peer payload. ACK,
/// FIN, and NAK bodies with real payload keep interior blank lines.
///
/// Between placed headers retiring the same token the later one speaks, as
/// [`decisive`] rules. `None` when no placed header retires the token or the
/// header says nothing before its boundary.
#[must_use]
pub fn answer(prose: &str, token: &str, standing: &impl Standing) -> Option<Range<usize>> {
    let (header, marker) = scan_acknowledgements(prose, standing)
        .into_iter()
        .rev()
        .find_map(|acknowledgement| {
            let marker = acknowledgement
                .recognitions
                .iter()
                .find_map(|recognition| match &recognition.outcome {
                    Outcome::Retired { marker, key, .. } if key.as_str() == token => Some(*marker),
                    Outcome::Retired { .. } | Outcome::Missed { .. } => None,
                })?;
            Some((acknowledgement, marker))
        })?;
    let mut end = next_placed_packet_start(prose, header.end).map_or(prose.len(), |start| {
        message::boundary_before(prose, header.end, start)
    });
    if let Some(blank) = first_blank_line(prose, header.end, end) {
        let before = &prose[header.end..blank];
        let payload_free = match marker {
            Marker::Ack => ack_reaction(Some(before)),
            Marker::Fin => before.trim().is_empty(),
            Marker::Nak => false,
        };
        if payload_free {
            end = blank;
        }
    }
    let hunk = &prose[header.end..end];
    let start = header.end + (hunk.len() - hunk.trim_start().len());
    let end = start + hunk.trim().len();
    (start < end).then_some(start..end)
}

/// Start of the first blank line after the header's own line.
fn first_blank_line(prose: &str, start: usize, end: usize) -> Option<usize> {
    let mut line_start = start;
    let mut header_line = true;
    while line_start < end {
        let line_end = prose[line_start..end]
            .find('\n')
            .map_or(end, |offset| line_start + offset);
        if !header_line && prose[line_start..line_end].trim().is_empty() {
            return Some(line_start);
        }
        if line_end == end {
            break;
        }
        line_start = line_end + 1;
        header_line = false;
    }
    None
}

/// Reads acknowledgment packets with their source header boundaries.
///
/// A packet may appear anywhere in authored prose. Markers in quoted, code, or
/// rendered source context remain misses, while explicit negation,
/// hypothetical phrasing, and narration about protocol markers stay silent.
pub fn scan_acknowledgements(prose: &str, standing: &impl Standing) -> Vec<Acknowledgement> {
    let mut acknowledgements = Vec::new();
    let _ = visit_packets(prose, |lines, packet| {
        if let Packet::Retirement(clause) = packet {
            let (tokens, end, separator) = clause.effective(prose, standing);
            let mut recognitions = Vec::new();
            clause.judge(prose, lines, standing, tokens, &mut recognitions);
            acknowledgements.push(Acknowledgement {
                start: clause.start,
                end,
                separator,
                recognitions,
            });
        }
        ControlFlow::Continue(())
    });
    acknowledgements
}

/// Whether `prose` contains a placed SYN header.
///
/// This is the allocation-free predicate for transcript surfaces that need to
/// distinguish an explicitly authored packet from ordinary narration.
#[must_use]
pub fn has_message(prose: &str) -> bool {
    if !prose.contains(message::MARKER) {
        return false;
    }
    matches!(
        visit_packets(prose, |_, packet| match packet {
            Packet::Message {
                position: Position::Placed,
                ..
            } => ControlFlow::Break(()),
            Packet::Retirement(_) | Packet::Message { .. } => ControlFlow::Continue(()),
        }),
        ControlFlow::Break(())
    )
}

/// Reads every placed SYN packet, each bounded by the next placed packet or the
/// end of the prose.
///
/// Quoted, code, rendered-source, negated, hypothetical, and narrated markers
/// publish nothing and bound nothing. The scan needs no standing: a
/// publication retires no key, and whether an ACK, FIN, or NAK header is placed
/// does not depend on which keys are live.
#[must_use]
pub fn scan_messages(prose: &str) -> Vec<Message> {
    let mut placed = Vec::new();
    let _ = visit_packets(prose, |_, packet| {
        match packet {
            Packet::Retirement(clause) if clause.position == Position::Placed => {
                placed.push((clause.start, None));
            }
            Packet::Message {
                position: Position::Placed,
                start,
                header,
            } => placed.push((start, Some(header))),
            Packet::Retirement(_) | Packet::Message { .. } => {}
        }
        ControlFlow::Continue(())
    });
    let starts: Vec<usize> = placed.iter().map(|(start, _)| *start).collect();
    placed
        .into_iter()
        .filter_map(|(start, header)| {
            let header = header?;
            let boundary = starts
                .iter()
                .find(|next| **next >= header.end)
                .map_or(prose.len(), |next| {
                    message::boundary_before(prose, header.end, *next)
                });
            Some(message::judge(prose, start, header, boundary))
        })
        .collect()
}

/// First placed packet starting at or after `floor`, if one exists.
fn next_placed_packet_start(prose: &str, floor: usize) -> Option<usize> {
    let mut next = None;
    let _ = visit_packets(prose, |_, packet| {
        let start = match packet {
            Packet::Retirement(clause) if clause.position == Position::Placed => clause.start,
            Packet::Message {
                position: Position::Placed,
                start,
                ..
            } => start,
            Packet::Retirement(_) | Packet::Message { .. } => {
                return ControlFlow::Continue(());
            }
        };
        if start >= floor {
            next = Some(start);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    next
}

/// Whether prose contains an authored acknowledgement attempt.
///
/// This is the standing-independent preflight for callers that need to load
/// standing before they can run [`scan`]. It shares the canonical parser,
/// including multiline headers and every quoted-context exclusion.
#[must_use]
pub fn has_acknowledgement(prose: &str) -> bool {
    visit_packets(prose, |_, packet| match packet {
        Packet::Retirement(clause) if clause.position == Position::Placed => ControlFlow::Break(()),
        Packet::Retirement(_) | Packet::Message { .. } => ControlFlow::Continue(()),
    })
    .is_break()
}

/// The byte ranges of every placed protocol header, in source order.
///
/// An ACK, FIN, or NAK range runs from its marker, including touching emphasis,
/// through its key list and optional separator; a SYN range is that message's
/// [`Message::header`]. Bodies and response prose are never inside a range, and
/// quoted, code, rendered-source, negated, hypothetical, and narrated markers
/// contribute none, so removing every range leaves what author wrote around
/// the protocol. The scan needs no standing.
#[must_use]
pub fn placed_headers(prose: &str) -> Vec<Range<usize>> {
    let mut headers = Vec::new();
    let _ = visit_packets(prose, |_, packet| {
        match packet {
            Packet::Retirement(clause) if clause.position == Position::Placed => {
                headers.push(clause.start..clause.end);
            }
            Packet::Message {
                position: Position::Placed,
                start,
                header,
            } => headers.push(start..header.end),
            Packet::Retirement(_) | Packet::Message { .. } => {}
        }
        ControlFlow::Continue(())
    });
    headers
}

/// One protocol header the authored-context grammar recognized.
enum Packet {
    /// An ACK, FIN, or NAK clause.
    Retirement(Clause),
    /// A publication header whose body is bounded once every packet is known.
    Message {
        position: Position,
        start: usize,
        header: message::Header,
    },
}

fn visit_packets(
    prose: &str,
    mut visit: impl FnMut(&SourceLines, Packet) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let lines = SourceLines::read(prose);
    for (start, _) in prose.char_indices() {
        let Some(lead) = lead_at(prose, start) else {
            continue;
        };
        let decorated_start = emphasis_start(prose, start);
        let quoted = lines.suppressed(start) || inline_quoted(prose, start, &lines);
        if !quoted && guarded_context(prose, start) {
            continue;
        }
        let packet =
            match lead {
                Lead::Retirement(marker) => {
                    let Some(header) = tokens_after(prose, start + marker.as_str().len()) else {
                        continue;
                    };
                    if !header.tokens.iter().any(|&(token_start, token_end)| {
                        intended_token(&prose[token_start..token_end])
                    }) {
                        continue;
                    }
                    Packet::Retirement(Clause {
                        marker,
                        position: position(quoted, &header.tokens, &lines),
                        start: decorated_start,
                        end: header.end,
                        separator: header.separator,
                        tokens: header.tokens,
                    })
                }
                Lead::Message => {
                    let Some(header) = message::header_after(prose, start + message::MARKER.len())
                    else {
                        continue;
                    };
                    Packet::Message {
                        position: position(quoted, &header.tokens, &lines),
                        start: decorated_start,
                        header,
                    }
                }
            };
        visit(&lines, packet)?;
    }
    ControlFlow::Continue(())
}

/// A header speaks only when its marker and every token sit in authored prose.
fn position(quoted: bool, tokens: &[(usize, usize)], lines: &SourceLines) -> Position {
    if quoted
        || tokens
            .iter()
            .any(|&(token_start, _)| lines.suppressed(token_start))
    {
        Position::Quoted
    } else {
        Position::Placed
    }
}

/// One Markdown fenced block, closed only by its own character and width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence {
    marker: u8,
    length: usize,
}

impl Fence {
    fn opens(line: &str) -> Option<Self> {
        let (marker, length, _) = fence_run(line)?;
        Some(Self { marker, length })
    }

    fn closes(self, line: &str) -> bool {
        fence_run(line).is_some_and(|(marker, length, rest)| {
            marker == self.marker
                && length >= self.length
                && line.get(rest..).is_some_and(|tail| tail.trim().is_empty())
        })
    }
}

/// A backtick or tilde run of at least three characters after at most three spaces.
fn fence_run(line: &str) -> Option<(u8, usize, usize)> {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let marker = *bytes.get(indentation)?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = bytes[indentation..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (length >= 3).then_some((marker, length, indentation + length))
}

#[derive(Debug, Clone, Copy)]
struct SourceLine {
    start: usize,
    suppressed: bool,
}

struct SourceLines(Vec<SourceLine>);

impl SourceLines {
    fn read(prose: &str) -> Self {
        let mut lines = Vec::new();
        let mut fence: Option<Fence> = None;
        let mut start = 0;
        for terminated in prose.split_inclusive('\n') {
            let line = terminated.strip_suffix('\n').unwrap_or(terminated);
            let line = line.strip_suffix('\r').unwrap_or(line);
            let fence_line = Fence::opens(line);
            let suppressed = fence.is_some()
                || fence_line.is_some()
                || markdown_quote(line)
                || markdown_list(line)
                || indentation_width(
                    line.get(..line.len() - line.trim_start().len())
                        .unwrap_or_default(),
                ) >= 4
                || rendered_source_context(line);
            if let Some(open) = fence {
                if open.closes(line) {
                    fence = None;
                }
            } else if let Some(open) = fence_line {
                fence = Some(open);
            }
            lines.push(SourceLine { start, suppressed });
            start += terminated.len();
        }
        Self(lines)
    }

    fn line(&self, offset: usize) -> Option<(usize, SourceLine)> {
        let index = self
            .0
            .partition_point(|line| line.start <= offset)
            .checked_sub(1)?;
        Some((index, self.0[index]))
    }

    fn suppressed(&self, offset: usize) -> bool {
        self.line(offset).is_some_and(|(_, line)| line.suppressed)
    }

    fn locus(&self, offset: usize) -> Locus {
        let (index, line) = self
            .line(offset)
            .expect("token offset belongs to source line");
        Locus {
            line: index + 1,
            column: offset - line.start + 1,
        }
    }
}

fn markdown_quote(line: &str) -> bool {
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    indentation <= 3 && line.as_bytes().get(indentation) == Some(&b'>')
}

/// An ordered or unordered Markdown list item after at most three spaces.
fn markdown_list(line: &str) -> bool {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return false;
    }
    let item = &bytes[indentation..];
    if matches!(item.first(), Some(b'-' | b'+' | b'*')) {
        return item.get(1).is_some_and(|byte| matches!(byte, b' ' | b'\t'));
    }
    let digits = item.iter().take_while(|byte| byte.is_ascii_digit()).count();
    (1..=9).contains(&digits)
        && item
            .get(digits)
            .is_some_and(|byte| matches!(byte, b'.' | b')'))
        && item
            .get(digits + 1)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
}

fn rendered_source_context(line: &str) -> bool {
    let stripped = line.trim_start();
    let Some((path, rest)) = stripped.split_once(':') else {
        return false;
    };
    if path.is_empty()
        || !path
            .chars()
            .all(|character| character.is_alphanumeric() || "._/-".contains(character))
    {
        return false;
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest.as_bytes().get(digits) == Some(&b':')
}

/// Where a clause sat, which decides whether it speaks at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// Authored prose, so it speaks.
    Placed,
    /// Inside quoting, code, or rendered source context.
    Quoted,
}

/// One well-formed `MARKER key[ key...][separator]` clause.
struct Clause {
    marker: Marker,
    position: Position,
    start: usize,
    end: usize,
    separator: Option<char>,
    /// Each named token as a half-open byte range in the complete prose.
    tokens: Vec<(usize, usize)>,
}

impl Clause {
    /// The exact key run this packet names against current standing.
    ///
    /// The first syntactic key establishes that this is a control attempt, so
    /// an unknown first key still receives its truthful refusal. After that,
    /// only keys present in current standing extend the run; the first other
    /// token begins the optional body. This preserves ordinary same-line and
    /// multiline prose without requiring a colon.
    fn effective<'a>(
        &'a self,
        prose: &str,
        standing: &impl Standing,
    ) -> (&'a [(usize, usize)], usize, Option<char>) {
        let named = self
            .tokens
            .iter()
            .enumerate()
            .take_while(|&(index, &(start, end))| {
                prose[start..end]
                    .parse::<Key>()
                    .ok()
                    .is_some_and(|key| index == 0 || standing.disposition(&key).is_some())
            })
            .count();
        let tokens = &self.tokens[..named];
        if named == self.tokens.len() {
            return (tokens, self.end, self.separator);
        }
        let end = tokens
            .last()
            .map_or(self.start, |&(_, end)| skip_key_decoration(prose, end));
        (tokens, end, None)
    }

    /// Judges every token this clause named.
    fn judge(
        &self,
        prose: &str,
        lines: &SourceLines,
        standing: &impl Standing,
        tokens: &[(usize, usize)],
        into: &mut Vec<Recognition>,
    ) {
        for &(start, end) in tokens {
            let token = &prose[start..end];
            into.push(Recognition {
                locus: lines.locus(start),
                outcome: self.outcome(token, standing),
            });
        }
    }

    /// What one token did, given where its clause sat.
    fn outcome(&self, token: &str, standing: &impl Standing) -> Outcome {
        let missed = |miss| Outcome::Missed {
            token: token.to_owned(),
            miss,
        };
        if self.position == Position::Quoted {
            return missed(Miss::Quoted);
        }
        let Ok(key) = token.parse::<Key>() else {
            return missed(Miss::Malformed);
        };
        match standing.disposition(&key) {
            None => missed(Miss::Unknown),
            Some(Disposition::Live) => Outcome::Retired {
                marker: self.marker,
                key,
                disposition: self.marker.disposition(),
            },
            Some(_) => missed(Miss::Historical),
        }
    }
}

/// The protocol word a header leads with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lead {
    Retirement(Marker),
    Message,
}

/// The protocol word beginning at this offset, if a whole word begins there.
fn lead_at(prose: &str, start: usize) -> Option<Lead> {
    let leading = prose.get(..start)?;
    if leading
        .bytes()
        .next_back()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    let rest = prose.get(start..)?;
    if rest.starts_with(message::MARKER) {
        return Some(Lead::Message);
    }
    MARKERS
        .into_iter()
        .find(|marker| rest.starts_with(marker.as_str()))
        .map(Lead::Retirement)
}

struct Header {
    tokens: Vec<(usize, usize)>,
    end: usize,
    separator: Option<char>,
}

/// The shaped-key run and optional separator after one marker.
fn tokens_after(prose: &str, marker_end: usize) -> Option<Header> {
    let marker_boundary = skip_emphasis(prose, marker_end);
    let mut cursor = skip_header_space(prose, marker_boundary);
    if cursor == marker_boundary {
        return None;
    }

    let mut tokens = Vec::new();
    let mut header_end = cursor;
    loop {
        let token_start = skip_key_decoration(prose, cursor);
        let Some(end) = key_token_end(prose, token_start) else {
            break;
        };
        tokens.push((token_start, end));
        header_end = skip_key_decoration(prose, end);
        cursor = skip_header_space(prose, header_end);
        // A comma between keys lists them; before anything else it separates.
        if prose.as_bytes().get(cursor) == Some(&b',') {
            let listed = skip_header_space(prose, cursor + 1);
            if key_token_end(prose, skip_key_decoration(prose, listed)).is_some() {
                cursor = listed;
            }
        }
    }
    if tokens.is_empty() {
        return None;
    }

    let mut probe = skip_key_decoration(prose, header_end);
    probe = skip_header_space(prose, probe);
    let separator = prose.get(probe..)?.chars().next().filter(|character| {
        matches!(
            character,
            ':' | '.' | ',' | ';' | '!' | '?' | '\u{2013}' | '\u{2014}'
        )
    });
    if let Some(separator) = separator {
        probe += separator.len_utf8();
        probe = skip_key_decoration(prose, probe);
        header_end = probe;
    }

    Some(Header {
        tokens,
        end: header_end,
        separator,
    })
}

fn key_token_end(prose: &str, start: usize) -> Option<usize> {
    let end = start.checked_add(Key::WIDTH)?;
    let token = prose.get(start..end)?;
    if Key::shaped(token)
        && !prose
            .as_bytes()
            .get(end)
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        Some(end)
    } else {
        None
    }
}

fn skip_header_space(prose: &str, mut cursor: usize) -> usize {
    let mut crossed_line = false;
    while let Some(byte) = prose.as_bytes().get(cursor) {
        if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            break;
        }
        if *byte == b'\n' {
            if crossed_line {
                break;
            }
            crossed_line = true;
        }
        cursor += 1;
    }
    cursor
}

/// Markdown emphasis touching the front of a protocol header.
fn emphasis_start(prose: &str, marker_start: usize) -> usize {
    let mut start = marker_start;
    while prose
        .as_bytes()
        .get(start.wrapping_sub(1))
        .is_some_and(|byte| matches!(byte, b'*' | b'_'))
    {
        start -= 1;
    }
    start
}

fn skip_emphasis(prose: &str, mut cursor: usize) -> usize {
    while prose
        .as_bytes()
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b'*' | b'_'))
    {
        cursor += 1;
    }
    cursor
}

/// Markdown emphasis or a copied selector marker around one keyed-control key.
fn skip_key_decoration(prose: &str, mut cursor: usize) -> usize {
    while prose
        .as_bytes()
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b'*' | b'_' | b'#'))
    {
        cursor += 1;
    }
    cursor
}

fn intended_token(token: &str) -> bool {
    token
        .bytes()
        .any(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
}

fn inline_quoted(prose: &str, marker_start: usize, lines: &SourceLines) -> bool {
    let Some((_, line)) = lines.line(marker_start) else {
        return false;
    };
    let prefix = &prose[line.start..marker_start];
    if prefix
        .trim_end_matches([' ', '\t'])
        .ends_with(['`', '"', '\''])
    {
        return true;
    }
    ['`', '"'].into_iter().any(|quote| {
        prefix
            .chars()
            .filter(|character| *character == quote)
            .count()
            % 2
            == 1
    })
}

const CONTEXT_WINDOW: usize = 6;
/// Negations that are not an `n't` contraction, which [`negates`] reads by
/// its ending instead.
const NEGATION_WORDS: &[&str] = &[
    "cannot", "cant", "never", "not", "refuse", "refused", "refuses", "refusing", "will-not",
    "wont",
];
/// The `n't` ending of a negative contraction, with a straight or a
/// typographic apostrophe.
const CONTRACTED_NEGATIONS: &[&str] = &["n't", "n\u{2019}t"];
const PROHIBITION_WORDS: &[&str] = &[
    "avoid",
    "avoided",
    "avoiding",
    "avoids",
    "ban",
    "banned",
    "banning",
    "bans",
    "delete",
    "deleted",
    "deletes",
    "deleting",
    "eliminate",
    "eliminated",
    "eliminates",
    "eliminating",
    "forbid",
    "forbids",
    "forbidden",
    "forbidding",
    "prevent",
    "prevented",
    "preventing",
    "prevents",
    "remove",
    "removed",
    "removes",
    "removing",
];
const RELATION_BREAK_WORDS: &[&str] = &["by", "through", "using", "via", "with"];
const TURNING_WORDS: &[&str] = &["but", "hence", "so", "therefore", "thus"];
const HYPOTHETICAL_WORDS: &[&str] = &[
    "could",
    "hypothetically",
    "if",
    "should",
    "whether",
    "would",
];
const NARRATION_WORDS: &[&str] = &[
    "cite",
    "cited",
    "contain",
    "contained",
    "containing",
    "contains",
    "emit",
    "emits",
    "emitted",
    "emitting",
    "example",
    "examples",
    "form",
    "literal",
    "mention",
    "mentioned",
    "mentions",
    "narrated",
    "phrase",
    "quote",
    "quoted",
    "recognise",
    "recognised",
    "recognises",
    "recognising",
    "recognize",
    "recognized",
    "recognizes",
    "recognizing",
    "said",
    "say",
    "saying",
    "string",
    "token",
    "write",
    "writes",
    "writing",
    "wrote",
];

fn guarded_context(prose: &str, marker_start: usize) -> bool {
    let words = RecentWords::before(prose, marker_start);
    words.contains_negation()
        || words.contains_pair("instead", "of")
        || words.contains("instead-of")
        || ["refuse", "refused", "refuses", "refusing"]
            .into_iter()
            .any(|word| words.contains_pair(word, "to"))
        || words.direct_prohibition()
        || words.contains_any(HYPOTHETICAL_WORDS)
        || words.contains_any(NARRATION_WORDS)
}

struct RecentWords<'a> {
    words: [&'a str; CONTEXT_WINDOW],
    len: usize,
}

impl<'a> RecentWords<'a> {
    fn before(prose: &'a str, marker_start: usize) -> Self {
        let clause_start = prose[..marker_start]
            .rfind(['\r', '\n', '.', '!', '?', ';'])
            .map_or(0, |index| index + 1);
        let prefix = &prose[clause_start..marker_start];
        let mut words = Self {
            words: [""; CONTEXT_WINDOW],
            len: 0,
        };
        let mut cursor = 0;
        while cursor < prefix.len() {
            let Some(character) = prefix[cursor..].chars().next() else {
                break;
            };
            if !character.is_alphanumeric() {
                cursor += character.len_utf8();
                continue;
            }
            let start = cursor;
            cursor += character.len_utf8();
            while cursor < prefix.len() {
                let Some(character) = prefix[cursor..].chars().next() else {
                    break;
                };
                let joins = character.is_alphanumeric()
                    || matches!(character, '\'' | '-')
                    || (character == '\u{2019}' && contraction_apostrophe(prefix, cursor));
                if !joins {
                    break;
                }
                cursor += character.len_utf8();
            }
            words.push(&prefix[start..cursor]);
        }
        words
    }

    fn push(&mut self, word: &'a str) {
        if contains_word(TURNING_WORDS, word) {
            self.len = 0;
            return;
        }
        if self.len < CONTEXT_WINDOW {
            self.words[self.len] = word;
            self.len += 1;
        } else {
            self.words.rotate_left(1);
            self.words[CONTEXT_WINDOW - 1] = word;
        }
    }

    fn contains(&self, expected: &str) -> bool {
        self.words[..self.len]
            .iter()
            .any(|word| word.eq_ignore_ascii_case(expected))
    }

    fn contains_negation(&self) -> bool {
        self.words[..self.len].iter().any(|word| negates(word))
    }

    fn contains_any(&self, expected: &[&str]) -> bool {
        self.words[..self.len]
            .iter()
            .any(|word| contains_word(expected, word))
    }

    fn contains_pair(&self, first: &str, second: &str) -> bool {
        self.words[..self.len]
            .windows(2)
            .any(|pair| pair[0].eq_ignore_ascii_case(first) && pair[1].eq_ignore_ascii_case(second))
    }

    fn direct_prohibition(&self) -> bool {
        let words = &self.words[..self.len];
        for index in (0..words.len()).rev() {
            if contains_word(PROHIBITION_WORDS, words[index]) {
                return !words[index + 1..]
                    .iter()
                    .any(|word| contains_word(RELATION_BREAK_WORDS, word));
            }
        }
        false
    }
}

/// Whether the U+2019 at `at` is the apostrophe of a word's final `n’t`.
/// Anywhere else it is a closing quote or a possessive mark and ends the word,
/// so the guard word before it still reads bare.
fn contraction_apostrophe(prefix: &str, at: usize) -> bool {
    let mut following = prefix[at + '\u{2019}'.len_utf8()..].chars();
    prefix[..at].ends_with(['n', 'N'])
        && following
            .next()
            .is_some_and(|next| next.eq_ignore_ascii_case(&'t'))
        && !following.next().is_some_and(char::is_alphanumeric)
}

fn contains_word(words: &[&str], candidate: &str) -> bool {
    words
        .iter()
        .any(|word| candidate.eq_ignore_ascii_case(word))
}

/// Whether `word` is a negation: a listed one, or any contraction ending in
/// `n't`, so every auxiliary's negative form reads alike.
fn negates(word: &str) -> bool {
    contains_word(NEGATION_WORDS, word)
        || CONTRACTED_NEGATIONS.iter().any(|ending| {
            word.len()
                .checked_sub(ending.len())
                .filter(|start| *start > 0)
                .and_then(|start| word.get(start..))
                .is_some_and(|tail| tail.eq_ignore_ascii_case(ending))
        })
}

/// Markdown indentation columns, with a tab advancing to the next four-column stop.
fn indentation_width(leading: &str) -> usize {
    leading.chars().fold(0, |width, character| match character {
        ' ' => width + 1,
        '\t' => width + (4 - width % 4),
        _ if character.is_whitespace() => width + 1,
        _ => width,
    })
}
