//! Bounded resolution of authenticated transcript quote targets.

use std::ops::Range;
use std::path::{Path, PathBuf};

use pulldown_cmark::{CowStr, Event as MarkdownEvent, Parser, Tag, TagEnd};

use bus::BusReader;
use wire::principal::{NamedActor, PrincipalDirectory};
use wire::{
    Actor, CanonicalUuid, MediaQuote, QUOTE_DEPTH_LIMIT, QuoteAuthor, QuotePresentation,
    QuoteResolution, QuoteTarget, QuoteUnavailable, ResolvedMediaQuote, ResolvedQuote,
};

/// One fixed-label Markdown quote link and its exact source range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageQuote {
    /// Bytes occupied by the complete link in the original message.
    pub range: Range<usize>,
    /// Destination spelling supplied to [`Resolver::resolve`].
    pub target: String,
}

/// Losslessly parsed message text with its structural quote links.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuoteMessage<'a> {
    source: &'a str,
    quotes: Vec<MessageQuote>,
}

impl<'a> QuoteMessage<'a> {
    /// Recognize ordinary Markdown links whose complete visible label is `quote`.
    ///
    /// Messages without the quote URI authority bypass Markdown parsing and
    /// allocate nothing. Parsing never normalizes the source bytes.
    #[must_use]
    pub fn parse(source: &'a str) -> Self {
        if !source.contains("spice://quote/") {
            return Self {
                source,
                quotes: Vec::new(),
            };
        }
        let mut quotes = Vec::new();
        let mut open: Option<(usize, CowStr<'a>, bool)> = None;
        for (event, range) in Parser::new(source).into_offset_iter() {
            match event {
                MarkdownEvent::Start(Tag::Link { dest_url, .. }) => {
                    open = Some((range.start, dest_url, false));
                }
                MarkdownEvent::Text(label) if open.is_some() => {
                    let Some((_, _, admitted)) = open.as_mut() else {
                        unreachable!("the match guard observed an open link");
                    };
                    if !*admitted && label.as_ref() == "quote" {
                        *admitted = true;
                    } else {
                        open = None;
                    }
                }
                MarkdownEvent::End(TagEnd::Link) => {
                    if let Some((start, target, true)) = open.take()
                        && target.starts_with("spice://quote/")
                    {
                        quotes.push(MessageQuote {
                            range: start..range.end,
                            target: target.to_string(),
                        });
                    }
                }
                _ if open.is_some() => open = None,
                _ => {}
            }
        }
        Self { source, quotes }
    }

    /// Exact original Markdown, unchanged by structural recognition.
    #[must_use]
    pub const fn render(&self) -> &'a str {
        self.source
    }

    /// Quote links in message order.
    #[must_use]
    pub fn quotes(&self) -> &[MessageQuote] {
        &self.quotes
    }
}

/// Resolver over the repository's durable tape and journal authority.
pub struct Resolver<'a> {
    store: ResidentStore<'a>,
}
/// Resident-owned reconstruction of the app prose presented for one tape event.
pub type AgentProsePresenter =
    fn(&BusReader, CanonicalUuid, CanonicalUuid, u32, &listen::Prose) -> Result<String, String>;

impl<'a> Resolver<'a> {
    /// Bind quote reads to one admitted tape root and Bus snapshot source.
    #[must_use]
    pub fn new(paths: &'a crate::repository::StatePaths, bus: &'a BusReader) -> Self {
        Self::from_tape(paths.tape(), bus)
    }

    /// Bind quote reads to an already admitted resident tape root.
    #[must_use]
    pub const fn from_tape(tape: &'a Path, bus: &'a BusReader) -> Self {
        Self {
            store: ResidentStore {
                tape,
                bus,
                agent_presenter: None,
            },
        }
    }

    /// Bind quote reads with the resident's authoritative agent-card presentation.
    #[must_use]
    pub const fn from_tape_with_agent_presenter(
        tape: &'a Path,
        bus: &'a BusReader,
        agent_presenter: AgentProsePresenter,
    ) -> Self {
        Self {
            store: ResidentStore {
                tape,
                bus,
                agent_presenter: Some(agent_presenter),
            },
        }
    }

