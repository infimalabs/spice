//! Routing uses the validated command policy and one complete public-node corpus.

use std::sync::Arc;

use board::routing::{Decision, Node, Query, RouteError, RoutingRoots, Scored, Target};
use board::similarity::{TermCounts, utf8_prefix};
use wire::Phase;
use wire::intent::{RouteEvidence, RouteStructure, RouteSuffix, TaskRoute};

use super::mutation::ValidatedSnapshot;
use super::*;

mod authority;

/// Most bytes of an origin's statement or directive that join the query.
const ORIGIN_VIEW_BYTES: usize = 2048;

/// Recapture only a disposable routing observation for an automatic creation.
/// The unchanged request still carries every caller-authored witness. Explicit
/// placement previews and violated source or authority assumptions never retry.
pub(super) fn automatic(
    mut create: impl FnMut() -> Outcome<CommandAnswer>,
) -> Outcome<CommandAnswer> {
    loop {
        let outcome = create();
        if matches!(&outcome, Outcome::Refusal(Refusal::Bare { diagnostic })
            if diagnostic.reason() == "task::routing-snapshot-moved"
                || (diagnostic.reason() == "board::witness-moved"
                    && diagnostic.observed() == Some("task::routing-generation-moved")))
        {
            continue;
        }
        return outcome;
    }
}

/// Exact authored fields, kept separate from their normalized scoring terms.
pub(super) struct Statement<'a> {
    title: &'a str,
    pitch: &'a str,
    description: Option<&'a str>,
    term: &'a [String],
    phase: &'a Phase,
    tail: Option<&'a wire::TopicTail>,
}

impl<'a> Statement<'a> {
    pub(super) fn new(
        title: &'a str,
        pitch: &'a str,
        description: Option<&'a str>,
        term: &'a [String],
        phase: &'a Phase,
    ) -> Self {
        Self {
            title,
            pitch,
            description,
            term,
            phase,
            tail: None,
        }
    }

    pub(super) fn task(head: &'a TaskHead, phase: &'a Phase) -> Self {
        Self::new(
            &head.title,
            &head.pitch,
            head.description.as_deref(),
            &head.term,
            phase,
        )
        .with_topic(head.topic.named.as_ref())
    }

    pub(super) fn with_topic(mut self, named: Option<&'a wire::TopicInput>) -> Self {
        self.tail = match named {
            Some(wire::TopicInput::Tail(tail)) => Some(tail),
            _ => None,
        };
        self
    }

    fn terms(&self) -> TermCounts {
        TermCounts::statement(self.title, self.pitch, self.description, self.term)
    }
}

/// The bounded immutable origin a creation names, consulted only after the
/// statement alone cannot choose.
#[derive(Clone, serde::Serialize)]
pub(super) struct OriginView {
    source: wire::TaskProvenance,
    revision: Option<Seq>,
    text: String,
}

impl OriginView {
    /// A parent task's statement or a steering directive's text; document and
    /// operator-Draft provenance carry no origin view.
    pub(super) fn of(
        bus: &Bus,
        provenance: &wire::TaskProvenance,
    ) -> Result<Option<Self>, Outcome<CommandAnswer>> {
        let observed = match provenance {
            wire::TaskProvenance::Task(stamp) => {
                let stamp = stamp.clone();
                bus.read(move |read| {
                    Ok(read.task_head(&stamp)?.map(|head| {
                        let statement = head.body();
                        let mut text = String::with_capacity(ORIGIN_VIEW_BYTES);
                        for field in std::iter::once(statement.title.as_str())
                            .chain(std::iter::once(statement.pitch.as_str()))
                            .chain(statement.description.as_deref())
                        {
                            if text.len() == ORIGIN_VIEW_BYTES {
                                break;
                            }
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            let prefix = utf8_prefix(field, ORIGIN_VIEW_BYTES - text.len());
                            text.push_str(prefix);
                            if prefix.len() != field.len() {
                                break;
                            }
                        }
                        (head.position(), text)
                    }))
                })
            }
            wire::TaskProvenance::Steering(key) => {
                let key = key.clone();
                bus.read(move |read| {
                    Ok(read.steering_key(&key)?.map(|head| {
                        (
                            head.position(),
                            utf8_prefix(&head.body().text, ORIGIN_VIEW_BYTES).to_owned(),
                        )
                    }))
                })
            }
            wire::TaskProvenance::Document(_) | wire::TaskProvenance::OperatorDraft => {
                return Ok(None);
            }
        }
        .map_err(|error| store_fault("task::routing-origin-unreadable", error.to_string()))?;
        let (revision, text) = observed.map_or_else(
            || (None, String::new()),
            |(revision, text)| (Some(revision), text),
        );
        Ok(Some(Self {
            source: provenance.clone(),
            revision,
            text,
        }))
    }
}

