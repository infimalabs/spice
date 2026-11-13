//! Authentication, packing, and typed failure policy for app subscriptions.

use std::collections::BTreeSet;

use bus::AppHeads;
use wire::{
    Actor, Fault, FaultClass, Overflow, Packed, Refusal, Seq, StreamFact, StreamFrame, TrustView,
    ViewSubject, WireVersion,
};

/// Typed refusal or fault returned before a stream frame changes client state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    pub(crate) fn admission(error: wire::StreamAdmissionError) -> Self {
        match error {
            wire::StreamAdmissionError::Wire(refusal) => Self::Refusal(refusal),
            wire::StreamAdmissionError::Epoch(fault)
            | wire::StreamAdmissionError::Cursor(fault) => Self::Fault(fault),
        }
    }

    /// Encode this failure as one current stream response.
    #[must_use]
    pub fn frame(self, epoch: wire::CanonicalUuid, id: Option<wire::CanonicalUuid>) -> StreamFrame {
        match self {
            Self::Refusal(refusal) => StreamFrame::Refusal {
                v: WireVersion::CURRENT,
                id: id.expect("decoded stream refusals retain their request id"),
                epoch,
                refusal,
            },
            Self::Fault(fault) => StreamFrame::Fault {
                v: WireVersion::CURRENT,
                epoch,
                id,
                fault,
            },
        }
    }
}

/// Admit `subjects` for `actor`; `operator` is whether its principal
/// currently holds the operator role.
pub(super) fn admit_subjects(
    actor: &Actor,
    operator: bool,
    subjects: &[ViewSubject],
) -> Result<BTreeSet<ViewSubject>, Failure> {
    if subjects.is_empty() {
        return Err(Failure::Refusal(refusal(
            "stream::subjects-empty",
            "subscribe to at least one visible subject",
        )));
    }
    let mut admitted = BTreeSet::new();
    for subject in subjects {
        admit_subject(actor, operator, *subject)?;
        if !admitted.insert(*subject) {
            return Err(Failure::Refusal(refusal(
                "stream::subject-duplicate",
                "each view subject may appear once",
            )));
        }
    }
    Ok(admitted)
}

pub(super) fn admit_subject(
    actor: &Actor,
    operator: bool,
    subject: ViewSubject,
) -> Result<(), Failure> {
    if matches!(
        subject,
        ViewSubject::Trust | ViewSubject::Messages | ViewSubject::Quota
    ) && !operator
    {
        return Err(Failure::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "stream::subject-authority",
                Some("principal".to_owned()),
                Some("operator".to_owned()),
                Some(actor.to_string()),
            ),
        )));
    }
    Ok(())
}

pub(super) fn required_horizon(heads: &AppHeads) -> Result<Seq, Failure> {
    heads.cursor().ok_or_else(|| {
        Failure::Fault(protocol_fault(
            "stream::cursor-missing",
            "resident authority has no durable stream cursor",
        ))
    })
}

pub(super) fn trust_rows(heads: &AppHeads) -> Vec<TrustView> {
    let mut rows = heads
        .trust()
        .iter()
        .map(|head| TrustView {
            position: head.position(),
            head: head.body().clone(),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.position
            .cmp(&right.position)
            .then_with(|| left.head.digest.cmp(&right.head.digest))
    });
    rows
}

pub(super) fn present_facts(packed: Packed<wire::Envelope<wire::AnyFact>>) -> Packed<StreamFact> {
    Packed {
        rows: packed
            .rows
            .into_iter()
            .filter(app_visible_fact)
            .map(StreamFact::from)
            .collect(),
        overflow: packed.overflow,
        horizon: packed.horizon,
    }
}

/// Whether one authority fact belongs on the operator app stream.
///
/// `journal::ingested` is a resident accounting receipt for a captured source
/// byte range. The separately bounded message subject carries the transcript
/// activity an operator can act on; sending both turns every growing
/// transcript into duplicate socket traffic and exposes internal accounting as
/// chronology. The receipt remains in durable authority for replay and
/// forensic reads.
pub(super) fn app_visible_fact(fact: &wire::Envelope<wire::AnyFact>) -> bool {
    !matches!(fact.fact.kind(), wire::FactKind::JournalIngested)
}

pub(super) fn pack_unique<T>(
    rows: Vec<T>,
    limit: usize,
    horizon: Seq,
    position: impl Fn(&T) -> Seq,
) -> Result<Packed<T>, Failure> {
    if rows.len() <= limit {
        return Ok(Packed {
            rows,
            overflow: None,
            horizon,
        });
    }
    let mut cut = limit;
    while cut > 0 && position(&rows[cut - 1]) == position(&rows[cut]) {
        cut -= 1;
    }
    if cut == 0 {
        return Err(Failure::Fault(protocol_fault(
            "stream::cursor-ambiguous",
            "one packed cursor group exceeds the configured row bound",
        )));
    }
    let omitted = u64::try_from(rows.len() - cut).expect("a vector length fits in u64");
    let continuation = position(&rows[cut]);
    let mut rows = rows;
    rows.truncate(cut);
    Ok(Packed {
        rows,
        overflow: Some(Overflow {
            omitted,
            continuation,
        }),
        horizon,
    })
}

pub(super) fn refusal(reason: &str, detail: &str) -> Refusal {
    Refusal::bare(wire::RefusalDiagnostic::new(
        reason,
        None,
        None,
        Some(detail.to_owned()),
    ))
}

pub(super) fn protocol_fault(reason: &str, detail: &str) -> Fault {
    Fault {
        class: FaultClass::Protocol,
        reason: reason.to_owned(),
        detail: detail.to_owned(),
    }
}

pub(super) fn thread_fault(error: impl std::fmt::Display) -> Failure {
    Failure::Fault(Fault {
        class: FaultClass::Store,
        reason: "stream::thread-invalid".to_owned(),
        detail: error.to_string(),
    })
}