    /// Expand exactly one target at the caller's current nesting depth.
    ///
    /// The returned text is never scanned recursively. A caller that finds a
    /// quote link in it invokes this method again with the depth advanced by one.
    #[must_use]
    pub fn resolve(&self, target: &str, depth: u8) -> QuoteResolution {
        resolve_with(&self.store, target, depth)
    }

    /// Resolve every fixed-label quote link into a complete presentation tree.
    #[must_use]
    pub fn present(&self, text: &str) -> Vec<QuotePresentation> {
        present_with(&self.store, text, 0)
    }

    /// Replace every structural quote link with authenticated source text.
    ///
    /// Nested links are followed through the shared depth bound. A refusal is
    /// rendered explicitly in place, so delivery never silently omits source
    /// text and never asks the recipient to resolve authority itself.
    #[must_use]
    pub fn expand(&self, source: &str) -> String {
        expand_with(&self.store, source, 0)
    }
}

/// App presentation of every quote link in `text`, recursively.
///
/// Each resolved node shows its source's referenced media as inline
/// Markdown, exactly as a directive context does, while nested quote
/// discovery reads the raw authored text.
fn present_with(store: &impl QuoteStore, text: &str, depth: u8) -> Vec<QuotePresentation> {
    QuoteMessage::parse(text)
        .quotes()
        .iter()
        .map(|quote| {
            let presented = admitted_target(&quote.target, depth).and_then(|target| {
                if target.is_media() {
                    let (source, media) = media_source(store, &target)?;
                    return Ok(QuotePresentation::Media {
                        target: quote.target.clone(),
                        media,
                        attribution: source.attribution,
                        disposition: source.disposition,
                    });
                }
                let source = resolve_text_source(store, &target)?;
                let media_quotes = media_quotes(&target, &source.media)?;
                let SourceQuote {
                    text,
                    attribution,
                    disposition,
                    media,
                    body,
                    ..
                } = source;
                let text = body.unwrap_or(text);
                let quotes = present_with(
                    store,
                    &text,
                    depth.checked_add(1).unwrap_or(QUOTE_DEPTH_LIMIT),
                );
                let inlined = wire::media::with_media_payloads(&text, &media)
                    .map_err(|_| QuoteUnavailable::Unreadable)?;
                Ok(QuotePresentation::Resolved {
                    target: quote.target.clone(),
                    text: inlined.into_owned(),
                    attribution,
                    disposition,
                    quotes,
                    media_quotes,
                })
            });
            presented.unwrap_or_else(|reason| QuotePresentation::Unavailable {
                target: quote.target.clone(),
                reason,
            })
        })
        .collect()
}

/// Derive one exact media target per retained image on a text quote source.
///
/// # Errors
///
/// Returns unreadable when the source kind cannot own media or retained bytes
/// no longer satisfy media admission.
pub fn media_quotes(
    source: &QuoteTarget,
    media: &[wire::Media],
) -> Result<Vec<MediaQuote>, QuoteUnavailable> {
    media
        .iter()
        .enumerate()
        .map(|(ordinal, media)| {
            let ordinal = u8::try_from(ordinal).map_err(|_| QuoteUnavailable::Unreadable)?;
            let digest = media.identity().map_err(|_| QuoteUnavailable::Unreadable)?;
            let target = match source {
                QuoteTarget::Tape {
                    lane,
                    source,
                    locus,
                    ..
                } => QuoteTarget::TapeMedia {
                    lane: *lane,
                    source: *source,
                    locus: *locus,
                    ordinal,
                    digest,
                },
                QuoteTarget::Steering { key, .. } => QuoteTarget::SteeringMedia {
                    key: key.clone(),
                    ordinal,
                    digest,
                },
                QuoteTarget::Room { .. }
                | QuoteTarget::TapeMedia { .. }
                | QuoteTarget::SteeringMedia { .. } => {
                    return Err(QuoteUnavailable::Unreadable);
                }
            };
            Ok(MediaQuote {
                target,
                name: media.name.clone(),
            })
        })
        .collect()
}

