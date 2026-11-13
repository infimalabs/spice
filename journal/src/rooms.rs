//! Room and preparation authority folded from immutable facts.
//!
//! The current projection: room publication positions, write-once proposal
//! heads, and one authorization record per selected output and decision.
//! One fold derives that projection.
//! A commit runs it against stored authority before any head moves, so an
//! unsound reference refuses the whole write; verification and reindexing run
//! it over the complete history, so a stored row is only ever one the fold
//! derives.
//!
//! Source bytes stay with their owners: a projection row names a fact
//! position, a task, or a canonical link, never copied conversation text.
//! Transcript links name a nonempty span within an accounted generation
//! boundary; a matching stream and source identity alone are not authority.

use std::collections::BTreeMap;

use wire::rooms::{
    ExecutionAuthorized, PreparationDecided, ProposalRef, RoomAuthorityStamp, RoomContributed,
    RoomDelivered, RoomLeft, RoomMessaged, RoomPublicationContent, RoomPublished, RoomSource,
};
use wire::{Actor, AnyFact, CanonicalUuid, Seq, TaskPreparation, TaskStamp, TranscriptSourceHead};

mod awareness;
mod claims;
mod conversation;
mod dependencies;
mod history;
mod hunk;
mod packets;
mod preparation;
mod projection;
mod sql;

pub(crate) use awareness::require_deliveries as require_return_deliveries;
pub(crate) use conversation::{
    SCHEMA_SQL as CONVERSATION_SCHEMA_SQL, require_deliveries as require_conversation_deliveries,
};
pub(crate) use history::History;
use hunk::require_hunk;
pub(crate) use packets::require_packet_routes;

pub(crate) const MATURITY_SCHEMA_SQL: &str = r#"
CREATE TABLE design_notices (
    design TEXT NOT NULL,
    seq INTEGER NOT NULL UNIQUE REFERENCES fact_envelopes(seq),
    PRIMARY KEY (design, seq)
) STRICT, WITHOUT ROWID;
CREATE INDEX design_notices_design_seq ON design_notices(design, seq);
CREATE TABLE design_maturity (
    design TEXT PRIMARY KEY,
    body BLOB NOT NULL
) STRICT;
"#;
pub(crate) use sql::{Stored, require_open_controllers, settle_controls, write_projection};

/// Room-authority projection tables, created with the current schema.
pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE room_publications (
    room TEXT NOT NULL,
    publication TEXT NOT NULL,
    seq INTEGER NOT NULL UNIQUE REFERENCES fact_envelopes(seq),
    PRIMARY KEY (room, publication)
) STRICT, WITHOUT ROWID;
CREATE INDEX room_publications_room_seq ON room_publications(room, seq);
CREATE TABLE proposal_heads (
    subject TEXT PRIMARY KEY,
    controller TEXT NOT NULL,
    head_seq INTEGER NOT NULL UNIQUE REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE INDEX proposal_heads_controller_seq ON proposal_heads(controller, head_seq);
CREATE TABLE authorization_heads (
    output TEXT NOT NULL,
    decision TEXT NOT NULL,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL,
    PRIMARY KEY (output, decision)
) STRICT, WITHOUT ROWID;
CREATE INDEX authorization_heads_decision ON authorization_heads(decision);
"#;

/// Singleton repository OOPS-generation head.
pub(crate) const OOPS_GENERATION_SCHEMA_SQL: &str = r#"
CREATE TABLE oops_generation_head (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
"#;

/// Per-lane room catch-up cursors, created with the current schema.
pub(crate) const CURSOR_SCHEMA_SQL: &str = r#"
CREATE TABLE room_cursors (
    lane TEXT PRIMARY KEY,
    head_seq INTEGER NOT NULL UNIQUE REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
"#;

/// Why the fold could not admit one fact.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The fact names absent, stale, or contradictory authority.
    Unsound(String),
    /// Stored authority could not be read or written.
    Store(rusqlite::Error),
    /// A stored or proposed value did not cross the canonical codec.
    Codec(serde_json::Error),
}

impl From<rusqlite::Error> for Refusal {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error)
    }
}

