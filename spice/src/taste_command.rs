//! Tracked taste reporting and reviewed curation proposals.

use std::collections::{BTreeMap, BTreeSet};

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::config::LoadError;
use spice::repository::Repository;
use wire::body::{TasteDecision, TasteJudgeAttempt, TasteJudged, TasteReminded};
use wire::intent::{CommandAnswer, TaskAdd, TaskAnswer};
use wire::{
    CanonicalUuid, ConfigSource, Digest, Disposition, Domain, FaultClass, Instant, Name, Outcome,
    Refusal, Reply, ReplyPosition, Seq,
};

use crate::cli::Contract;
use crate::command_client::Host;
use crate::command_fault as fault;

pub(crate) mod view;

/// The public topic every taste curation proposal files under.
pub(crate) const TOPIC: &str = "/taste/policy";

fn topic() -> Option<wire::TopicInput> {
    Some(TOPIC.parse().expect("the taste topic is a public topic"))
}

/// Recent fires carried per taste.
///
/// The curation view shows fires and their ages rather than an all-time count,
/// so each row is bounded and newest first. Nothing is persisted: the whole
/// view is derived from one journal snapshot on every call.
const RECENT_FIRES: usize = 12;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Surface {
    text: Option<String>,
    location: String,
    digest: Digest,
    tastes: Vec<TasteDefinition>,
}

impl Surface {
    fn load(repository: &Repository) -> Result<Self, LoadError> {
        let loaded = spice::config::capture_repository(repository, &[])?;
        Ok(Self::from_loaded(&loaded))
    }

    fn scoped(
        repository: &Repository,
        configuration: Option<&spice::config::RepositoryCapture>,
    ) -> Result<Self, LoadError> {
        let loaded = spice::config::capture_repository_scoped(repository, &[], configuration)?;
        Ok(Self::from_loaded(&loaded))
    }

    fn from_loaded(loaded: &spice::config::Loaded) -> Self {
        let text = loaded.tracked().map(ToOwned::to_owned);
        let location = loaded.locations().tracked().display().to_string();
        let digest = tracked_digest(text.as_deref());
        let tastes = loaded
            .resolved()
            .root()
            .taste()
            .bag()
            .tastes()
            .iter()
            .map(|taste| TasteDefinition {
                name: taste.name().clone(),
                words: taste
                    .triggers()
                    .iter()
                    .map(|trigger| trigger.as_str().to_owned())
                    .collect(),
                message: taste.message().to_owned(),
                enabled: taste.enabled(),
                scopes: taste.scopes().clone(),
            })
            .collect();
        Self {
            text,
            location,
            digest,
            tastes,
        }
    }