/// One steering directive as every presentation of it reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteeringSource {
    /// Who said the directive and whom it reached.
    pub attribution: wire::QuoteAttribution,
    /// Authenticated body of the publication that opened the directive, shown
    /// without the header its recipient read.
    pub body: Option<String>,
}

/// Derive `row`'s attribution once from its own record.
///
/// A peer message names the exact occupant that wrote it, so a later occupant
/// of the sending lane never inherits its authorship; any other directive
/// names the principal on its creating fact. The recipient is the lane the
/// directive was delivered to, with the occupant a peer message's route
/// proves was seated there. An opening room publication or a peer directive
/// is a SYN packet. Any other directive is no packet, whatever its words
/// spell.
#[must_use]
pub fn steering_source(row: &bus::SteeringPresentation) -> SteeringSource {
    let created = &row.created().fact;
    let messaged = created.read::<wire::body::SteerMessaged>();
    let peer = messaged
        .as_ref()
        .map(|message| (message.from, message.sender))
        .or_else(|| {
            created
                .read::<wire::body::SteerQueued>()
                .map(|message| (message.from, message.sender))
        });
    let author = peer.map_or_else(
        || QuoteAuthor::Directive {
            actor: row.created().actor.clone(),
        },
        |(lane, occupant)| QuoteAuthor::Agent { lane, occupant },
    );
    let recipients = match row.head().body().recipient {
        Actor::Lane(lane) => vec![wire::QuoteRecipient {
            lane,
            occupant: messaged.map(|message| message.occupant),
        }],
        Actor::Spice | Actor::Principal(_) => Vec::new(),
    };
    let body = row
        .publication()
        .and_then(|publication| publication.text().map(str::to_owned));
    SteeringSource {
        attribution: wire::QuoteAttribution {
            author,
            recipients,
            packet: row.publication().is_some() || peer.is_some(),
        },
        body,
    }
}

/// Bind the resident quote authority into app serialization.
#[must_use]
pub fn presenter(
    tape: PathBuf,
    bus: BusReader,
    agent_presenter: AgentProsePresenter,
) -> serve::stream::QuotePresenter {
    serve::stream::QuotePresenter::new(move |text| {
        Resolver::from_tape_with_agent_presenter(&tape, &bus, agent_presenter).present(text)
    })
}

/// Every born principal's recorded name in the journal at `journal`, when a
/// principal directive authored `resolution`. A quote any other actor wrote,
/// or none, needs no names and reads nothing.
///
/// # Errors
///
/// Returns why the journal or its principal index cannot be read.
pub fn author_names(
    journal: &Path,
    resolution: &QuoteResolution,
) -> Result<Option<PrincipalDirectory>, String> {
    let author = match resolution {
        QuoteResolution::Resolved(quote) => &quote.author,
        QuoteResolution::Media(quote) => &quote.author,
        QuoteResolution::Unavailable { .. } => return Ok(None),
    };
    if !matches!(
        author,
        QuoteAuthor::Directive {
            actor: Actor::Principal(_)
        }
    ) {
        return Ok(None);
    }
    let reader = journal::JournalReader::open(journal)
        .map_err(|error| format!("cannot open the journal to name the quote's author: {error}"))?;
    reader
        .read(|read| read.principal_roles())
        .map(|roles| Some(PrincipalDirectory::codes(&roles)))
        .map_err(|error| format!("cannot read the principal names a quote's author takes: {error}"))
}