impl From<serde_json::Error> for Refusal {
    fn from(error: serde_json::Error) -> Self {
        Self::Codec(error)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsound(detail) => formatter.write_str(detail),
            Self::Store(error) => write!(formatter, "room authority storage failed: {error}"),
            Self::Codec(error) => write!(formatter, "room authority codec failed: {error}"),
        }
    }
}

fn unsound<T>(detail: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::Unsound(detail.into()))
}

/// One stored proposal head: its controller, position, and canonical body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProposalRow {
    pub(crate) controller: String,
    pub(crate) seq: u64,
    pub(crate) body: Vec<u8>,
}

/// One stored authorization record for one selected output and decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthorizationRow {
    pub(crate) decision: String,
    pub(crate) seq: u64,
    pub(crate) body: Vec<u8>,
}

/// The complete room-authority projection, row for row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    /// `(room, publication)` to publishing fact position.
    pub(crate) publications: BTreeMap<(String, String), u64>,
    /// `(Design, notice position)` rows for bounded per-Design reads.
    pub(crate) notices: BTreeMap<(String, u64), ()>,
    /// Design to its canonical bounded maturity accumulator.
    pub(crate) maturity: BTreeMap<String, Vec<u8>>,
    /// Repository OOPS generation's head position and canonical body.
    pub(crate) oops_generation: Option<(u64, Vec<u8>)>,
    /// Canonical proposal subject to its write-once head.
    pub(crate) proposals: BTreeMap<String, ProposalRow>,
    /// `(selected output, decision)` to its authorization record.
    pub(crate) authorizations: BTreeMap<(String, String), AuthorizationRow>,
    /// Logical lane to its catch-up cursor head position and canonical body.
    pub(crate) cursors: BTreeMap<String, (u64, Vec<u8>)>,
    /// Conversation publications.
    pub(crate) conversation: conversation::Heads,
}

/// Authority one fold step reads and changes.
///
/// Stored authority answers from SQLite inside the commit transaction; history
/// answers from what the fold has already seen. Task preparation is part of
/// this view because output control must name tasks the fold can prove exist
/// and are not already controlled.
pub(crate) trait Authority {
    /// Whether the task exists, and its current preparation when it does.
    fn task(&self, task: &TaskStamp) -> Result<Option<Option<TaskPreparation>>, Refusal>;
    /// Record a task birth or an admitted output control.
    fn set_task(&mut self, task: &TaskStamp, preparation: Option<TaskPreparation>);
    /// Declared intent of an existing task.
    fn task_intent(&self, task: &TaskStamp) -> Result<Option<Option<wire::TaskIntent>>, Refusal>;
    /// Record an admitted task's declared intent.
    fn set_task_intent(&mut self, task: &TaskStamp, intent: Option<wire::TaskIntent>);
    /// Current ordinary dependency edges of an existing task.
    fn task_dependencies(&self, task: &TaskStamp) -> Result<Option<Vec<TaskStamp>>, Refusal>;
    /// Replace one task's replay-visible ordinary dependencies.
    fn set_task_dependencies(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>);
    /// Record the exact ordinary dependencies authored at birth.
    fn set_task_birth_dependencies(&mut self, added: &wire::body::TaskAdded) {
        self.set_task_dependencies(&added.task, added.depends.clone());
    }
    /// Record the exact ordinary dependencies authored by an edit.
    fn set_task_edit_dependencies(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>) {
        self.set_task_dependencies(task, dependencies);
    }
    /// Current statement head position of one task, when it exists.
    fn task_statement(&self, task: &TaskStamp) -> Result<Option<Seq>, Refusal>;
    /// Record one task statement mutation.
    fn set_task_statement(&mut self, task: &TaskStamp, statement: Seq);
    /// Every active claim on one task with its current claim-head position.
    fn active_claims(&self, task: &TaskStamp) -> Result<Vec<claims::ActiveClaim>, Refusal>;
    /// Replace the replay-visible active claims of one task.
    fn set_active_claims(&mut self, task: &TaskStamp, claims: Vec<claims::ActiveClaim>);
    /// Record the current accounted head of one logical transcript stream.
    fn account(&mut self, head: &TranscriptSourceHead);
    /// Current accounted head of the linked logical transcript stream.
    fn transcript(
        &self,
        head: &TranscriptSourceHead,
    ) -> Result<Option<TranscriptSourceHead>, Refusal>;
    /// Whether this room publication already exists.
    fn published(&self, room: &str, publication: &str) -> Result<bool, Refusal>;
    /// Whether this position is an indexed room publication.
    fn publication_at(&self, seq: u64) -> Result<bool, Refusal>;
    /// Whether this position is an indexed Design request.
    fn request_at(&self, seq: u64) -> Result<bool, Refusal>;
    /// The canonical body of one lane's current catch-up cursor.
    fn cursor(&self, lane: &str) -> Result<Option<Vec<u8>>, Refusal>;
    /// Canonical body of the current repository OOPS generation.
    fn oops_generation(&self) -> Result<Option<Vec<u8>>, Refusal>;
    /// The canonical body of one proposal head.
    fn proposal(&self, subject: &str) -> Result<Option<Vec<u8>>, Refusal>;
    /// Whether this decision already authorized a selection.
    fn decision_authorized(&self, decision: &str) -> Result<bool, Refusal>;
    /// The lane the operator addressed with direction `key`, if the operator
    /// sent one under that key.
    fn operator_direction(&self, key: &str) -> Result<Option<CanonicalUuid>, Refusal>;
    /// Record the lane one operator direction addresses.
    fn set_operator_direction(&mut self, key: &str, lane: CanonicalUuid);
    /// The canonical body of one admitted conversation publication.
    fn conversation(&self, publication: &str) -> Result<Option<Vec<u8>>, Refusal>;
    /// Apply one projection change.
    fn change(&mut self, change: Change) -> Result<(), Refusal>;
}