    fn provenance(&self) -> TasteProvenance {
        TasteProvenance {
            source: ConfigSource::Tracked,
            location: self.location.clone(),
            present: self.text.is_some(),
            digest: self.text.as_ref().map(|_| self.digest.clone()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TasteDefinition {
    name: Name,
    words: Vec<String>,
    message: String,
    enabled: bool,
    scopes: gate::ScopeSelector,
}

/// Tracked taste rows and only the evidence frontier this request can render.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TasteSnapshot {
    digest: Digest,
    provenance: TasteProvenance,
    horizon: Seq,
    tastes: Vec<TasteRow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TasteProvenance {
    source: ConfigSource,
    location: String,
    present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<Digest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TasteRow {
    name: Name,
    words: Vec<String>,
    message: String,
    enabled: bool,
    scopes: gate::ScopeSelector,
    fires: Vec<TasteFire>,
    vendors: Vec<VendorTally>,
    recurrence: Vec<Recurrence>,
    confirmed: usize,
    cleared: usize,
    judgments: Vec<TasteJudgment>,
    history_population: usize,
    #[serde(skip)]
    history_start: Option<HistoryCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HistoryCursor {
    horizon: Seq,
    position: Seq,
    kind: HistoryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryKind {
    Fire,
    Judgment,
}

impl HistoryCursor {
    fn parse(value: &str) -> Option<Self> {
        let mut fields = value.split('/');
        let horizon = Seq::new(fields.next()?.parse().ok()?)?;
        let position = Seq::new(fields.next()?.parse().ok()?)?;
        let kind = match fields.next()? {
            "fire" => HistoryKind::Fire,
            "judgment" => HistoryKind::Judgment,
            _ => return None,
        };
        fields.next().is_none().then_some(Self {
            horizon,
            position,
            kind,
        })
    }

    fn key(self) -> String {
        let kind = match self.kind {
            HistoryKind::Fire => "fire",
            HistoryKind::Judgment => "judgment",
        };
        format!("{}/{}/{kind}", self.horizon, self.position)
    }
}

/// One reminder this taste opened, with the standing of its stable directive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TasteFire {
    position: Seq,
    at: Instant,
    lane: CanonicalUuid,
    vendor: Name,
    occupant: CanonicalUuid,
    compaction: u64,
    content: Digest,
    key: String,
    standing: Disposition,
    locus: String,
}

struct FireEvidence {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    compaction: u64,
    content: Digest,
    directive: CanonicalUuid,
    key: String,
    locus: wire::body::TranscriptLocus,
}

/// Fires and answers for one vendor, over the fires this report carries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct VendorTally {
    vendor: Name,
    fires: usize,
    acked: usize,
    declined: usize,
}

/// One unchanged identity that fired again in a later compaction epoch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Recurrence {
    content: Digest,
    epochs: usize,
    latest: Seq,
}

/// A join this report refuses to guess at.
enum EvidenceError {
    /// A reminder names a lane the durable stream never seated.
    ///
    /// Nothing in the stream requires a reminded lane to have been seated
    /// first, so this is the one join a real journal can actually be missing.
    LaneUnseated(CanonicalUuid),
    /// A reminder names a directive its own steering standing does not carry.
    ///
    /// The standing is replayed from the same snapshot the reminder came from,
    /// and the replay opens a directive for every reminder it absorbs, so this
    /// join can only fail if the store contradicts itself.
    DirectiveMissing(CanonicalUuid),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TasteJudgment {
    position: Seq,
    at: Instant,
    verdict: Verdict,
    locus: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bag: Option<Digest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attempts: Vec<TasteJudgeAttempt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Confirmed,
    Cleared,
}

/// Authenticate through the resident and report one exact tracked generation.
pub(crate) fn report(
    matches: &ArgMatches,
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<view::TasteView> {
    let page = match crate::read_page::Page::from_matches(matches, &["taste"]) {
        Ok(page) => page,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let name = match matches
        .get_one::<String>("name")
        .map(|written| written.parse::<Name>())
        .transpose()
    {
        Ok(name) => name,
        Err(error) => {
            return refusal(
                "taste::name-invalid",
                Some("one current lower-case taste name".to_owned()),
                Some(error.to_string()),
                "copy one exact name from spice taste output",
            );
        }
    };
    let query = match name.as_ref() {
        None => SnapshotQuery::Summary,
        Some(name) => {
            let after = if let Some(value) = page.after() {
                let Some(cursor) = HistoryCursor::parse(value) else {
                    return Outcome::Refusal(
                        page.cursor_refusal("HORIZON/POSITION/fire or HORIZON/POSITION/judgment"),
                    );
                };
                Some(cursor)
            } else {
                None
            };
            if page.retain() > journal::MAX_RETAINED_FACT_PAGE_ROWS {
                return refusal(
                    "taste::bound-exceeded",
                    Some(format!(
                        "--limit at most {}",
                        journal::MAX_RETAINED_FACT_PAGE_ROWS - 1
                    )),
                    Some(page.limits().rows.to_string()),
                    "lower --limit for a bounded taste history read",
                );
            }
            SnapshotQuery::History {
                name,
                after,
                unique_limit: page.retain(),
            }
        }
    };
    match snapshot::<view::TasteView>(contract, configuration, query, ambient) {
        Ok((_, reply)) => match view::project(reply.body, page, name.as_ref()) {
            Ok(body) => Outcome::Reply(Reply {
                position: reply.position,
                body,
            }),
            Err(refusal) => Outcome::Refusal(refusal),
        },
        Err(outcome) => outcome,
    }
}

/// Create reviewed work for one evidence-backed taste admission.
pub(crate) fn add(
    matches: &ArgMatches,
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<TaskAnswer> {
    let evidence = matches
        .get_one::<String>("TEXT")
        .expect("the generated taste add syntax requires TEXT")
        .clone();
    if evidence.trim().is_empty() {
        return refusal(
            "taste::evidence-empty",
            Some("non-empty recurring evidence".to_owned()),
            Some(evidence),
            "describe the recurring evidence that motivates the proposed taste",
        );
    }
    let origin = match crate::board_command::parse_origin(matches) {
        Ok(origin) => origin,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(outcome) => return outcome,
    };
    let surface = match Surface::scoped(&repository, configuration) {
        Ok(surface) => surface,
        Err(error) => return load_failure(&error),
    };
    let title = format!("Propose tracked taste addition: {}", headline(&evidence));
    let description = format!(
        concat!(
            "Recurring evidence:\n\n{}\n\n",
            "Tracked source: {}\nTracked generation: {}"
        ),
        evidence, surface.location, surface.digest,
    );
    submit_plan(
        Host::new(&repository, ambient),
        contract,
        TaskAdd {
            preparation: None,
            intent: None,
            title,
            pitch: "A correction that keeps recurring deserves a tracked taste.".to_owned(),
            topic: topic(),
            description: Some(description),
            term: vec![
                format!(
                    "Operator review adds one exact [taste.NAME] row to tracked {}.",
                    wire::TRACKED_POLICY_FILE
                ),
                concat!(
                    "Paired behavior and mention fixtures confirm and clear the proposed bag, ",
                    "and the standing non-hit corpus remains clear."
                )
                .to_owned(),
            ],
            requirements: wire::EnvironmentRequirements::default(),
            origin,
            flow: Some("todo,review".to_owned()),
            depends: Vec::new(),
            related: Vec::new(),
            claim: false,
            draft: false,
        },
    )
}

/// Create reviewed work for one current tracked taste retirement.
pub(crate) fn drop(
    matches: &ArgMatches,
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<TaskAnswer> {
    let written = matches
        .get_one::<String>("NAME")
        .expect("the generated taste drop syntax requires NAME");
    let name = match written.parse::<Name>() {
        Ok(name) => name,
        Err(error) => {
            return refusal(
                "taste::name-invalid",
                Some("one current lower-case taste name".to_owned()),
                Some(error.to_string()),
                "copy one exact name from spice taste output",
            );
        }
    };
    let origin = match crate::board_command::parse_origin(matches) {
        Ok(origin) => origin,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let (repository, reply) = match snapshot::<TaskAnswer>(
        contract,
        configuration,
        SnapshotQuery::Curation(&name),
        ambient,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(row) = reply.body.tastes.iter().find(|row| row.name == name) else {
        return refusal(
            "taste::unknown",
            Some(
                reply
                    .body
                    .tastes
                    .iter()
                    .map(|row| row.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(name.to_string()),
            "read the current tracked taste set and name one tracked taste",
        );
    };
    let description = retirement_description(row, &reply.body);
    submit_plan(
        Host::new(&repository, ambient),
        contract,
        TaskAdd {
            preparation: None,
            intent: None,
            title: format!("Propose tracked taste retirement: {}", row.name),
            pitch: "A taste that no longer earns its corrections should retire.".to_owned(),
            topic: topic(),
            description: Some(description),
            term: vec![
                format!(
                    "Operator review narrows, revises, or deletes [taste.{}] in tracked {}.",
                    row.name,
                    wire::TRACKED_POLICY_FILE
                ),
                "Earlier durable judgments remain history and cannot restore deleted membership."
                    .to_owned(),
            ],
            requirements: wire::EnvironmentRequirements::default(),
            origin,
            flow: Some("todo,review".to_owned()),
            depends: Vec::new(),
            related: Vec::new(),
            claim: false,
            draft: false,
        },
    )
}

/// The retirement proposal's description: the tracked row, its source, and its
/// cited declined and cleared loci.
fn retirement_description(row: &TasteRow, snapshot: &TasteSnapshot) -> String {
    let declines = cited(
        row.fires
            .iter()
            .filter(|fire| fire.standing == Disposition::Declined)
            .map(|fire| {
                format!(
                    "- {} on {} at {} (journal {})",
                    fire.locus, fire.vendor, fire.at, fire.position
                )
            }),
    );
    let clearances = cited(
        row.judgments
            .iter()
            .filter(|judgment| judgment.verdict == Verdict::Cleared)
            .map(|judgment| {
                format!(
                    "- {} at {} (journal {})",
                    judgment.locus, judgment.at, judgment.position
                )
            }),
    );
    let words = serde_json::to_string(&row.words).expect("taste words encode as JSON");
    let message = serde_json::to_string(&row.message).expect("taste message encodes as JSON");
    let scopes = render_scopes(&row.scopes);
    format!(
        concat!(
            "Current tracked row:\n",
            "[taste.{name}]\nwords = {words}\nmessage = {message}\n",
            "enabled = {enabled}\nscopes = {scopes}\n\n",
            "Tracked source: {location}\nTracked generation: {digest}\n\n",
            "Recent declined loci:\n{declines}\n\n",
            "Historical cleared loci:\n{clearances}"
        ),
        name = row.name,
        words = words,
        message = message,
        enabled = row.enabled,
        scopes = scopes,
        location = snapshot.provenance.location,
        digest = snapshot.digest,
        declines = declines,
        clearances = clearances,
    )
}

fn render_scopes(scopes: &gate::ScopeSelector) -> String {
    let axes = [
        ("paths", scopes.paths()),
        ("extensions", scopes.extensions()),
        ("vendors", scopes.vendors()),
        ("models", scopes.models()),
        ("phases", scopes.phases()),
    ];
    let entries = axes
        .into_iter()
        .filter(|(_, values)| !values.is_empty())
        .map(|(name, values)| {
            let values = serde_json::to_string(values).expect("taste scopes encode as JSON");
            format!("{name} = {values}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{ {entries} }}")
}

const RECENT_JUDGMENTS: usize = 12;

enum SnapshotQuery<'a> {
    Summary,
    History {
        name: &'a Name,
        after: Option<HistoryCursor>,
        unique_limit: usize,
    },
    Curation(&'a Name),
}

enum SnapshotReadError {
    Journal(journal::GuardError),
    Evidence(EvidenceError),
    CursorMissing,
}

impl From<journal::GuardError> for SnapshotReadError {
    fn from(error: journal::GuardError) -> Self {
        Self::Journal(error)
    }
}

fn snapshot<B>(
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    query: SnapshotQuery<'_>,
    ambient: &Ambient,
) -> Result<(Repository, Reply<TasteSnapshot>), Outcome<B>> {
    let repository = current_repository()?;
    let state = repository
        .admit_state()
        .map_err(|error| crate::state_failure(&error).outcome())?;
    let epoch = crate::command_client::authenticate(state.serve(), contract, ambient)
        .map_err(crate::typed_outcome)?;
    let surface =
        Surface::scoped(&repository, configuration).map_err(|error| load_failure(&error))?;
    let reader = journal::JournalReader::open(state.journal()).map_err(|error| {
        fault(
            FaultClass::Store,
            "taste::journal-unreadable",
            error.to_string(),
        )
    })?;
    let read = reader.read_with(|read| {
        let residency = read.residency_head()?;
        let cursor = read.cursor()?.ok_or(SnapshotReadError::CursorMissing)?;
        let horizon = match query {
            SnapshotQuery::History {
                after: Some(after), ..
            } => after.horizon,
            SnapshotQuery::Summary
            | SnapshotQuery::History { after: None, .. }
            | SnapshotQuery::Curation(_) => cursor,
        };
        let tastes = match query {
            SnapshotQuery::Summary => surface
                .tastes
                .iter()
                .map(|taste| summary_row(read, taste, horizon))
                .collect::<Result<Vec<_>, _>>()?,
            SnapshotQuery::History {
                name,
                after,
                unique_limit,
            } => surface
                .tastes
                .iter()
                .filter(|taste| taste.name == *name)
                .map(|taste| history_row(read, taste, horizon, after, unique_limit))
                .collect::<Result<Vec<_>, _>>()?,
            SnapshotQuery::Curation(name) => surface
                .tastes
                .iter()
                .filter(|taste| taste.name == *name)
                .map(|taste| {
                    history_row(read, taste, horizon, None, RECENT_FIRES + RECENT_JUDGMENTS)
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok::<_, SnapshotReadError>((residency, cursor, horizon, tastes))
    });
    let (residency, cursor, horizon, tastes) = match read {
        Ok(answer) => answer,
        Err(journal::ReadError::Journal(error))
        | Err(journal::ReadError::Answer(SnapshotReadError::Journal(error))) => {
            return Err(fault(
                FaultClass::Store,
                "taste::journal-unreadable",
                error.to_string(),
            ));
        }
        Err(journal::ReadError::Answer(SnapshotReadError::Evidence(error))) => {
            return Err(evidence_failure(error));
        }
        Err(journal::ReadError::Answer(SnapshotReadError::CursorMissing)) => {
            return Err(fault(
                FaultClass::Store,
                "taste::cursor-missing",
                "the authenticated resident has no durable journal position".to_owned(),
            ));
        }
    };
    let Some(residency) = residency else {
        return Err(fault(
            FaultClass::Store,
            "taste::residency-missing",
            "the authenticated resident has no durable residency head".to_owned(),
        ));
    };
    if residency.body().epoch != epoch {
        return Err(refusal(
            "taste::epoch-moved",
            Some(epoch.to_string()),
            Some(residency.body().epoch.to_string()),
            "reconnect through the current resident discovery and retry",
        ));
    }
    let current = Surface::load(&repository).map_err(|error| load_failure(&error))?;
    if current != surface {
        return Err(refusal(
            "taste::surface-moved",
            Some(surface.digest.to_string()),
            Some(current.digest.to_string()),
            "retry against the current tracked taste configuration",
        ));
    }
    let report = TasteSnapshot {
        digest: surface.digest.clone(),
        provenance: surface.provenance(),
        horizon,
        tastes,
    };
    Ok((
        repository,
        Reply {
            position: ReplyPosition::Snapshot(cursor),
            body: report,
        },
    ))
}

fn summary_row(
    read: &journal::JournalRead<'_>,
    taste: &TasteDefinition,
    horizon: Seq,
) -> Result<TasteRow, SnapshotReadError> {
    let evidence = read.taste_evidence_summary(taste.name.as_str(), horizon, RECENT_FIRES)?;
    let fires = evidence
        .fires
        .iter()
        .map(|envelope| indexed_fire(read, envelope))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(taste_row(
        taste,
        fires,
        evidence.confirmed,
        evidence.cleared,
        Vec::new(),
        0,
        None,
    ))
}

fn history_row(
    read: &journal::JournalRead<'_>,
    taste: &TasteDefinition,
    horizon: Seq,
    after: Option<HistoryCursor>,
    unique_limit: usize,
) -> Result<TasteRow, SnapshotReadError> {
    let through = after.map_or(horizon, |cursor| cursor.position);
    let evidence = read.taste_evidence_page(taste.name.as_str(), horizon, through, unique_limit)?;
    let mut fires = Vec::new();
    let mut judgments = Vec::new();
    for row in evidence.rows {
        if row.fire {
            fires.push(indexed_fire(read, &row.envelope)?);
        }
        if row.judgment {
            let (_, judgment) = indexed_judgment(&row.envelope).ok_or_else(|| {
                SnapshotReadError::Journal(journal::GuardError::Invalid(format!(
                    "taste judgment index at {} names {}",
                    row.envelope.seq,
                    row.envelope.fact.kind()
                )))
            })?;
            judgments.push(judgment);
        }
    }
    Ok(taste_row(
        taste,
        fires,
        evidence.confirmed,
        evidence.cleared,
        judgments,
        evidence.population,
        after,
    ))
}

fn taste_row(
    taste: &TasteDefinition,
    fires: Vec<TasteFire>,
    confirmed: usize,
    cleared: usize,
    judgments: Vec<TasteJudgment>,
    history_population: usize,
    history_start: Option<HistoryCursor>,
) -> TasteRow {
    TasteRow {
        name: taste.name.clone(),
        words: taste.words.clone(),
        message: taste.message.clone(),
        enabled: taste.enabled,
        scopes: taste.scopes.clone(),
        vendors: tally(&fires),
        recurrence: recurrence(&fires),
        fires,
        confirmed,
        cleared,
        judgments,
        history_population,
        history_start,
    }
}

fn indexed_fire(
    read: &journal::JournalRead<'_>,
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Result<TasteFire, SnapshotReadError> {
    let evidence = fire_evidence(envelope).ok_or_else(|| {
        SnapshotReadError::Journal(journal::GuardError::Invalid(format!(
            "taste fire index at {} names {}",
            envelope.seq,
            envelope.fact.kind()
        )))
    })?;
    let vendor = read
        .taste_lane_vendor(evidence.lane)?
        .ok_or(SnapshotReadError::Evidence(EvidenceError::LaneUnseated(
            evidence.lane,
        )))?;
    let standing = read
        .steering_head(evidence.directive)?
        .ok_or(SnapshotReadError::Evidence(
            EvidenceError::DirectiveMissing(evidence.directive),
        ))?
        .body()
        .disposition;
    Ok(TasteFire {
        position: envelope.seq,
        at: envelope.at,
        lane: evidence.lane,
        vendor,
        occupant: evidence.occupant,
        compaction: evidence.compaction,
        content: evidence.content,
        key: evidence.key,
        standing,
        locus: locus_text(&evidence.locus),
    })
}

fn fire_evidence(envelope: &wire::Envelope<wire::AnyFact>) -> Option<FireEvidence> {
    if let Some(body) = envelope.fact.read::<TasteReminded>() {
        return Some(FireEvidence {
            lane: body.lane,
            occupant: body.occupant,
            compaction: body.compaction,
            content: body.content,
            directive: body.directive,
            key: body.key,
            locus: body.locus,
        });
    }
    let body = envelope.fact.read::<TasteJudged>()?;
    let TasteDecision::Confirmed { directive, key, .. } = body.decision else {
        return None;
    };
    Some(FireEvidence {
        lane: body.lane,
        occupant: body.occupant,
        compaction: body.compaction,
        content: body.content,
        directive,
        key,
        locus: body.locus,
    })
}

fn indexed_judgment(envelope: &wire::Envelope<wire::AnyFact>) -> Option<(String, TasteJudgment)> {
    let body = envelope.fact.read::<TasteJudged>()?;
    Some((
        body.taste,
        TasteJudgment {
            position: envelope.seq,
            at: envelope.at,
            verdict: match body.decision {
                TasteDecision::Confirmed { .. } => Verdict::Confirmed,
                TasteDecision::Cleared => Verdict::Cleared,
            },
            locus: locus_text(&body.locus),
            content: Some(body.content),
            bag: Some(body.bag),
            trigger: Some(body.trigger),
            vendor: Some(body.vendor),
            source: Some(body.source),
            compaction: Some(body.compaction),
            attempts: body.attempts,
        },
    ))
}

/// Fires and answers per vendor, over the fires the row carries.
fn tally(fires: &[TasteFire]) -> Vec<VendorTally> {
    let mut tallies = BTreeMap::<Name, VendorTally>::new();
    for fire in fires {
        let tally = tallies
            .entry(fire.vendor.clone())
            .or_insert_with(|| VendorTally {
                vendor: fire.vendor.clone(),
                fires: 0,
                acked: 0,
                declined: 0,
            });
        tally.fires += 1;
        match fire.standing {
            Disposition::Acked => tally.acked += 1,
            Disposition::Declined => tally.declined += 1,
            _ => {}
        }
    }
    tallies.into_values().collect()
}

/// Identities that fired again after a compaction epoch moved.
fn recurrence(fires: &[TasteFire]) -> Vec<Recurrence> {
    let mut spans = BTreeMap::<Digest, (BTreeSet<u64>, Seq)>::new();
    for fire in fires {
        let span = spans
            .entry(fire.content.clone())
            .or_insert_with(|| (BTreeSet::new(), fire.position));
        span.0.insert(fire.compaction);
        span.1 = span.1.max(fire.position);
    }
    spans
        .into_iter()
        .filter(|(_, (epochs, _))| epochs.len() > 1)
        .map(|(content, (epochs, latest))| Recurrence {
            content,
            epochs: epochs.len(),
            latest,
        })
        .collect()
}

fn locus_text(locus: &wire::body::TranscriptLocus) -> String {
    let coordinates = format!(
        "tape {}+{} event {} line {} column {}",
        locus.member, locus.offset, locus.event, locus.line, locus.column
    );
    match locus.source {
        Some(source) => format!("source {source} {coordinates}"),
        None => coordinates,
    }
}

fn submit_plan(host: Host<'_>, contract: &Contract, plan: TaskAdd) -> Outcome<TaskAnswer> {
    match crate::board_command::submit_task_add(host, contract, plan) {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::Task(answer) => Outcome::Reply(Reply {
                position: reply.position,
                body: *answer,
            }),
            answer => fault(
                FaultClass::Protocol,
                "taste::task-answer",
                format!("expected a task answer, received {answer:?}"),
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

/// Render cited evidence, spelling an empty citation rather than dropping it.
fn cited(lines: impl Iterator<Item = String>) -> String {
    let lines = lines.collect::<Vec<_>>();
    if lines.is_empty() {
        "- none".to_owned()
    } else {
        lines.join("\n")
    }
}

fn headline(evidence: &str) -> String {
    let normalized = evidence.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let prefix = chars.by_ref().take(72).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn tracked_digest(text: Option<&str>) -> Digest {
    let mut framed = Vec::new();
    framed.extend_from_slice(ConfigSource::Tracked.as_str().as_bytes());
    framed.push(0);
    match text {
        None => framed.push(0),
        Some(text) => {
            framed.push(1);
            let length = u64::try_from(text.len()).expect("configuration text length fits u64");
            framed.extend_from_slice(&length.to_be_bytes());
            framed.extend_from_slice(text.as_bytes());
        }
    }
    Digest::compute(Domain::ConfigExecutableSurface, &framed)
}

fn current_repository<B>() -> Result<Repository, Outcome<B>> {
    let current = std::env::current_dir().map_err(|error| {
        fault(
            FaultClass::Environment,
            "taste::working-directory-unavailable",
            error.to_string(),
        )
    })?;
    Repository::discover(&current).map_err(|error| {
        fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            error.to_string(),
        )
    })
}

/// Report a join the durable stream cannot answer, by name.
///
/// A missing seating or directive is evidence the report would otherwise have
/// to guess at, and a guess here reads as curation truth. It refuses instead,
/// naming the exact identity that did not join.
fn evidence_failure<B>(error: EvidenceError) -> Outcome<B> {
    match error {
        EvidenceError::LaneUnseated(lane) => refusal(
            "taste::lane-unseated",
            Some("one durable lane seating at or before the reminder".to_owned()),
            Some(lane.to_string()),
            "replay or repair the lane history that seats the reminded lane",
        ),
        // A refusal would offer the operator a remedy for something no request
        // of theirs caused. Replayed standing that lost a reminder it just
        // absorbed is a broken invariant, so it faults by name instead.
        EvidenceError::DirectiveMissing(directive) => fault(
            FaultClass::Store,
            "taste::directive-missing",
            format!("replayed steering standing does not carry directive {directive}"),
        ),
    }
}

fn load_failure<B>(error: &LoadError) -> Outcome<B> {
    if let Some(repository) = error.repository() {
        return fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            repository.to_string(),
        );
    }
    let detail = error.diagnostic();
    if error.unreadable() {
        fault(FaultClass::Environment, "taste::policy-unreadable", detail)
    } else {
        refusal(
            "taste::policy-refused",
            Some("one tracked taste set admitted by the typed registry".to_owned()),
            Some(detail),
            "correct the named configuration inputs and retry",
        )
    }
}

fn refusal<B>(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Outcome<B> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("tracked-taste-set".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    ))
}