/// One resolution as a reader receives it: the recorded author, a human
/// beside the name `principals` record, then the exact authenticated text.
///
/// # Errors
///
/// Returns the in-place refusal steering delivery renders when the target
/// names no quotable source.
pub fn render(
    resolution: &QuoteResolution,
    principals: Option<&PrincipalDirectory>,
) -> Result<String, String> {
    match resolution {
        QuoteResolution::Resolved(quote) => {
            let author = render_author(&quote.author, principals);
            Ok(format!("Quoted from {author}:\n\n{}", quote.text))
        }
        QuoteResolution::Media(quote) => {
            let author = render_author(&quote.author, principals);
            let markdown = quote.media.markdown().map_err(|error| error.to_string())?;
            Ok(format!("Quoted image from {author}:\n\n{markdown}"))
        }
        QuoteResolution::Unavailable { target, reason } => {
            let mut refusal = String::new();
            render_unavailable(&mut refusal, target, *reason);
            Err(refusal)
        }
    }
}

fn render_author(author: &QuoteAuthor, principals: Option<&PrincipalDirectory>) -> String {
    match author {
        QuoteAuthor::Agent { lane, occupant } => {
            format!("agent occupant {occupant} of lane {lane}")
        }
        QuoteAuthor::Directive { actor } => principals
            .map_or(NamedActor { actor, code: None }, |principals| {
                principals.named(actor)
            })
            .to_string(),
    }
}

