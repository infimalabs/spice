//! `spice room history HANDLE`: one anchor task's room, oldest publication first.
//!
//! The handle resolves by birth stamp, so every spelling a re-homed task has
//! carried reads the same room, and the page names the current one. Each
//! publication is projected from the authority that owns its bytes: principal
//! text from its journal fact, an agent packet from its author's durable tape,
//! and each input it routed, to a lane or a principal alike, from that input's
//! steering head: its line disposition and, once per body, the body that
//! answered it. A typed contribution's text is its own fact, and each source it
//! cites is resolved through [`RoomSources`]. A source that no longer yields
//! the bytes renders as unavailable with its reason. Publications page whole in
//! journal order through [`Page`]; a page that withholds any continues at the
//! first withheld publication, and a shown page also groups its contributions
//! by kind. `--contributions` pages only contributions. Every read also lists
//! the anchor's operator decisions in decision order. A principal actor reads
//! as its immutable code. Reading writes nothing.

pub(crate) mod awareness;
pub(crate) mod git_source;
pub(crate) mod inbox;
pub(crate) mod input;
mod newest;
pub(crate) mod sections;
mod sources;
pub(crate) mod unread;

pub(crate) use newest::{NewestRows, newest_rows};
use sources::preceding_fact;
pub(crate) use sources::{RoomSources, SourceOwners};

use std::fmt;
use std::path::Path;

use clap::ArgMatches;
use serde::Serialize;
use spice::repository::Repository;
use wire::output::{Bounded, Export};
use wire::principal::{NamedActor, PrincipalCode, PrincipalDirectory};
use wire::rooms::{
    PrincipalInputRef, RoomAuthorityStamp, RoomContributed, RoomContributionKind, RoomLeft,
    RoomMessaged, RoomPosted, RoomPublicationContent, RoomPublished, RoomSource,
    RoomSourceUnavailable, RoomSourceView, StatementItem, TranscriptHunk,
};
use wire::{
    Actor, CanonicalUuid, Disposition, Fault, FaultClass, Instant, Outcome, Refusal, Reply,
    ReplyPosition, Seq, TaskHandle, TaskStamp,
};

use crate::read_page::{Page, Paged};

/// What a continuation's `--after` names.
const CURSOR: &str = "a publication of this room";

/// An actor a room read shows, beside the immutable code of the principal it
/// resolves to. It serializes as the actor alone, so JSON keeps the identity
/// contract; text reads it as [`NamedActor`].
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub(crate) struct Named {
    actor: Actor,
    #[serde(skip)]
    code: Option<PrincipalCode>,
}

impl Named {
    fn of(principals: &PrincipalDirectory, actor: Actor) -> Self {
        let code = principals.named(&actor).code;
        Self { actor, code }
    }
}

impl fmt::Display for Named {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        NamedActor {
            actor: &self.actor,
            code: self.code,
        }
        .fmt(formatter)
    }
}

/// One page of a room, or the receipt of a page exported whole.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum RoomHistory {
    Shown {
        room: TaskHandle,
        anchor: TaskStamp,
        page: Bounded<Publication>,
        /// The page's contributions by kind, each in journal order.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        contributions: Vec<sections::ContributionGroup>,
        /// The anchor's operator decisions, in decision order.
        #[serde(skip_serializing_if = "sections::Decisions::is_empty")]
        decisions: sections::Decisions,
    },
    Exported {
        room: TaskHandle,
        anchor: TaskStamp,
        export: Export,
    },
}

/// One publication as its owning authorities state it.
#[derive(Debug, Serialize)]
pub(crate) struct Publication {
    publication: RoomAuthorityStamp,
    seq: Seq,
    at: Instant,
    /// The operator, or the lane whose agent authored the packet.
    actor: Named,
    /// Occupant whose transcript authored an agent packet.
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<CanonicalUuid>,
    content: Content,
    /// Each input the publication routed into this room, to a lane or a
    /// principal, with its line disposition.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    directed: Vec<Route>,
    /// Whether an agent packet only recorded on this room: it routed no input,
    /// so it owes no one attention and is closed as recorded.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    closed: bool,
}

/// A publication's bytes, or why its owning source cannot supply them.
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum Content {
    Text {
        text: String,
    },
    /// A journal fact or artifact the publication cites rather than quotes.
    Linked {
        source: RoomSource,
    },
    /// The authoring lane left the room for this reason.
    Left {
        clause: String,
    },
    /// The authoring lane's typed problem-space contribution.
    Contribution {
        kind: RoomContributionKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        question: Option<QuestionView>,
        text: String,
        sources: Vec<RoomSourceView>,
    },
    Unavailable {
        source: RoomSource,
        unavailable: RoomSourceUnavailable,
    },
}

