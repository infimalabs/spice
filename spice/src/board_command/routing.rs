//! A source-bound forecast of where the board's topic tree seats a statement.

use std::sync::Arc;

use board::routing::{RouteError, RoutingRoots};
use topic::evidence::Prepared;
use wire::Phase;
use wire::intent::TaskRoute;

use super::mutation::ValidatedSnapshot;
use super::*;

pub(super) mod authority;
pub(super) mod proposal;

const AUTOMATIC_ATTEMPTS: usize = 2;

/// Recapture one disposable routing or duplicate-screen observation for an
/// automatic creation, then return a second movement refusal rather than
/// spinning under continuous board activity. The unchanged request still
/// carries every caller-authored witness. Explicit placement previews and
/// violated source or authority assumptions never retry.
pub(super) fn automatic(
    mut create: impl FnMut() -> Outcome<CommandAnswer>,
) -> Outcome<CommandAnswer> {
    for attempt in 0..AUTOMATIC_ATTEMPTS {
        let outcome = create();
        let moved = matches!(&outcome, Outcome::Refusal(Refusal::Bare { diagnostic })
            if diagnostic.reason() == "task::routing-snapshot-moved"
                || (diagnostic.reason() == "board::witness-moved"
                    && diagnostic.observed().is_some_and(|observed|
                        observed == "task::routing-generation-moved"
                            || observed.starts_with("task::duplicate-screen-moved:"))));
        if moved && attempt + 1 < AUTOMATIC_ATTEMPTS {
            continue;
        }
        return outcome;
    }
    unreachable!("the positive automatic attempt bound always returns")
}

/// Exact authored fields, kept separate from their normalized scoring terms.
pub(super) struct Statement<'a> {
    title: &'a str,
    pitch: &'a str,
    description: Option<&'a str>,
    term: &'a [String],
    phase: &'a Phase,
    tail: Option<&'a wire::TopicTail>,
    for_approval: bool,
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
            for_approval: false,
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

    /// Manual approval still checks scope and freshness, but not the automatic filing floor.
    pub(super) fn for_approval(mut self) -> Self {
        self.for_approval = true;
        self
    }

    fn prepare(&self) -> Prepared {
        topic::evidence::prepare_statement("", self.title, self.pitch, self.description, self.term)
    }

    /// Every authored field as written, for naming a topic nobody else seeded.
    fn authored(&self) -> Vec<&'a str> {
        let mut texts = vec![self.title, self.pitch];
        texts.extend(self.description);
        texts.extend(self.term.iter().map(String::as_str));
        texts
    }
}

/// Revision-bound origin of a filing.
#[derive(Clone, serde::Serialize)]
pub(super) struct OriginView {
    source: wire::TaskProvenance,
    revision: Option<Seq>,
}