/// One projection consequence of an admitted fact.
pub(crate) enum Change {
    Publication {
        room: String,
        publication: String,
        seq: u64,
    },
    OopsGeneration {
        seq: u64,
        body: Vec<u8>,
    },
    Proposal {
        subject: String,
        row: ProposalRow,
    },
    Authorization {
        outputs: Vec<String>,
        row: AuthorizationRow,
    },
    Cursor {
        lane: String,
        seq: u64,
        body: Vec<u8>,
    },
    Conversation(conversation::Edit),
    MaturityNotice {
        seq: u64,
        notice: Box<wire::rooms::DesignNoticed>,
    },
    MaturityContribution(wire::rooms::RoomContributed),
    MaturityLeave(RoomLeft),
}

/// Canonical key of one proposal subject.
pub(crate) fn subject(proposal: &ProposalRef) -> Result<String, Refusal> {
    String::from_utf8(wire::encode(proposal)?)
        .map_err(|_| Refusal::Unsound("proposal subject is not text".to_owned()))
}

/// Admit one fact `actor` wrote at `seq` against `authority` and apply its
/// projection consequence.
pub(crate) fn fold(
    authority: &mut impl Authority,
    seq: Seq,
    actor: &Actor,
    fact: &AnyFact,
) -> Result<(), Refusal> {
    if let Some(body) = fact.read::<wire::body::OopsGenerationAdvanced>() {
        return advance_oops_generation(authority, seq, &body);
    }
    dependencies::track(authority, fact);
    for task in fact.revised_tasks() {
        authority.set_task_statement(task, seq);
    }
    if let Some(task) = fact.task_subject() {
        match fact.kind() {
            // A Design pass names the claim position that winning, carrying,
            // rebinding, or generation acknowledgement established; a lease
            // renewal only extends that lease.
            wire::FactKind::ClaimWon
            | wire::FactKind::ClaimCarried
            | wire::FactKind::ClaimRebound
            | wire::FactKind::ClaimAcknowledged
            | wire::FactKind::ClaimFreed
            | wire::FactKind::ClaimLapsed => {
                let mut active = authority.active_claims(task)?;
                claims::advance(&mut active, seq, actor, fact);
                authority.set_active_claims(task, active);
                return Ok(());
            }
            wire::FactKind::ClaimRenewed => return Ok(()),
            _ => {}
        }
    }
    if let Some(body) = fact.read::<wire::rooms::OutputControlled>() {
        return control(authority, &body.output, body.preparation);
    }
    if let Some((task, preparation)) = fact.task_preparation() {
        authority.set_task(task, preparation.cloned());
        if let Some((_, intent)) = fact.declared_intent() {
            authority.set_task_intent(task, intent);
        }
        return Ok(());
    }
    if let Some((task, intent)) = fact.declared_intent() {
        authority.set_task_intent(task, intent);
        return Ok(());
    }
    if let Some(body) = fact.read::<wire::body::JournalIngested>() {
        authority.account(&TranscriptSourceHead {
            lane: body.lane,
            vendor: body.vendor,
            stream: body.stream,
            source: body.source,
            next: body.to,
        });
        return Ok(());
    }
    if let Some(body) = fact.read::<RoomPublished>() {
        if let RoomPublicationContent::Linked { link } = &body.content {
            require_source(authority, seq, link)?;
        }
        return publish(authority, seq, &body.room, &body.publication);
    }
    if let Some(body) = fact.read::<RoomMessaged>() {
        if *actor != Actor::Lane(body.hunk.head.lane) {
            return unsound(format!(
                "room packet in {} is written by {actor}, not its hunk's lane {}",
                body.room, body.hunk.head.lane
            ));
        }
        require_hunk(authority, &body.hunk)?;
        return publish(authority, seq, &body.room, &body.publication);
    }
    if let Some(result) = fold_traffic(authority, seq, actor, fact) {
        return result;
    }
    fold_preparation(authority, seq, actor, fact).unwrap_or(Ok(()))
}