/// The Design question item a contribution answers. Its handle is the
/// item's rendered prefix among the current statement, and its text is shown
/// only while the statement the post witnessed is still the Design's current
/// one, because the journal keeps no superseded statement text.
#[derive(Debug, Serialize)]
struct QuestionView {
    item: StatementItem,
    handle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

/// One directed input this publication opened and its line disposition.
#[derive(Debug, Serialize)]
struct Route {
    directive: CanonicalUuid,
    receipt: Receipt,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum Receipt {
    /// The input's current steering standing, for either endpoint kind.
    Steering {
        key: String,
        recipient: Named,
        disposition: Disposition,
        /// Where the input stands on its line, by key.
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<Box<wire::intent::LineStanding>>,
        /// The body that answered it, shown under the first input that body
        /// covered so a room shows each body exactly once.
        #[serde(skip_serializing_if = "Option::is_none")]
        answer: Option<Box<Answer>>,
    },
    /// The journal holds no steering head for the directive.
    Missing,
}

/// One line body as its own steering input records it.
#[derive(Debug, Serialize)]
struct Answer {
    key: String,
    body: wire::body::PeerBodyKind,
    text: String,
}

/// Read one bounded page of a room from the working repository.
pub(crate) fn run(matches: &ArgMatches, contract: &crate::cli::Contract) -> Outcome<RoomHistory> {
    match discover() {
        Ok(repository) => run_at(&repository, matches, contract),
        Err(failure) => failure.outcome(),
    }
}

/// The repository the working directory belongs to.
fn discover() -> Result<Repository, Failure> {
    std::env::current_dir()
        .map_err(|error| {
            fault(
                FaultClass::Environment,
                "room::working-directory-unavailable",
                error,
            )
        })
        .and_then(|working| {
            Repository::discover(&working).map_err(|error| {
                fault(
                    FaultClass::Environment,
                    spice::repository::ResolveError::REASON,
                    error,
                )
            })
        })
}

/// The admitted state and journal reader of `repository`.
fn open(
    repository: &Repository,
) -> Result<(spice::repository::StatePaths, journal::JournalReader), Failure> {
    let state = repository
        .admit_state()
        .map_err(|error| fault(FaultClass::Environment, "room::state-unavailable", error))?;
    let journal = journal::JournalReader::open(state.journal())
        .map_err(|error| fault(FaultClass::Store, "room::journal-unreadable", error))?;
    Ok((state, journal))
}

/// The task handle `matches` names and the birth stamp anchoring its room.
fn named_room(matches: &ArgMatches) -> Result<(TaskHandle, TaskStamp), Failure> {
    let named = matches
        .get_one::<String>("HANDLE")
        .expect("generated room parser requires HANDLE");
    let named = named
        .parse::<TaskHandle>()
        .map_err(|error| task_invalid(named, &error))?;
    let anchor = named
        .born()
        .map_err(|error| task_invalid(named.as_str(), &error))?;
    Ok((named, anchor))
}

/// [`run`] against `repository`.
pub(crate) fn run_at(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
) -> Outcome<RoomHistory> {
    match open(repository) {
        Ok((state, journal)) => {
            let artifacts = artifact::Store::new(state.artifacts(), state.artifacts_lock());
            let owners = SourceOwners {
                tape: state.tape(),
                artifacts: &artifacts,
                git: repository.common_dir(),
            };
            answer(&journal, owners, matches, contract.path)
        }
        Err(failure) => failure.outcome(),
    }
}

/// One page of the room `matches` names, read from `journal` and the
/// authorities `owners` names.
pub(crate) fn answer(
    journal: &journal::JournalReader,
    owners: SourceOwners<'_>,
    matches: &ArgMatches,
    path: &[&str],
) -> Outcome<RoomHistory> {
    match history(journal, owners, matches, path) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

fn history(
    journal: &journal::JournalReader,
    owners: SourceOwners<'_>,
    matches: &ArgMatches,
    path: &[&str],
) -> Result<(ReplyPosition, RoomHistory), Failure> {
    let page = Page::from_matches(matches, path)?;
    let selection = if matches.get_flag("contributions") {
        Selection::Contributions
    } else {
        Selection::Publications
    };
    let (named, anchor) = named_room(matches)?;
    let after = page
        .after()
        .map(|after| {
            after
                .parse::<RoomAuthorityStamp>()
                .map_err(|_| page.cursor_refusal(CURSOR))
        })
        .transpose()?;
    let recorded = journal
        .read_with(|read| recorded(read, &anchor, after.as_ref(), &page, selection))
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => store(error),
            journal::ReadError::Answer(failure) => failure,
        })?;
    if let Some(notice) = crate::board_command::rename_notice(&named, &recorded.room) {
        eprintln!("{notice}");
    }
    let mut sources = RoomSources::new(Some(owners), &anchor);
    let rows = recorded
        .rows
        .into_iter()
        .map(|row| {
            let key = row.publication.to_string();
            (row.project(owners.tape, &mut sources), key)
        })
        .collect();
    let body = match page.admit(rows, recorded.population)? {
        Paged::Shown(page) => RoomHistory::Shown {
            room: recorded.room,
            contributions: sections::group(&page.rows),
            decisions: recorded.decisions,
            anchor,
            page,
        },
        Paged::Exported(export) => RoomHistory::Exported {
            room: recorded.room,
            anchor,
            export,
        },
    };
    Ok((ReplyPosition::Snapshot(recorded.cursor), body))
}

/// Which of a room's publications a read pages.
#[derive(Clone, Copy)]
enum Selection {
    /// Every publication: the complete chronological history.
    Publications,
    /// Only typed contributions.
    Contributions,
}

/// What one read snapshot of the journal holds of a room.
struct Recorded {
    cursor: Seq,
    /// The anchor task's current handle.
    room: TaskHandle,
    /// Publications at or after the caller's cursor.
    population: usize,
    /// The first of them, at most one page and the record it withholds.
    rows: Vec<Row>,
    /// The anchor's decisions in decision order.
    decisions: sections::Decisions,
}

/// One publication as the journal records it, before its source is read.
struct Row {
    publication: RoomAuthorityStamp,
    seq: Seq,
    at: Instant,
    actor: Named,
    body: Record,
    directed: Vec<Route>,
    closed: bool,
}

/// What a publication's fact records.
enum Record {
    /// A conversation or leave body.
    Body(Body),
    /// A typed contribution, owning its text and citing its sources.
    Contributed {
        kind: RoomContributionKind,
        question: Option<QuestionView>,
        text: String,
        sources: Vec<RoomSource>,
    },
}

enum Body {
    Published(RoomPublicationContent),
    Messaged {
        hunk: TranscriptHunk,
        author: CanonicalUuid,
    },
    Left {
        clause: String,
    },
}

fn recorded(
    read: &journal::JournalRead<'_>,
    anchor: &TaskStamp,
    after: Option<&RoomAuthorityStamp>,
    page: &Page,
    selection: Selection,
) -> Result<Recorded, Failure> {
    let room = current_handle(read, anchor)?;
    let cursor = read.cursor().map_err(store)?.ok_or_else(|| {
        fault(
            FaultClass::Store,
            "room::cursor-missing",
            "the journal names a task but no read position",
        )
    })?;
    let start = match after {
        None => None,
        Some(publication) => {
            let seq = read
                .room_publication_seq(anchor, publication)
                .map_err(store)?
                .ok_or_else(|| Failure::Refusal(page.cursor_refusal(CURSOR)))?;
            Seq::new(seq.get() - 1)
        }
    };
    let population = match selection {
        Selection::Publications => read.room_publication_count(anchor, start),
        Selection::Contributions => read.room_contribution_count(anchor, start),
    }
    .map_err(store)?;
    let retain = page.retain().min(population);
    let budget = journal::FactReadBudget::new(
        retain.clamp(1, journal::FactReadBudget::RETAINED.rows()),
        journal::FactReadBudget::RETAINED.decoded_bytes(),
    )
    .map_err(store)?;
    let roles = read.principal_roles().map_err(store)?;
    let principals = PrincipalDirectory::codes(&roles);
    let mut rows = Vec::with_capacity(retain);
    let mut next = start;
    while rows.len() < retain {
        let fetched = match selection {
            Selection::Publications => read.room_publications(anchor, next, budget),
            Selection::Contributions => read.room_contributions(anchor, next, budget),
        }
        .map_err(store)?;
        for envelope in fetched.rows.into_iter().take(retain - rows.len()) {
            rows.push(row(read, &principals, envelope)?);
        }
        match fetched.continuation {
            Some(seq) => next = Some(seq),
            None => break,
        }
    }
    let decisions = sections::decisions(read, &principals, anchor, &room)?;
    Ok(Recorded {
        cursor,
        room,
        population,
        rows,
        decisions,
    })
}

/// The current handle of the room anchored at `anchor`, refusing an anchor
/// that names no task.
fn current_handle(
    read: &journal::JournalRead<'_>,
    anchor: &TaskStamp,
) -> Result<TaskHandle, Failure> {
    let heads = read.task_heads_for(anchor).map_err(store)?.ok_or_else(|| {
        Failure::Refusal(crate::read_page::refusal(
            "room",
            "task-unknown",
            Some(anchor.to_string()),
            "read the board and pass an existing task handle",
        ))
    })?;
    Ok(board::Entry::new(
        heads.statement().body().clone(),
        heads.workflow().body().clone(),
        heads.completion().body().clone(),
        heads.claim_bodies(),
    )
    .map_err(|error| fault(FaultClass::Store, "room::task-head-invalid", error))?
    .handle()
    .clone())
}

/// One publication envelope as a history row, beside the principal name
/// recorded for its actor.
fn row(
    read: &journal::JournalRead<'_>,
    principals: &PrincipalDirectory,
    envelope: wire::Envelope<wire::AnyFact>,
) -> Result<Row, Failure> {
    let (publication, body, directives, conversed) =
        if let Some(published) = envelope.fact.read::<RoomPublished>() {
            (
                published.publication,
                Record::Body(Body::Published(published.content)),
                Vec::new(),
                None,
            )
        } else if let Some(messaged) = envelope.fact.read::<RoomMessaged>() {
            let conversed = messaged_conversation(read, envelope.seq, &messaged.hunk)?;
            (
                messaged.publication,
                Record::Body(Body::Messaged {
                    hunk: messaged.hunk,
                    author: messaged.author,
                }),
                messaged.directed,
                conversed,
            )
        } else if let Some(posted) = envelope.fact.read::<RoomPosted>() {
            let conversed = read
                .room_conversation(&posted.publication)
                .map_err(store)?
                .map(|(_, publication)| publication);
            (
                posted.publication,
                Record::Body(Body::Published(RoomPublicationContent::Authored {
                    text: posted.text,
                })),
                posted.directed,
                conversed,
            )
        } else if let Some(left) = envelope.fact.read::<RoomLeft>() {
            (
                left.publication,
                Record::Body(Body::Left {
                    clause: left.clause,
                }),
                Vec::new(),
                None,
            )
        } else if let Some(contributed) = envelope.fact.read::<RoomContributed>() {
            let question = match &contributed.item {
                Some(item) => {
                    let (handle, text) = question_text(read, &contributed, item).map_err(store)?;
                    Some(QuestionView {
                        item: item.clone(),
                        handle,
                        text,
                    })
                }
                None => None,
            };
            (
                contributed.publication,
                Record::Contributed {
                    kind: contributed.contribution,
                    question,
                    text: contributed.text,
                    sources: contributed.sources,
                },
                Vec::new(),
                None,
            )
        } else {
            return Err(fault(
                FaultClass::Store,
                "room::publication-invalid",
                format!(
                    "room publication {} is a {} fact",
                    envelope.seq,
                    envelope.fact.kind()
                ),
            ));
        };
    let principal_inputs = conversed.iter().flat_map(|publication| {
        publication
            .principal_routes()
            .map(|(principal, _)| {
                PrincipalInputRef::published(publication.publication(), principal).directive
            })
            .collect::<Vec<_>>()
    });
    let directed: Vec<Route> = directives
        .into_iter()
        .chain(principal_inputs)
        .map(|directive| {
            Ok(Route {
                directive,
                receipt: receipt(read, principals, directive)?,
            })
        })
        .collect::<Result<_, Failure>>()?;
    let closed = matches!(body, Record::Body(Body::Messaged { .. })) && directed.is_empty();
    Ok(Row {
        publication,
        seq: envelope.seq,
        at: envelope.at,
        actor: Named::of(principals, envelope.actor),
        body,
        directed,
        closed,
    })
}

/// The conversation record a lane packet's room record shares its write
/// with: the first one after it, proved by owning the same transcript hunk.
/// A packet that conversed with no operator or question has none.
fn messaged_conversation(
    read: &journal::JournalRead<'_>,
    seq: Seq,
    hunk: &TranscriptHunk,
) -> Result<Option<wire::rooms::RoomPublication>, Failure> {
    Ok(read
        .room_conversation_after(seq)
        .map_err(store)?
        .map(|(_, publication)| publication)
        .filter(|publication| {
            matches!(publication.source(), RoomSource::Transcript(owned) if owned == hunk)
        }))
}

/// A directive's current steering standing, with the body that answered it
/// when this is the first input that body covered.
fn receipt(
    read: &journal::JournalRead<'_>,
    principals: &PrincipalDirectory,
    directive: CanonicalUuid,
) -> Result<Receipt, Failure> {
    let Some(head) = read.steering_head(directive).map_err(store)? else {
        return Ok(Receipt::Missing);
    };
    let head = head.into_body();
    let line = crate::peer_line::standing(read, &head).map_err(store)?;
    let answer = match line.as_ref().and_then(|line| line.answer.as_ref()) {
        Some(wire::intent::LineAnswer::Answered { key, body }) => {
            answering_body(read, &head.key, key, *body)?.map(Box::new)
        }
        Some(wire::intent::LineAnswer::Seen {}) | None => None,
    };
    Ok(Receipt::Steering {
        line: line.map(Box::new),
        recipient: Named::of(principals, head.recipient),
        key: head.key,
        disposition: head.disposition,
        answer,
    })
}

/// The body `key` that answered input `input`, absent unless `input` is the
/// first input that body covered, so each body shows once.
fn answering_body(
    read: &journal::JournalRead<'_>,
    input: &str,
    key: &str,
    body: wire::body::PeerBodyKind,
) -> Result<Option<Answer>, Failure> {
    let head = read.steering_key(key).map_err(store)?.ok_or_else(|| {
        fault(
            FaultClass::Store,
            "room::answer-missing",
            format!("input {input} was answered by {key}, which has no steering head"),
        )
    })?;
    let head = head.into_body();
    let first = crate::peer_line::standing(read, &head)
        .map_err(store)?
        .and_then(|line| line.covered.into_iter().next());
    Ok((first.as_deref() == Some(input)).then_some(Answer {
        key: head.key,
        body,
        text: head.text,
    }))
}

impl Row {
    /// This publication with its bytes read from their owning source: a body
    /// from the tapes under `tape`, a contribution's citations through
    /// `sources`.
    fn project(self, tape: &Path, sources: &mut RoomSources<'_>) -> Publication {
        let (content, author) = match self.body {
            Record::Body(body) => project_body(body, tape),
            Record::Contributed {
                kind,
                question,
                text,
                sources: cited,
            } => (
                Content::Contribution {
                    kind,
                    question,
                    text,
                    sources: cited
                        .into_iter()
                        .map(|source| sources.view(source, self.seq))
                        .collect(),
                },
                None,
            ),
        };
        Publication {
            publication: self.publication,
            seq: self.seq,
            at: self.at,
            actor: self.actor,
            author,
            content,
            directed: self.directed,
            closed: self.closed,
        }
    }
}

/// A body's bytes read from their owning source, with the occupant whose
/// transcript authored an agent packet.
fn project_body(body: Body, tape: &Path) -> (Content, Option<CanonicalUuid>) {
    match body {
        Body::Published(RoomPublicationContent::Authored { text }) => {
            (Content::Text { text }, None)
        }
        Body::Published(RoomPublicationContent::Linked {
            link: RoomSource::Transcript(hunk),
        }) => (transcript(tape, hunk), None),
        Body::Published(RoomPublicationContent::Linked { link }) => {
            (Content::Linked { source: link }, None)
        }
        Body::Messaged { hunk, author } => (transcript(tape, hunk), Some(author)),
        Body::Left { clause } => (Content::Left { clause }, None),
    }
}

/// The body of one conversation publication, before its bytes are read: an
/// operator post's text from its journal fact, a lane's from its transcript.
fn conversation_body(
    read: &journal::JournalRead<'_>,
    publication: &wire::rooms::RoomPublication,
) -> Result<Body, Failure> {
    let source = publication.source();
    if let RoomSource::Journal { fact } = source
        && let Some(seq) = Seq::new(fact.get())
        && let Some(posted) = read
            .room_fact(seq)
            .map_err(store)?
            .and_then(|envelope| envelope.fact.read::<RoomPosted>())
    {
        return Ok(Body::Published(RoomPublicationContent::Authored {
            text: posted.text,
        }));
    }
    Ok(Body::Published(RoomPublicationContent::Linked {
        link: source.clone(),
    }))
}

/// The direction `publication`'s principal route cites by `#KEY`, as its
/// steering head reads.
fn concerned_direction(
    read: &journal::JournalRead<'_>,
    publication: &wire::rooms::RoomPublication,
) -> Result<Option<wire::intent::RoomConcernedDirection>, Failure> {
    let Some(key) = publication
        .routes()
        .iter()
        .find_map(|route| route.direction.as_deref())
    else {
        return Ok(None);
    };
    let head = read.steering_key(key).map_err(store)?.ok_or_else(|| {
        fault(
            FaultClass::Store,
            "room::direction-missing",
            format!(
                "publication {} concerns direction {key}, which has no steering head",
                publication.publication()
            ),
        )
    })?;
    let head = head.into_body();
    Ok(Some(wire::intent::RoomConcernedDirection {
        key: key.to_owned(),
        text: head.text,
        attachments: head.attachments,
    }))
}

/// The hunk's bytes from its lane's durable tape, or why the tape cannot
/// supply them.
fn transcript(tape: &Path, hunk: TranscriptHunk) -> Content {
    match spice::quote::room_hunk_text(tape, &hunk) {
        Ok(text) => Content::Text { text },
        Err(unavailable) => Content::Unavailable {
            source: RoomSource::Transcript(hunk),
            unavailable,
        },
    }
}

/// One conversation publication's text, read from its owning source exactly
/// as room history reads it, with the principal its route reaches; an unknown
/// publication is a missing source.
pub(crate) fn conversation_text(
    bus: &bus::Bus,
    tape: &Path,
    request: &wire::intent::RoomConversationText,
) -> Outcome<wire::intent::CommandAnswer> {
    use wire::intent::RoomConversationContent as Text;

    let publication = request.publication.clone();
    let read = bus.read(move |read| {
        let (body, direction, recipient) = match read.room_conversation(&publication)? {
            Some((_, admitted)) => (
                Some(conversation_body(read, &admitted).map_err(Failure::guard)?),
                concerned_direction(read, &admitted).map_err(Failure::guard)?,
                read.room_operator_requester(&admitted)?,
            ),
            None => (None, None, None),
        };
        Ok((read.cursor()?, body, direction, recipient))
    });
    let (cursor, body, direction, recipient) = match read {
        Ok((Some(cursor), body, direction, recipient)) => (cursor, body, direction, recipient),
        Ok((None, ..)) => {
            return fault(
                FaultClass::Store,
                "room::cursor-missing",
                "the admitted journal names no read position",
            )
            .outcome();
        }
        Err(error) => {
            return fault(FaultClass::Store, "room::journal-unreadable", error).outcome();
        }
    };
    let content = match body.map(|body| project_body(body, tape).0) {
        None => Text::Unavailable {
            unavailable: RoomSourceUnavailable::Missing {},
        },
        Some(Content::Text { text }) => Text::Text { text },
        Some(Content::Linked { source }) => Text::Linked { source },
        Some(Content::Unavailable { unavailable, .. }) => Text::Unavailable { unavailable },
        // A conversation body is authored or linked, never a leave or contribution.
        Some(Content::Left { .. } | Content::Contribution { .. }) => Text::Unavailable {
            unavailable: RoomSourceUnavailable::Unreadable {},
        },
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: wire::intent::CommandAnswer::RoomConversationText(Box::new(
            wire::intent::RoomConversationTextAnswer {
                publication: request.publication.clone(),
                content,
                direction,
                recipient,
            },
        )),
    })
}