impl OriginView {
    /// Capture a task or steering revision so approval cannot outlive its origin.
    pub(super) fn of(
        bus: &Bus,
        provenance: &wire::TaskProvenance,
    ) -> Result<Option<Self>, Outcome<CommandAnswer>> {
        let revision = match provenance {
            wire::TaskProvenance::Task(stamp) => {
                let stamp = stamp.clone();
                bus.read(move |read| Ok(read.task_head(&stamp)?.map(|head| head.position())))
            }
            wire::TaskProvenance::Steering(key) => {
                let key = key.clone();
                bus.read(move |read| Ok(read.steering_key(&key)?.map(|head| head.position())))
            }
            _ => return Ok(None),
        }
        .map_err(|error| store_fault("task::routing-origin-unreadable", error.to_string()))?;
        Ok(Some(Self {
            source: provenance.clone(),
            revision,
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
    barrier: Digest,
}

pub(super) fn creator(
    snapshot: &ValidatedSnapshot,
    actor: &Actor,
) -> Result<Option<wire::CanonicalUuid>, Outcome<CommandAnswer>> {
    match actor {
        Actor::Lane(lane) => snapshot
            .answer
            .residency
            .body
            .lanes
            .iter()
            .find(|row| row.lane == *lane)
            .map(|row| Some(row.team))
            .ok_or_else(|| {
                protocol_fault("team::lane-missing", "the acting lane has no current team")
            }),
        _ => Ok(None),
    }
}

impl Capture {
    pub(super) fn new(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        actor: &Actor,
        statement: Statement<'_>,
        origin: Option<OriginView>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let creator = creator(snapshot, actor)?;
        Self::with_team(bus, snapshot, creator, statement, origin)
    }

    /// Existing tasks route under their immutable creator team, not the invoker.
    pub(super) fn new_for_team(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        creator: Option<wire::CanonicalUuid>,
        statement: Statement<'_>,
        origin: Option<OriginView>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        Self::with_team(bus, snapshot, creator, statement, origin)
    }

    pub(super) fn topology(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
    ) -> Result<(Vec<journal::TopicRoutingNode>, Option<u64>), Outcome<CommandAnswer>> {
        let cursor = snapshot.cursor;
        let policy = Arc::clone(&snapshot.policy);
        let residency = snapshot.answer.residency.position;
        bus.read(move |read| {
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
        })
    }

    fn with_team(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        creator: Option<wire::CanonicalUuid>,
        statement: Statement<'_>,
        origin: Option<OriginView>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let (nodes, generation) = Self::topology(bus, snapshot)?;
        let authority = authority::Authority::new(snapshot, creator, statement.phase);
        let restricted = match authority
            .and_then(|authority| authority.roots().map(|roots| (authority, roots)))
        {
            Err(error) => Err(error),
            Ok((authority, roots)) => Ok(proposal::choose(
                bus, snapshot, &authority, &roots, &nodes, &statement,
            )?),
        };
        let binding = serde_json::to_string(&(
            snapshot.policy.config(),
            snapshot.answer.residency.position,
            creator,
            generation,
            (
                statement.title,
                statement.pitch,
                &snapshot.answer.task_barrier,
                statement.description,
                statement.term,
                statement.phase,
                statement.tail.map(wire::TopicTail::as_str),
            ),
            &origin,
            restricted.as_ref().ok().map(|routed| &routed.route),
        ))
        .map_err(|error| protocol_fault("task::routing-binding-invalid", error.to_string()))?;
        Ok(Self {
            policy: Arc::clone(&snapshot.policy),
            residency: snapshot.answer.residency.position,
            generation,
            fingerprint: board::digest_of(&binding),
            origin: origin.map(|origin| (origin.source, origin.revision)),
            restricted,
            barrier: snapshot.answer.task_barrier.clone(),
        })
    }

    pub(super) fn restricted(&self) -> Result<Routed, RouteError> {
        self.restricted.clone()
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
        read_guard::guard_task_projection(write, self.barrier.clone());
    }
}

/// The router could not decide and no admitted unmatched topic receives the
/// work: the task stays unplaced, keeping any authored tail for its placement.
pub(super) fn undecided(tail: Option<&wire::TopicTail>) -> Routed {
    unavailable(
        tail,
        UNDECIDED_ROUTE,
        "no unique admitted filing destination; choose a topic with --topic",
    )
}

fn unavailable(tail: Option<&wire::TopicTail>, reason: &str, detail: &str) -> Routed {
    Routed {
        topic: wire::TaskTopic::unplaced(tail.cloned()),
        route: TaskRoute::Unavailable {
            reason: reason.to_owned(),
            detail: detail.to_owned(),
        },
    }
}

/// Diagnostic of a route that left its task unplaced.
pub(super) const UNDECIDED_ROUTE: &str = "task::route-undecided";

/// Bind the composed path to the observed stable node when one exists.
pub(super) fn routed(
    parent: wire::Topic,
    companions: Vec<wire::TaskStamp>,
    tree_lag: Option<wire::topic::TreeLag>,
    unmatched: bool,
    tail: Option<&wire::TopicTail>,
    nodes: &[journal::TopicRoutingNode],
) -> Routed {
    let path = tail.map_or_else(
        || parent.clone(),
        |tail| board::routing::compose(&parent, tail),
    );
    let id = |topic: &wire::Topic| {
        nodes
            .iter()
            .find(|node| &node.path == topic)
            .map(|node| node.id)
    };
    let topic = wire::TaskTopic {
        nodes: Some(wire::topic::TopicNodes {
            node: id(&path),
            placed: id(&parent),
        }),
        path: Some(path.clone()),
        placed: Some(parent),
        named: tail.cloned().map(wire::TopicInput::Tail),
        unmatched,
    };
    Routed {
        topic,
        route: TaskRoute::Pick {
            topic: path,
            companions,
            tree_lag,
            unmatched,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn persistent_movement_returns_after_the_second_automatic_attempt() {
        let attempts = Cell::new(0);
        let outcome = automatic(|| {
            attempts.set(attempts.get() + 1);
            Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
                "board::witness-moved",
                Some("current durable heads".to_owned()),
                None,
                Some("task::duplicate-screen-moved: sibling changed".to_owned()),
            )))
        });

        assert_eq!(attempts.get(), AUTOMATIC_ATTEMPTS);
        assert!(
            matches!(&outcome, Outcome::Refusal(refusal)
                if refusal.reason() == "board::witness-moved"
                    && refusal.diagnostic().and_then(wire::RefusalDiagnostic::observed)
                        == Some("task::duplicate-screen-moved: sibling changed")),
            "the second authoritative movement returns: {outcome:?}"
        );
    }
}