fn advance_oops_generation(
    authority: &mut impl Authority,
    seq: Seq,
    advanced: &wire::body::OopsGenerationAdvanced,
) -> Result<(), Refusal> {
    let current = authority
        .oops_generation()?
        .map(|body| wire::decode::<wire::OopsGenerationHead>(&body))
        .transpose()?
        .unwrap_or_default();
    if current.revision.checked_add(1) != Some(advanced.revision)
        || advanced.from != current.design
        || advanced.from.is_some() == advanced.to.is_some()
    {
        return unsound(format!(
            "OOPS generation transition at {seq} does not advance its exact preceding head"
        ));
    }
    if let Some(design) = &advanced.to {
        match authority.task_intent(design)? {
            Some(Some(wire::TaskIntent::OpportunisticDesign)) => {}
            Some(_) => {
                return unsound(format!(
                    "OOPS generation at {seq} names task {design} without opportunistic Design intent"
                ));
            }
            None => {
                return unsound(format!(
                    "OOPS generation at {seq} names absent task {design}"
                ));
            }
        }
    }
    let head = wire::OopsGenerationHead {
        revision: advanced.revision,
        design: advanced.to.clone(),
    };
    authority.change(Change::OopsGeneration {
        seq: seq.get(),
        body: wire::encode(&head)?,
    })
}

/// Admit one decision or authorization fact, or `None` when `fact` is
/// neither.
fn fold_preparation(
    authority: &mut impl Authority,
    seq: Seq,
    actor: &Actor,
    fact: &AnyFact,
) -> Option<Result<(), Refusal>> {
    if let Some(PreparationDecided { decision }) = fact.read() {
        return Some(
            require_operator(actor, "a preparation decision")
                .and_then(|()| preparation::decide(authority, seq, decision)),
        );
    }
    if let Some(ExecutionAuthorized { authorization }) = fact.read() {
        return Some(
            require_operator(actor, "execution authorization")
                .and_then(|()| preparation::authorize(authority, seq, authorization)),
        );
    }
    None
}