/// One contribution's cited sources, resolved at its own journal position
/// exactly as [`answer`] resolves them, with the text of the question item it
/// names under the same rule; an unknown contribution refuses.
pub(crate) fn contribution_read(
    bus: &bus::Bus,
    owners: SourceOwners<'_>,
    request: &wire::intent::RoomContributionRead,
) -> Outcome<wire::intent::CommandAnswer> {
    let (room, publication) = (request.room.clone(), request.publication.clone());
    let read = bus.read(move |read| {
        let cursor = read.cursor()?;
        let Some(seq) = read.room_publication_seq(&room, &publication)? else {
            return Ok((cursor, None));
        };
        let page = read.fact_page(
            journal::FactPageStart::At(seq),
            seq,
            journal::FactReadBudget::RETAINED,
        )?;
        let Some(contributed) = page
            .rows
            .into_iter()
            .next()
            .and_then(|envelope| envelope.fact.read::<RoomContributed>())
            .filter(|contributed| contributed.publication == publication)
        else {
            return Ok((cursor, None));
        };
        let question = match &contributed.item {
            Some(item) => question_text(read, &contributed, item)?.1,
            None => None,
        };
        let facts = fact_summaries(read, &contributed.sources, seq)?;
        Ok((cursor, Some((seq, contributed, question, facts))))
    });
    let (cursor, contributed) = match read {
        Ok((Some(cursor), contributed)) => (cursor, contributed),
        Ok((None, _)) => {
            return fault(
                FaultClass::Store,
                "room::cursor-missing",
                "the admitted journal names no read position",
            )
            .outcome();
        }
        Err(error) => {
            return fault(FaultClass::Store, "room::journal-unreadable", error).outcome();
        }
    };
    let Some((seq, contributed, question, facts)) = contributed else {
        return Outcome::Refusal(crate::read_page::refusal(
            "room",
            "contribution-missing",
            Some(request.publication.to_string()),
            "read the room and name one of its contributions",
        ));
    };
    let mut sources = RoomSources::new(Some(owners), &request.room);
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: wire::intent::CommandAnswer::RoomContributionRead(Box::new(
            wire::intent::RoomContributionReadAnswer {
                publication: request.publication.clone(),
                sources: contributed
                    .sources
                    .into_iter()
                    .map(|source| sources.view(source, seq))
                    .collect(),
                facts,
                question,
            },
        )),
    })
}