/// What a text quote target's digest authenticates on one source.
#[derive(Clone, Debug, Eq, PartialEq)]
enum TextIdentity {
    /// The presented `text` is itself the digested prose.
    Presented,
    /// A presenter rewrote `text`; the target digests this authored prose.
    Authored(String),
    /// The source carries media but no quoteable prose, so no text target
    /// names it; only its media targets resolve.
    Absent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceQuote {
    identity: TextIdentity,
    text: String,
    attribution: wire::QuoteAttribution,
    disposition: Option<wire::Disposition>,
    /// Admitted media the source carries, which its text may reference by
    /// `attachment:` identity.
    media: Vec<wire::Media>,
    /// A packet's authenticated body, presented in place of `text` once
    /// `text` authenticates the target.
    body: Option<String>,
}

/// Attribution of words that reached no recorded lane: transcript prose and
/// room publications.
fn authored_by(author: QuoteAuthor) -> wire::QuoteAttribution {
    wire::QuoteAttribution {
        author,
        recipients: Vec::new(),
        packet: false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceError {
    Unknown,
    Unreadable,
}

trait QuoteStore {
    fn tape(
        &self,
        lane: CanonicalUuid,
        source: CanonicalUuid,
        locus: wire::MessageLocus,
    ) -> Result<SourceQuote, SourceError>;

    fn steering(&self, key: &str) -> Result<SourceQuote, SourceError>;

    fn room(
        &self,
        room: &wire::TaskStamp,
        publication: &wire::rooms::RoomAuthorityStamp,
    ) -> Result<SourceQuote, SourceError>;
}

struct ResidentStore<'a> {
    tape: &'a Path,
    bus: &'a BusReader,
    agent_presenter: Option<AgentProsePresenter>,
}

impl QuoteStore for ResidentStore<'_> {
    fn tape(
        &self,
        lane: CanonicalUuid,
        source: CanonicalUuid,
        locus: wire::MessageLocus,
    ) -> Result<SourceQuote, SourceError> {
        let record_locus = tape::Locus::new(locus.member, locus.offset, locus.length);
        let session = self
            .bus
            .transcript_session(lane, source, record_locus.member, record_locus.offset)
            .map_err(|_| SourceError::Unreadable)?
            .ok_or(SourceError::Unknown)?;
        let dialect = crate::extension::VENDORS
            .iter()
            .find(|entry| entry.vendor.name() == session.vendor)
            .map(|entry| entry.vendor.dialect())
            .ok_or(SourceError::Unknown)?;
        let root = crate::repository::lane_tape(self.tape, lane).join(&session.vendor);
        self.resolve_tape(&root, source, locus, dialect, lane, session.occupant)
    }

    fn steering(&self, key: &str) -> Result<SourceQuote, SourceError> {
        let mut presentations = self
            .bus
            .steering_presentations(vec![key.to_owned()])
            .map_err(|_| SourceError::Unreadable)?;
        let presentation = presentations.pop().ok_or(SourceError::Unknown)?;
        let head = presentation.head().body();
        if head.key != key {
            return Err(SourceError::Unknown);
        }
        let SteeringSource { attribution, body } = steering_source(&presentation);
        Ok(SourceQuote {
            identity: TextIdentity::Presented,
            text: head.text.clone(),
            attribution,
            disposition: Some(head.disposition),
            media: head.attachments.clone(),
            body,
        })
    }

    /// An operator post quotes as its directive-authored text; a lane packet
    /// quotes as the exact transcript hunk its occupant wrote. A leave carries
    /// no authored words and quotes nothing.
    fn room(
        &self,
        room: &wire::TaskStamp,
        publication: &wire::rooms::RoomAuthorityStamp,
    ) -> Result<SourceQuote, SourceError> {
        let envelope = self
            .bus
            .room_publication(room.clone(), publication.clone())
            .map_err(|_| SourceError::Unreadable)?
            .ok_or(SourceError::Unknown)?;
        let actor = envelope.actor;
        let authored = |text, packet| SourceQuote {
            identity: TextIdentity::Presented,
            text,
            attribution: wire::QuoteAttribution {
                packet,
                ..authored_by(QuoteAuthor::Directive {
                    actor: actor.clone(),
                })
            },
            disposition: None,
            media: Vec::new(),
            body: None,
        };
        if let Some(posted) = envelope.fact.read::<wire::rooms::RoomPosted>() {
            return Ok(authored(posted.text, true));
        }
        if let Some(published) = envelope.fact.read::<wire::rooms::RoomPublished>()
            && let wire::rooms::RoomPublicationContent::Authored { text } = published.content
        {
            return Ok(authored(text, false));
        }
        let messaged = envelope
            .fact
            .read::<wire::rooms::RoomMessaged>()
            .ok_or(SourceError::Unknown)?;
        let text =
            room_hunk_text(self.tape, &messaged.hunk).map_err(|unavailable| match unavailable {
                wire::rooms::RoomSourceUnavailable::Unreadable {} => SourceError::Unreadable,
                _ => SourceError::Unknown,
            })?;
        Ok(SourceQuote {
            identity: TextIdentity::Presented,
            text,
            attribution: authored_by(QuoteAuthor::Agent {
                lane: messaged.hunk.head.lane,
                occupant: messaged.author,
            }),
            disposition: None,
            media: Vec::new(),
            body: None,
        })
    }
}

fn resolve_with(store: &impl QuoteStore, target: &str, depth: u8) -> QuoteResolution {
    let parsed = match admitted_target(target, depth) {
        Ok(parsed) => parsed,
        Err(reason) => return unavailable(target, reason),
    };
    if parsed.is_media() {
        return match media_source(store, &parsed) {
            Ok((source, media)) => QuoteResolution::Media(ResolvedMediaQuote {
                media,
                author: source.attribution.author,
                disposition: source.disposition,
            }),
            Err(reason) => unavailable(target, reason),
        };
    }
    match resolve_text_source(store, &parsed) {
        Ok(source) => QuoteResolution::Resolved(ResolvedQuote {
            text: source.text,
            author: source.attribution.author,
            disposition: source.disposition,
        }),
        Err(reason) => unavailable(target, reason),
    }
}

fn admitted_target(target: &str, depth: u8) -> Result<QuoteTarget, QuoteUnavailable> {
    if depth >= QUOTE_DEPTH_LIMIT {
        return Err(QuoteUnavailable::DepthExceeded);
    }
    target
        .parse::<QuoteTarget>()
        .map_err(|_| QuoteUnavailable::Malformed)
}

fn source_for(
    store: &impl QuoteStore,
    target: &QuoteTarget,
) -> Result<SourceQuote, QuoteUnavailable> {
    let source = match target {
        QuoteTarget::Tape {
            lane,
            source,
            locus,
            ..
        }
        | QuoteTarget::TapeMedia {
            lane,
            source,
            locus,
            ..
        } => store.tape(*lane, *source, *locus),
        QuoteTarget::Steering { key, .. } | QuoteTarget::SteeringMedia { key, .. } => {
            if key.parse::<steer::Key>().is_err() {
                return Err(QuoteUnavailable::Malformed);
            }
            store.steering(key)
        }
        QuoteTarget::Room {
            room, publication, ..
        } => store.room(room, publication),
    };
    source.map_err(|error| match error {
        SourceError::Unknown => QuoteUnavailable::Unknown,
        SourceError::Unreadable => QuoteUnavailable::Unreadable,
    })
}

fn resolve_text_source(
    store: &impl QuoteStore,
    target: &QuoteTarget,
) -> Result<SourceQuote, QuoteUnavailable> {
    if target.is_media() {
        return Err(QuoteUnavailable::Malformed);
    }
    let resolved = source_for(store, target)?;
    let identity = match &resolved.identity {
        TextIdentity::Presented => resolved.text.as_str(),
        TextIdentity::Authored(prose) => prose.as_str(),
        TextIdentity::Absent => return Err(QuoteUnavailable::Unknown),
    };
    if QuoteTarget::text_digest(identity) != *target.digest() {
        return Err(QuoteUnavailable::DigestMismatch);
    }
    Ok(resolved)
}

/// The source owning a media target and the exact image it selects.
fn media_source(
    store: &impl QuoteStore,
    target: &QuoteTarget,
) -> Result<(SourceQuote, wire::Media), QuoteUnavailable> {
    let ordinal = match target {
        QuoteTarget::TapeMedia { ordinal, .. } | QuoteTarget::SteeringMedia { ordinal, .. } => {
            usize::from(*ordinal)
        }
        _ => return Err(QuoteUnavailable::Malformed),
    };
    let source = source_for(store, target)?;
    let media = source
        .media
        .get(ordinal)
        .ok_or(QuoteUnavailable::Unknown)?
        .clone();
    let identity = media.identity().map_err(|_| QuoteUnavailable::Unreadable)?;
    if identity != *target.digest() {
        return Err(QuoteUnavailable::DigestMismatch);
    }
    Ok((source, media))
}

fn expand_with(store: &impl QuoteStore, source: &str, depth: u8) -> String {
    let message = QuoteMessage::parse(source);
    if message.quotes().is_empty() {
        return source.to_owned();
    }
    let mut expanded = String::with_capacity(source.len());
    let mut cursor = 0;
    for quote in message.quotes() {
        expanded.push_str(&source[cursor..quote.range.start]);
        match resolve_with(store, &quote.target, depth) {
            QuoteResolution::Resolved(resolved) => {
                expanded.push_str(&expand_with(store, &resolved.text, depth + 1));
            }
            QuoteResolution::Media(resolved) => match resolved.media.markdown() {
                Ok(markdown) => expanded.push_str(&markdown),
                Err(_) => {
                    render_unavailable(&mut expanded, &quote.target, QuoteUnavailable::Unreadable)
                }
            },
            QuoteResolution::Unavailable { target, reason } => {
                render_unavailable(&mut expanded, &target, reason);
            }
        }
        cursor = quote.range.end;
    }
    expanded.push_str(&source[cursor..]);
    expanded
}

fn render_unavailable(rendered: &mut String, target: &str, reason: QuoteUnavailable) {
    use std::fmt::Write as _;

    let reason = match reason {
        QuoteUnavailable::Malformed => "malformed",
        QuoteUnavailable::Unknown => "unknown",
        QuoteUnavailable::Unreadable => "unreadable",
        QuoteUnavailable::DigestMismatch => "digest-mismatch",
        QuoteUnavailable::DepthExceeded => "depth-exceeded",
    };
    write!(
        rendered,
        "[quote unavailable: reason={reason} source={target:?}]"
    )
    .expect("writing to a String cannot fail");
}

fn unavailable(target: &str, reason: QuoteUnavailable) -> QuoteResolution {
    QuoteResolution::Unavailable {
        target: target.to_owned(),
        reason,
    }
}

/// Why one durable vendor record cannot be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordError {
    /// No tape, source identity, or member holds the record.
    Missing,
    /// The directory now belongs to another tape generation.
    Replaced,
    /// The recorded range extends beyond the retained member.
    Truncated,
    /// The retained bytes cannot be decoded in their declared dialect.
    Undecodable,
    /// Storage exists but cannot be read now.
    Unreadable,
}