fn origin_revision(
    read: &JournalRead<'_>,
    source: &wire::TaskProvenance,
) -> Result<Option<Seq>, GuardError> {
    match source {
        wire::TaskProvenance::Task(stamp) => Ok(read.task_head(stamp)?.map(|head| head.position())),
        wire::TaskProvenance::Steering(key) => {
            Ok(read.steering_key(key)?.map(|head| head.position()))
        }
        wire::TaskProvenance::Document(_) | wire::TaskProvenance::OperatorDraft => Ok(None),
    }
}

/// One route's destination and the evidence that chose it.
#[derive(Clone)]
pub(super) struct Routed {
    pub(super) topic: wire::TaskTopic,
    pub(super) route: TaskRoute,
}

pub(super) struct Capture {
    policy: Arc<drive::PolicyCapture>,
    residency: Seq,
    generation: Option<u64>,
    fingerprint: Digest,
    origin: Option<(wire::TaskProvenance, Option<Seq>)>,
    restricted: Result<Routed, RouteError>,
    unrestricted: Result<Routed, RouteError>,
}

impl Capture {
    pub(super) fn new(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        actor: &Actor,
        statement: Statement<'_>,
        origin: Option<OriginView>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let creator = match actor {
            Actor::Lane(lane) => Some(
                snapshot
                    .answer
                    .residency
                    .body
                    .lanes
                    .iter()
                    .find(|row| row.lane == *lane)
                    .ok_or_else(|| {
                        protocol_fault("team::lane-missing", "the acting lane has no current team")
                    })?
                    .team,
            ),
            _ => None,
        };
        Self::new_for_team(bus, snapshot, creator, statement, origin)
    }

    /// Existing tasks route under their immutable creator team, not the invoker.
    pub(super) fn new_for_team(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        creator: Option<wire::CanonicalUuid>,
        statement: Statement<'_>,
        origin: Option<OriginView>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let cursor = snapshot.cursor;
        let policy = Arc::clone(&snapshot.policy);
        let residency = snapshot.answer.residency.position;
        let (nodes, generation) = bus
            .read(move |read| {
                policy.require_current()?;
                if !read
                    .residency_head()?
                    .is_some_and(|head| head.position() == residency)
                {
                    return Err(GuardError::refused("team::membership-moved"));
                }
                if read.cursor()? != Some(cursor) {
                    return Err(GuardError::refused("task::routing-snapshot-moved"));
                }
                Ok((read.topic_routing_nodes()?, read.topic_revision()?))
            })
            .map_err(|error| match error {
                bus::BusReadError::Head(GuardError::Refused(refusal)) => {
                    Outcome::Refusal(Refusal::bare(refusal.into_diagnostic().0))
                }
                error => store_fault("task::routing-read-failed", error.to_string()),
            })?;
        let authority = authority::Authority::new(snapshot, creator, statement.phase);
        let unmatched = authority
            .as_ref()
            .ok()
            .and_then(authority::Authority::unmatched);
        // The unmatched topic collects heterogeneous work and must never
        // attract later work.
        let query = Query {
            statement: statement.terms(),
            origin: origin
                .as_ref()
                .map(|origin| TermCounts::from_texts([origin.text.as_str()])),
            tail: statement.tail.cloned(),
        };
        let scored = Scored::new(
            nodes
                .into_iter()
                .filter(|node| {
                    !wire::is_oops_topic(&node.path)
                        && unmatched.is_none_or(|unmatched| !unmatched.contains(&node.path))
                })
                .map(|node| {
                    let terms = TermCounts::from_texts(
                        std::iter::once(node.description.as_str())
                            .chain(node.documents.iter().map(|document| document.as_str())),
                    );
                    Node {
                        id: node.id,
                        path: node.path,
                        terms,
                    }
                })
                .collect(),
            &query,
        );
        let translate = |decision: Option<Decision>| match decision {
            Some(decision) => routed(decision, generation, statement.tail),
            None => undecided(statement.tail),
        };
        let restricted = authority
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|authority| {
                authority
                    .roots()
                    .map(|roots| translate(authority.pick(&scored, roots, true)))
            });
        let unrestricted = authority
            .as_ref()
            .map_err(Clone::clone)
            .map(|authority| translate(authority.pick(&scored, RoutingRoots::All, false)));
        let binding = serde_json::to_string(&(
            snapshot.policy.config(),
            snapshot.answer.residency.position,
            creator,
            generation,
            (
                statement.title,
                statement.pitch,
                statement.description,
                statement.term,
                statement.phase,
                statement.tail.map(wire::TopicTail::as_str),
            ),
            &origin,
            restricted.as_ref().ok().map(|routed| &routed.route),
            unrestricted.as_ref().ok().map(|routed| &routed.route),
        ))
        .map_err(|error| protocol_fault("task::routing-binding-invalid", error.to_string()))?;
        Ok(Self {
            policy: Arc::clone(&snapshot.policy),
            residency: snapshot.answer.residency.position,
            generation,
            fingerprint: board::digest_of(&binding),
            origin: origin.map(|origin| (origin.source, origin.revision)),
            restricted,
            unrestricted,
        })
    }

    pub(super) fn restricted(&self) -> Result<Routed, RouteError> {
        self.restricted.clone()
    }

    pub(super) fn unrestricted(&self) -> Result<Routed, RouteError> {
        self.unrestricted.clone()
    }

    pub(super) fn fingerprint(&self) -> &Digest {
        &self.fingerprint
    }

    pub(super) fn guard(&self, write: &mut JournalWrite) {
        let policy = Arc::clone(&self.policy);
        let residency = self.residency;
        let generation = self.generation;
        let origin = self.origin.clone();
        write.guard(JournalGuard::new(move |read| {
            policy.require_current()?;
            if let Some((source, revision)) = &origin
                && origin_revision(read, source)? != *revision
            {
                return Err(GuardError::refused("task::routing-origin-moved"));
            }
            if !read
                .residency_head()?
                .is_some_and(|head| head.position() == residency)
            {
                return Err(GuardError::refused("team::membership-moved"));
            }
            if read.topic_revision()? != generation {
                return Err(GuardError::refused("task::routing-generation-moved"));
            }
            Ok(())
        }));
    }
}