/// What each journal fact `sources` cites is, in citation order, for every
/// fact that yields its bytes to the contribution at `seq`.
fn fact_summaries(
    read: &journal::JournalRead<'_>,
    sources: &[RoomSource],
    seq: Seq,
) -> Result<Vec<wire::intent::RoomFactSummary>, journal::GuardError> {
    let mut summaries = Vec::new();
    for source in sources {
        let RoomSource::Journal { fact } = source else {
            continue;
        };
        let Some(at) = preceding_fact(*fact, seq) else {
            continue;
        };
        let page = read.fact_page(
            journal::FactPageStart::At(at),
            at,
            journal::FactReadBudget::RETAINED,
        )?;
        let Some(envelope) = page.rows.into_iter().next() else {
            continue;
        };
        let task = match envelope
            .fact
            .task_subject()
            .or_else(|| envelope.fact.room_record())
        {
            Some(born) => existing_task(read, born)?,
            None => None,
        };
        summaries.push(wire::intent::RoomFactSummary {
            fact: *fact,
            kind: envelope.fact.kind(),
            task,
        });
    }
    Ok(summaries)
}

/// `born` and its current handle while the task exists and is not deleted.
fn existing_task(
    read: &journal::JournalRead<'_>,
    born: &TaskStamp,
) -> Result<Option<wire::intent::RoomFactTask>, journal::GuardError> {
    let (Some(task), Some(workflow)) = (read.task_head(born)?, read.workflow_head(born)?) else {
        return Ok(None);
    };
    Ok(
        (!workflow.body().status.is_deleted()).then(|| wire::intent::RoomFactTask {
            born: born.clone(),
            handle: task.body().handle(workflow.body().phase),
        }),
    )
}