/// Decode the durable vendor record at `locus` of tape generation `source`
/// under `root`, one lane and vendor's tape directory.
///
/// # Errors
///
/// Returns the precise absence, generation, extent, decoding or storage
/// failure without creating or repairing any tape state.
pub fn decode_record(
    root: &Path,
    source: CanonicalUuid,
    locus: wire::MessageLocus,
    dialect: listen::Dialect,
) -> Result<listen::Decoded, RecordError> {
    let tape = tape::Tape::open_existing(root).map_err(|error| match error {
        tape::Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            RecordError::Missing
        }
        _ => RecordError::Unreadable,
    })?;
    let actual_source = tape.source_id().map_err(record_storage_error)?;
    if actual_source.as_str() != source.to_string() {
        return Err(RecordError::Replaced);
    }
    let record_locus = tape::Locus::new(locus.member, locus.offset, locus.length);
    let bytes = tape.read(record_locus).map_err(record_storage_error)?;
    listen::decode(dialect, &bytes, record_locus).map_err(|_| RecordError::Undecodable)
}

fn record_storage_error(error: tape::Error) -> RecordError {
    match error {
        tape::Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            RecordError::Missing
        }
        tape::Error::Io { source, .. } if source.kind() == std::io::ErrorKind::UnexpectedEof => {
            RecordError::Truncated
        }
        tape::Error::Refused(
            tape::Refusal::UnknownMember { .. } | tape::Refusal::MemberVanished { .. },
        ) => RecordError::Missing,
        tape::Error::Refused(tape::Refusal::OutOfRange { .. }) => RecordError::Truncated,
        _ => RecordError::Unreadable,
    }
}