/// The router could not decide and no admitted unmatched topic receives the
/// work: the task stays unplaced, keeping any authored tail for its placement.
fn undecided(tail: Option<&wire::TopicTail>) -> Routed {
    Routed {
        topic: wire::TaskTopic::unplaced(tail.cloned()),
        route: TaskRoute::Unavailable {
            reason: UNDECIDED_ROUTE.to_owned(),
            detail: "no admitted topic explains the statement and no unmatched topic is declared"
                .to_owned(),
        },
    }
}

/// Diagnostic of a route that left its task unplaced.
pub(super) const UNDECIDED_ROUTE: &str = "task::route-undecided";

/// Bind a decision to its destination: a scored node by stable identity, the
/// unmatched topic by path for the commit to resolve.
fn routed(decision: Decision, generation: Option<u64>, tail: Option<&wire::TopicTail>) -> Routed {
    let parent = decision.target.topic().clone();
    let placed_node = match &decision.target {
        Target::Node(pick) => Some(pick.node),
        Target::Unmatched(_) => None,
    };
    let final_node = decision.destination_node;
    let unmatched = decision.stop == wire::intent::RouteStop::Unmatched;
    let topic = wire::TaskTopic {
        path: Some(decision.destination.clone()),
        placed: Some(parent),
        named: tail.cloned().map(wire::TopicInput::Tail),
        unmatched,
        nodes: (placed_node.is_some() || final_node.is_some()).then_some(wire::topic::TopicNodes {
            node: final_node,
            placed: placed_node,
        }),
    };
    let components = decision
        .residual
        .iter()
        .filter(|term| decision.destination.child(term).is_ok())
        .cloned()
        .collect::<Vec<_>>();
    let confidence = decision.confidence();
    let structure = decision.structure.map(|structure| {
        Box::new(RouteStructure {
            tail: tail
                .expect("structural routing requires an authored tail")
                .as_str()
                .to_owned(),
            prefix: structure.prefix,
            matched: structure.matched.map(|matched| matched.topic),
            matched_components: structure.matched_components,
            connection: structure.connection,
            composed_overlap: structure.composed_overlap,
            alternative: structure.alternative,
        })
    });
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a cosine lies in [0, 1], so its thousandths fit"
    )]
    let overlap = (decision.overlap * 1000.0).round() as u16;
    let route = TaskRoute::Pick {
        topic: decision.destination.clone(),
        evidence: RouteEvidence {
            confidence,
            tier: decision.tier,
            stop: decision.stop,
            overlap,
            residual: decision.residual,
            structure,
            suffix: (!components.is_empty()).then_some(RouteSuffix {
                parent: decision.destination,
                components,
                topic_revision: generation,
            }),
        },
    };
    Routed { topic, route }
}