/// The handle and text of the question item `contributed` names. The handle
/// is unique among the Design's current items; the text is present only
/// while the statement the contribution witnessed is still current, because
/// the journal keeps no superseded statement text.
fn question_text(
    read: &journal::JournalRead<'_>,
    contributed: &RoomContributed,
    item: &StatementItem,
) -> Result<(String, Option<String>), journal::GuardError> {
    let head = read.task_head(&contributed.room)?;
    let term = head.as_ref().map_or(&[][..], |head| &head.body().term[..]);
    let current = StatementItem::each(term).collect::<Vec<_>>();
    let text = head
        .as_ref()
        .filter(|head| head.position() == contributed.statement)
        .and_then(|_| item.text_in(term).map(str::to_owned));
    Ok((item.handle(&current).to_owned(), text))
}

impl fmt::Display for RoomHistory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shown {
                room,
                anchor,
                page,
                contributions,
                decisions,
            } => {
                write!(formatter, "room {room} anchor {anchor}")?;
                for publication in &page.rows {
                    write!(formatter, "\n{publication}")?;
                }
                for group in contributions {
                    write!(formatter, "{group}")?;
                }
                write!(formatter, "{decisions}")?;
                match &page.omission {
                    Some(omission) => write!(formatter, "\n{omission}"),
                    None => Ok(()),
                }
            }
            Self::Exported {
                room,
                anchor,
                export,
            } => write!(formatter, "room {room} anchor {anchor}\n{export}"),
        }
    }
}