/// The visible assistant text block a room packet may be published from:
/// one complete streamed assistant text block, exactly as the agent wrote it.
///
/// Settled message prose repeats or paraphrases that block, reasoning is not
/// visible text, and a tool call's intent and arguments are call metadata, so
/// none of them is a packet source.
#[must_use]
pub fn packet_text(event: &listen::Event) -> Option<&str> {
    match event {
        listen::Event::StreamedProse(prose) if prose.role == listen::ProseRole::Assistant => {
            Some(prose.text.as_str())
        }
        _ => None,
    }
}

/// The exact assistant words one room transcript hunk names, read from its
/// lane's durable tape under `tape`.
///
/// # Errors
///
/// Returns why the tape no longer supplies those bytes.
pub fn room_hunk_text(
    tape: &Path,
    hunk: &wire::rooms::TranscriptHunk,
) -> Result<String, wire::rooms::RoomSourceUnavailable> {
    use wire::rooms::RoomSourceUnavailable;

    let dialect = crate::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == hunk.head.vendor.as_str())
        .map(|entry| entry.vendor.dialect())
        .ok_or(RoomSourceUnavailable::Unreadable {})?;
    let root = crate::repository::lane_tape(tape, hunk.head.lane).join(hunk.head.vendor.as_str());
    let decoded =
        decode_record(&root, hunk.head.source, hunk.locus, dialect).map_err(
            |error| match error {
                RecordError::Missing | RecordError::Replaced | RecordError::Truncated => {
                    RoomSourceUnavailable::Missing {}
                }
                RecordError::Unreadable | RecordError::Undecodable => {
                    RoomSourceUnavailable::Unreadable {}
                }
            },
        )?;
    if hunk.locus.event == 0
        && let Some(text) = crate::outbox_source::captured_text(&decoded)
    {
        return usize::try_from(hunk.text.start)
            .ok()
            .zip(usize::try_from(hunk.text.end).ok())
            .and_then(|(start, end)| text.get(start..end))
            .map(str::to_owned)
            .ok_or(RoomSourceUnavailable::Changed {});
    }
    hunk_text(decoded.events(), hunk.locus.event, hunk.text)
}

