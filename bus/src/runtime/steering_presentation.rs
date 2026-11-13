//! One steering directive's presentation context, with the room
//! publication that opened it resolved once per read and shared by every
//! directive it opened.

use std::sync::Arc;

use journal::crossing::{CanonicalUuid, FactKind, Seq, SteeringHead, rooms};
use journal::{GuardError, JournalHead};

/// One exact steering head with the immutable facts that created and most
/// recently transitioned it, and the room publication that opened it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteeringPresentation {
    head: JournalHead<journal::crossing::SteeringHead>,
    created: journal::crossing::Envelope<journal::crossing::AnyFact>,
    current: journal::crossing::Envelope<journal::crossing::AnyFact>,
    publication: Option<Arc<SteeringPublication>>,
}

impl SteeringPresentation {
    /// Read one key's presentation context from an already-admitted snapshot.
    pub fn read(read: &journal::JournalRead<'_>, key: &str) -> Result<Option<Self>, GuardError> {
        Ok(Self::read_batch(read, &[key])?.pop())
    }

    /// Read each known key's context from one snapshot, in key order.
    /// Opening publications resolve in directive creation order, so the
    /// batch reads and decodes each publication it reaches once and every
    /// directive that publication opened shares it.
    pub(super) fn read_batch<K: AsRef<str>>(
        read: &journal::JournalRead<'_>,
        keys: &[K],
    ) -> Result<Vec<Self>, GuardError> {
        let mut heads = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(head) = read.steering_key(key.as_ref())? {
                heads.push(head);
            }
        }
        let mut order = (0..heads.len()).collect::<Vec<_>>();
        order.sort_unstable_by_key(|&index| heads[index].body().created);
        let mut publications = OpeningPublications::default();
        let mut opened = vec![None; heads.len()];
        for index in order {
            opened[index] = publications.opening(read, heads[index].body())?;
        }
        heads
            .into_iter()
            .zip(opened)
            .map(|(head, publication)| Self::with_facts(read, head, publication))
            .collect()
    }

    fn with_facts(
        read: &journal::JournalRead<'_>,
        head: JournalHead<journal::crossing::SteeringHead>,
        publication: Option<Arc<SteeringPublication>>,
    ) -> Result<Self, GuardError> {
        let fact = |position| {
            read.fact_page(
                journal::FactPageStart::At(position),
                position,
                journal::FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)
                    .expect("one steering context fact is a repository budget"),
            )?
            .rows
            .into_iter()
            .next()
            .ok_or_else(|| {
                GuardError::Invalid(format!(
                    "steering key {} has no context fact at {position}",
                    head.body().key
                ))
            })
        };
        let created = fact(head.body().created)?;
        let current = fact(head.position())?;
        Ok(Self {
            head,
            created,
            current,
            publication,
        })
    }

    #[must_use]
    pub const fn head(&self) -> &JournalHead<journal::crossing::SteeringHead> {
        &self.head
    }

    #[must_use]
    pub const fn created(&self) -> &journal::crossing::Envelope<journal::crossing::AnyFact> {
        &self.created
    }

    #[must_use]
    pub const fn current(&self) -> &journal::crossing::Envelope<journal::crossing::AnyFact> {
        &self.current
    }

    /// The message, question, or answer whose room publication names this directive,
    /// shared with every other directive that publication opened. Absent
    /// on every directive no room publication opened.
    #[must_use]
    pub fn publication(&self) -> Option<&Arc<SteeringPublication>> {
        self.publication.as_ref()
    }
}

/// Positions after a directive's creating fact within which the room
/// publication its own write staged must appear. A write stages each
/// directive before the one publication naming it, and one publication
/// opens at most a bounded fanout of directives plus its room records.
const PUBLICATION_WINDOW: u64 = 256;

/// The message a room publication opened its directives with, decoded once
/// and shared immutably by each of their presentations.
#[derive(Debug, Eq, PartialEq)]
pub struct SteeringPublication {
    operation: rooms::RoomPublicationKind,
    text: Option<String>,
}

impl SteeringPublication {
    /// The publication's typed verb.
    #[must_use]
    pub const fn operation(&self) -> &rooms::RoomPublicationKind {
        &self.operation
    }

    /// An operator post's exact composed body. A lane packet's body stays in
    /// its author's transcript, and its directive already holds only it.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

/// One decoded publication, its position, and the directives it names.
struct Resolved {
    position: Seq,
    directed: Vec<CanonicalUuid>,
    publication: Arc<SteeringPublication>,
}

/// The last publication one read resolved.
///
/// Callers resolve directives in ascending creation order. A lookup from
/// directive position `s` that finds publication `P` proves no publication
/// lies strictly between them, so every later directive created before `P`
/// reuses it without another read or decode.
#[derive(Default)]
pub(super) struct OpeningPublications {
    last: Option<Resolved>,
}

impl OpeningPublications {
    /// The first room publication after `head`'s creating fact, kept only
    /// when its `directed` list names that directive. Directive identities
    /// are unique and the journal admits a publication only beside the
    /// directives it names, so the containment is the proof; position merely
    /// bounds the indexed lookup.
    pub(super) fn opening(
        &mut self,
        read: &journal::JournalRead<'_>,
        head: &SteeringHead,
    ) -> Result<Option<Arc<SteeringPublication>>, GuardError> {
        let created = head.created;
        let limit = created.get().saturating_add(PUBLICATION_WINDOW);
        if !self
            .last
            .as_ref()
            .is_some_and(|last| last.position > created)
        {
            self.last = first_after(read, created, limit)?;
        }
        Ok(self
            .last
            .as_ref()
            .filter(|last| last.position.get() <= limit && last.directed.contains(&head.directive))
            .map(|last| Arc::clone(&last.publication)))
    }
}

/// Read and decode the first publication in `(created, limit]`.
fn first_after(
    read: &journal::JournalRead<'_>,
    created: Seq,
    limit: u64,
) -> Result<Option<Resolved>, GuardError> {
    let Some(cursor) = read.cursor()? else {
        return Ok(None);
    };
    if cursor <= created {
        return Ok(None);
    }
    let upper = Seq::new(limit.min(cursor.get()))
        .expect("a bounded position after a durable fact is positive");
    let page = read.selected_fact_page(
        &[FactKind::RoomPosted, FactKind::RoomConversed],
        Some(created),
        Some(upper),
        journal::FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)
            .expect("one room publication is a repository budget"),
    )?;
    let Some(envelope) = page.rows.into_iter().next() else {
        return Ok(None);
    };
    let decoded = match envelope.fact.read::<rooms::RoomPosted>() {
        Some(posted) => Some((posted.directed, posted.operation, Some(posted.text))),
        None => envelope
            .fact
            .read::<rooms::RoomConversed>()
            .map(|conversed| (conversed.directed, conversed.operation, None)),
    };
    Ok(decoded.map(|(directed, operation, text)| Resolved {
        position: envelope.seq,
        directed,
        publication: Arc::new(SteeringPublication { operation, text }),
    }))
}