impl fmt::Display for Publication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} seq={} at={} from={}",
            self.publication, self.seq, self.at, self.actor
        )?;
        if let Some(author) = self.author {
            write!(formatter, " author={author}")?;
        }
        for route in &self.directed {
            write!(formatter, "\n  {route}")?;
        }
        if self.closed {
            formatter.write_str("\n  recorded; closed, no peer attention")?;
        }
        write_content(formatter, &self.content)
    }
}

impl fmt::Display for Route {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.receipt {
            Receipt::Steering {
                key,
                recipient,
                disposition,
                line,
                answer,
            } => {
                write!(
                    formatter,
                    "receipt to={recipient} key={key} {}",
                    disposition.as_str()
                )?;
                if let Some(line) = line {
                    write!(formatter, "; {}", line.standing())?;
                }
                if let Some(answer) = answer {
                    for line in answer.text.lines() {
                        write!(formatter, "\n    | {line}")?;
                    }
                }
                Ok(())
            }
            Receipt::Missing => write!(formatter, "receipt directive={} missing", self.directive),
        }
    }
}

/// A publication's bytes, one quoted line each, or what stands in for them.
fn write_content(formatter: &mut fmt::Formatter<'_>, content: &Content) -> fmt::Result {
    match content {
        Content::Text { text } => {
            for line in text.lines() {
                write!(formatter, "\n  | {line}")?;
            }
            Ok(())
        }
        Content::Linked { source } => write!(formatter, "\n  linked {}", Source(source)),
        Content::Left { clause } => write!(formatter, "\n  left the room: {clause}"),
        Content::Contribution {
            kind,
            question,
            text,
            sources,
        } => {
            write!(formatter, "\n  {kind}")?;
            match question {
                Some(QuestionView {
                    handle,
                    text: Some(asked),
                    ..
                }) => write!(formatter, " on {handle}: {asked}")?,
                Some(QuestionView {
                    handle, text: None, ..
                }) => {
                    write!(formatter, " on {handle} of an earlier statement")?;
                }
                None => {}
            }
            for line in text.lines() {
                write!(formatter, "\n  | {line}")?;
            }
            for source in sources {
                match source {
                    RoomSourceView::Available { source } => {
                        write!(formatter, "\n  cites {}", Source(source))?;
                    }
                    RoomSourceView::Unavailable {
                        source,
                        unavailable,
                    } => write!(
                        formatter,
                        "\n  cites {} unavailable: {}",
                        Source(source),
                        Unavailable(unavailable)
                    )?,
                }
            }
            Ok(())
        }
        Content::Unavailable {
            source,
            unavailable,
        } => write!(
            formatter,
            "\n  unavailable {}: {}",
            Source(source),
            Unavailable(unavailable)
        ),
    }
}