/// The cited bytes of one decoded record's events, or `changed` when the
/// named event no longer carries packet text spanning that range.
fn hunk_text(
    events: &[listen::Event],
    event: u32,
    range: wire::rooms::TextRange,
) -> Result<String, wire::rooms::RoomSourceUnavailable> {
    let text = usize::try_from(event)
        .ok()
        .and_then(|event| events.get(event))
        .and_then(packet_text);
    let range = usize::try_from(range.start)
        .ok()
        .zip(usize::try_from(range.end).ok());
    text.zip(range)
        .and_then(|(text, (start, end))| text.get(start..end))
        .map(str::to_owned)
        .ok_or(wire::rooms::RoomSourceUnavailable::Changed {})
}

impl ResidentStore<'_> {
    /// Quote one event of `lane`'s tape record, attributed to `occupant`.
    fn resolve_tape(
        &self,
        root: &Path,
        source: CanonicalUuid,
        locus: wire::MessageLocus,
        dialect: listen::Dialect,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    ) -> Result<SourceQuote, SourceError> {
        let decoded = decode_record(root, source, locus, dialect).map_err(|error| match error {
            RecordError::Missing | RecordError::Replaced | RecordError::Truncated => {
                SourceError::Unknown
            }
            RecordError::Unreadable | RecordError::Undecodable => SourceError::Unreadable,
        })?;
        let decoded = if crate::outbox_source::captured_text(&decoded).is_some() {
            let submission = self
                .bus
                .outbox(bus::OutboxQuery::Source {
                    lane,
                    source,
                    member: locus.member,
                    offset: locus.offset,
                })
                .map_err(|_| SourceError::Unreadable)?
                .pop()
                .filter(|submission| {
                    submission.body.occupant == occupant && submission.body.hunk.locus == locus
                })
                .ok_or(SourceError::Unknown)?;
            crate::outbox_source::materialize(decoded, &submission.body)
                .map_err(|_| SourceError::Unreadable)?
        } else {
            decoded
        };
        let event = usize::try_from(locus.event)
            .ok()
            .and_then(|event| decoded.events().get(event))
            .ok_or(SourceError::Unknown)?;
        let media = event_media(event).to_vec();
        let (identity, text) = if let Some(prose) = crate::agent_prose::quoteable(event) {
            let text = self
                .agent_presenter
                .map_or_else(
                    || Ok(prose.text.clone()),
                    |present| present(self.bus, lane, source, locus.event, prose),
                )
                .map_err(|_| SourceError::Unreadable)?;
            (TextIdentity::Authored(prose.text.clone()), text)
        } else if let listen::Event::Failure(failure) = event
            && !failure.message.trim().is_empty()
        {
            (
                TextIdentity::Authored(failure.message.clone()),
                failure.message.clone(),
            )
        } else if media.is_empty() {
            return Err(SourceError::Unknown);
        } else {
            (TextIdentity::Absent, String::new())
        };
        Ok(SourceQuote {
            identity,
            text,
            attribution: authored_by(QuoteAuthor::Agent { lane, occupant }),
            disposition: None,
            media,
            body: None,
        })
    }
}

fn event_media(event: &listen::Event) -> &[wire::Media] {
    match event {
        listen::Event::Prose(prose) | listen::Event::StreamedProse(prose) => &prose.media,
        listen::Event::Tool(listen::Tool {
            moment: listen::ToolMoment::Result(outcome),
            ..
        }) => &outcome.media,
        _ => &[],
    }
}

#[cfg(test)]
mod tests;