/// Admit one steering, catch-up, conversation, or leave fact, or `None`
/// when `fact` is none of them.
fn fold_traffic(
    authority: &mut impl Authority,
    seq: Seq,
    actor: &Actor,
    fact: &AnyFact,
) -> Option<Result<(), Refusal>> {
    if let Some(result) = awareness::fold(authority, seq, actor, fact) {
        return Some(result);
    }
    if let Some(sent) = fact.read::<wire::body::SteerSent>() {
        if is_principal(actor)
            && sent.fanout.is_some()
            && let Actor::Lane(lane) = sent.recipient
        {
            authority.set_operator_direction(&sent.key, lane);
        }
        return Some(Ok(()));
    }
    if let Some(result) = conversation::fold(authority, seq, actor, fact) {
        return Some(result);
    }
    if let Some(body) = fact.read::<RoomDelivered>() {
        return Some(deliver(authority, seq, &body));
    }
    if let Some(body) = fact.read::<RoomLeft>() {
        if *actor != Actor::Lane(body.lane) {
            return Some(unsound(format!(
                "room leave in {} is written by {actor}, not its lane {}",
                body.room, body.lane
            )));
        }
        return Some(
            publish(authority, seq, &body.room, &body.publication)
                .and_then(|()| authority.change(Change::MaturityLeave(body))),
        );
    }
    fact.read::<RoomContributed>()
        .map(|body| contribute(authority, seq, actor, &body))
}

/// Whether `actor` acts as a born principal. The fold replays immutable
/// history, so authorship asks only the actor's principal resolution, never a
/// role a later fact could change; the command door decides current roles.
fn is_principal(actor: &Actor) -> bool {
    matches!(actor, Actor::Principal(_))
}

/// [`is_principal`] for a stored actor spelling.
fn acts_as_principal(spelling: &str) -> bool {
    spelling
        .parse::<Actor>()
        .is_ok_and(|actor| is_principal(&actor))
}

fn require_operator(actor: &Actor, what: &str) -> Result<(), Refusal> {
    if is_principal(actor) {
        return Ok(());
    }
    unsound(format!("{what} is operator-owned, not written by {actor}"))
}

/// A delivery lands in its own lane's accounted transcript and strictly
/// advances that lane's cursor to an earlier indexed room publication. A
/// retained delivery may instead name a Design request, which room catch-up
/// no longer carries.
fn deliver(
    authority: &mut impl Authority,
    seq: Seq,
    delivered: &RoomDelivered,
) -> Result<(), Refusal> {
    require_hunk(authority, &delivered.hunk)?;
    let through = delivered.through;
    let indexed =
        authority.publication_at(through.get())? || authority.request_at(through.get())?;
    if through.get() >= seq.get() || !indexed {
        return unsound(format!(
            "delivery cursor {through} names no room publication or Design request earlier than {seq}"
        ));
    }
    let lane = delivered.hunk.head.lane.to_string();
    if let Some(previous) = authority.cursor(&lane)? {
        let previous = wire::decode::<RoomDelivered>(&previous)?.through;
        if through <= previous {
            return unsound(format!(
                "lane {lane} delivery cursor {through} does not advance past {previous}"
            ));
        }
    }
    authority.change(Change::Cursor {
        lane,
        seq: seq.get(),
        body: wire::encode(delivered)?,
    })
}

/// A contribution is its own lane's bounded text, and each source it cites is
/// authority that exists before it. A question item is named only at a
/// recorded statement head. Its Design's openness is checked against stored
/// heads at commit, by [`require_open_controllers`].
fn contribute(
    authority: &mut impl Authority,
    seq: Seq,
    actor: &Actor,
    body: &RoomContributed,
) -> Result<(), Refusal> {
    if *actor != Actor::Lane(body.lane) {
        return unsound(format!(
            "contribution in {} is written by {actor}, not its lane {}",
            body.room, body.lane
        ));
    }
    if let Err(error) = RoomContributed::admit(
        body.contribution,
        body.item.as_ref(),
        &body.text,
        &body.sources,
    ) {
        return unsound(format!("contribution in {}: {error}", body.room));
    }
    for source in &body.sources {
        require_source(authority, seq, source)?;
    }
    publish(authority, seq, &body.room, &body.publication)?;
    let current = authority.task_statement(&body.room)?.ok_or_else(|| {
        Refusal::Unsound(format!(
            "contribution Design {} has no statement",
            body.room
        ))
    })?;
    if current != body.statement {
        return unsound(format!(
            "contribution in {} names statement {} while its current head is {current}",
            body.room, body.statement
        ));
    }
    authority.change(Change::MaturityContribution(body.clone()))
}