/// A room source as one line names it.
struct Source<'a>(&'a RoomSource);

impl fmt::Display for Source<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RoomSource::Transcript(hunk) => write!(
                formatter,
                "transcript lane={} vendor={} source={} member={} offset={} event={} bytes={}..{}",
                hunk.head.lane,
                hunk.head.vendor,
                hunk.head.source,
                hunk.locus.member,
                hunk.locus.offset,
                hunk.locus.event,
                hunk.text.start,
                hunk.text.end
            ),
            RoomSource::Journal { fact } => write!(formatter, "journal fact={fact}"),
            RoomSource::Artifact { digest } => write!(formatter, "artifact digest={digest}"),
            RoomSource::Contribution { room, publication } => {
                write!(
                    formatter,
                    "contribution room={room} publication={publication}"
                )
            }
            RoomSource::Git(source) => write!(formatter, "git {source}"),
        }
    }
}

struct Unavailable<'a>(&'a RoomSourceUnavailable);

impl fmt::Display for Unavailable<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RoomSourceUnavailable::Missing {} => formatter.write_str("missing"),
            RoomSourceUnavailable::Hidden {} => formatter.write_str("hidden"),
            RoomSourceUnavailable::Changed {} => formatter.write_str("changed"),
            RoomSourceUnavailable::Unreadable {} => formatter.write_str("unreadable"),
        }
    }
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refusal(refusal)
    }
}

impl Failure {
    fn outcome<T>(self) -> Outcome<T> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }

    fn guard(self) -> journal::GuardError {
        match self {
            Self::Refusal(refusal) => journal::GuardError::Invalid(format!("{refusal:?}")),
            Self::Fault(fault) => {
                journal::GuardError::Invalid(format!("{}: {}", fault.reason, fault.detail))
            }
        }
    }
}

fn task_invalid(named: &str, error: &impl fmt::Display) -> Failure {
    Failure::Refusal(crate::read_page::refusal(
        "room",
        "task-invalid",
        Some(format!("{named}: {error}")),
        "read the board and pass one task handle",
    ))
}

fn store(error: journal::GuardError) -> Failure {
    fault(FaultClass::Store, "room::journal-unreadable", error)
}

fn fault(class: FaultClass, reason: &str, detail: impl fmt::Display) -> Failure {
    Failure::Fault(Fault {
        class,
        reason: reason.to_owned(),
        detail: detail.to_string(),
    })
}