fn control(
    authority: &mut impl Authority,
    output: &TaskStamp,
    preparation: TaskPreparation,
) -> Result<(), Refusal> {
    match authority.task(output)? {
        None => return unsound(format!("controlled output {output} does not exist")),
        Some(Some(current)) => {
            return unsound(format!(
                "output {output} is already controlled by {}",
                current.controller
            ));
        }
        Some(None) => {}
    }
    if preparation.parent == *output || preparation.controller == *output {
        return unsound(format!("output {output} cannot prepare or control itself"));
    }
    for task in [
        &preparation.parent,
        &preparation.controller,
        &preparation.construction,
    ] {
        require_task(authority, task, "preparation")?;
    }
    authority.set_task(output, Some(preparation));
    Ok(())
}

fn publish(
    authority: &mut impl Authority,
    seq: Seq,
    room: &TaskStamp,
    publication: &RoomAuthorityStamp,
) -> Result<(), Refusal> {
    require_task(authority, room, "room anchor")?;
    let room = room.to_string();
    let publication = publication.to_string();
    if authority.published(&room, &publication)? {
        return unsound(format!(
            "room {room} publication {publication} already exists"
        ));
    }
    authority.change(Change::Publication {
        room,
        publication,
        seq: seq.get(),
    })
}

fn record<T: serde::Serialize>(
    authority: &mut impl Authority,
    seq: Seq,
    proposal: &ProposalRef,
    controller: String,
    body: &T,
) -> Result<(), Refusal> {
    let key = subject(proposal)?;
    if authority.proposal(&key)?.is_some() {
        return unsound(format!("proposal subject {key} is already recorded"));
    }
    authority.change(Change::Proposal {
        subject: key,
        row: ProposalRow {
            controller,
            seq: seq.get(),
            body: wire::encode(body)?,
        },
    })
}

pub(super) fn require_task(
    authority: &impl Authority,
    task: &TaskStamp,
    role: &str,
) -> Result<(), Refusal> {
    match authority.task(task)? {
        Some(_) => Ok(()),
        None => unsound(format!("{role} task {task} does not exist")),
    }
}

fn require_proposal(
    authority: &impl Authority,
    proposal: &ProposalRef,
) -> Result<Vec<u8>, Refusal> {
    let key = subject(proposal)?;
    match authority.proposal(&key)? {
        Some(body) => Ok(body),
        None => unsound(format!("proposal subject {key} does not exist")),
    }
}

/// A link must name authority that exists before the linking fact. Artifact
/// and repository bytes live outside the journal, so the link is the whole
/// authority.
pub(super) fn require_source(
    authority: &impl Authority,
    seq: Seq,
    source: &RoomSource,
) -> Result<(), Refusal> {
    match source {
        RoomSource::Journal { fact } => {
            if fact.get() == 0 || fact.get() >= seq.get() {
                return unsound(format!(
                    "source fact {} is not authority earlier than {seq}",
                    fact.get()
                ));
            }
        }
        RoomSource::Transcript(hunk) => require_hunk(authority, hunk)?,
        RoomSource::Artifact { .. } | RoomSource::Git(_) => {}
        RoomSource::Contribution { room, publication } => {
            if !authority.published(room.as_str(), &publication.to_string())? {
                return unsound(format!(
                    "source contribution {publication} is not published in room {room}"
                ));
            }
        }
    }
    Ok(())
}
