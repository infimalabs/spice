//! Bounded resident-side trust reporting over indexed current authority.

use bus::Bus;
use journal::{FactReadBudget, GuardError, JournalHead, JournalRead};
use wire::intent::TrustReportCursor;
use wire::intent::{
    CommandAnswer, TrustReportAnswer, TrustReportNext, TrustReportRead, TrustReportRuling,
};
use wire::output::{Exhausted, INTERACTIVE, Request};
use wire::{Digest, Outcome, Reply, ReplyPosition, SpiceCommand, TrustHead, TrustState};

use super::{store_fault, trust_show};

/// Answer one repository trust report from one immutable journal snapshot.
pub(super) fn read(bus: &Bus, request: &TrustReportRead) -> Outcome<CommandAnswer> {
    let budget = match INTERACTIVE.budget(Request {
        rows: Some(request.limits.rows),
        bytes: Some(request.limits.bytes),
        text: Some(request.limits.text),
    }) {
        Ok(budget) if budget.limits() == request.limits && request.limits.rows > 0 => budget,
        Ok(_) => {
            return store_fault(
                "trust::report-bound-invalid",
                "trust reports require at least one row",
            );
        }
        Err(error) => return store_fault("trust::report-bound-invalid", &error.to_string()),
    };
    let after = request.after.clone();
    let snapshot = bus.read(move |read| build(read, after.as_ref(), budget.limits()));
    match snapshot {
        Ok((cursor, answer)) => Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(cursor),
            body: CommandAnswer::TrustReport(Box::new(answer)),
        }),
        Err(error) => store_fault("trust::report-unreadable", &error.to_string()),
    }
}

fn build(
    read: &JournalRead<'_>,
    after: Option<&TrustReportCursor>,
    limits: wire::output::Limits,
) -> Result<(wire::Seq, TrustReportAnswer), GuardError> {
    let cursor = read
        .cursor()?
        .ok_or_else(|| GuardError::Invalid("a trust report requires a non-empty journal".into()))?;
    let frontier_budget = FactReadBudget::new(limits.rows, journal::MAX_RETAINED_FACT_PAGE_BYTES)
        .expect("interactive trust limits fit a repository read budget");
    let page = read.trust_report_head_page(after, frontier_budget)?;
    let baseline = match page
        .rows
        .iter()
        .find(|head| head.body().state == TrustState::Pended)
    {
        Some(pended) => match read.latest_other_granted_trust(&pended.body().digest)? {
            Some(granted) => {
                let documents = trust_show::recorded_documents(read, granted.position())?;
                validate_documents(&granted.body().digest, documents.as_ref())?;
                Some((granted.body().digest.clone(), documents))
            }
            None => None,
        },
        None => None,
    };

    let mut rulings = Vec::with_capacity(page.rows.len());
    let mut shown_bytes = 0usize;
    let mut withheld: Option<(TrustReportRuling, Exhausted)> = None;
    let mut resume_after = after.cloned();
    for head in page.rows {
        let shell = shell(&head);
        let shell_bytes = wire::encoded_len(&shell).map_err(|error| {
            GuardError::Invalid(format!("trust report shell cannot be encoded: {error}"))
        })?;
        if shell_bytes > limits.bytes.saturating_sub(shown_bytes) {
            withheld = Some((shell, Exhausted::Bytes));
            break;
        }
        let ruling = if head.body().state == TrustState::Pended {
            let documents = trust_show::recorded_documents(read, head.position())?;
            validate_documents(&head.body().digest, documents.as_ref())?;
            let differential = trust_show::answer(
                &head.body().digest,
                head.body().state,
                documents.as_ref(),
                baseline
                    .as_ref()
                    .map(|(digest, documents)| (digest, documents.as_ref())),
            );
            let full = TrustReportRuling {
                digest: head.body().digest.clone(),
                standing: head.body().state,
                differential: Some(differential),
                show: None,
            };
            if differential_fits_text(&full, limits.text) {
                let full_bytes = wire::encoded_len(&full).map_err(|error| {
                    GuardError::Invalid(format!("trust report ruling cannot be encoded: {error}"))
                })?;
                if full_bytes <= limits.bytes.saturating_sub(shown_bytes) {
                    full
                } else {
                    shell
                }
            } else {
                shell
            }
        } else {
            shell
        };
        shown_bytes += wire::encoded_len(&ruling).map_err(|error| {
            GuardError::Invalid(format!("trust report ruling cannot be encoded: {error}"))
        })?;
        resume_after = Some(TrustReportCursor {
            standing: ruling.standing,
            digest: ruling.digest.clone(),
        });
        rulings.push(ruling);
    }

    let next = match withheld {
        Some((ruling, exhausted)) => Some(next(ruling, exhausted, limits, resume_after.as_ref())?),
        None => page
            .next
            .as_ref()
            .map(shell)
            .map(|ruling| next(ruling, Exhausted::Rows, limits, resume_after.as_ref()))
            .transpose()?,
    };
    Ok((
        cursor,
        TrustReportAnswer {
            rulings,
            shown_bytes,
            next,
        },
    ))
}
fn differential_fits_text(ruling: &TrustReportRuling, limit: usize) -> bool {
    ruling.differential.as_ref().is_none_or(|differential| {
        differential.documents.as_ref().is_none_or(|documents| {
            documents.iter().all(|document| {
                document.lines.iter().all(|line| match line {
                    wire::intent::TrustDiffLine::Kept { text, .. }
                    | wire::intent::TrustDiffLine::Added { text, .. }
                    | wire::intent::TrustDiffLine::Removed { text, .. } => text.len() <= limit,
                    wire::intent::TrustDiffLine::Elided { .. } => true,
                })
            })
        })
    })
}

fn shell(head: &JournalHead<TrustHead>) -> TrustReportRuling {
    let pended = head.body().state == TrustState::Pended;
    TrustReportRuling {
        digest: head.body().digest.clone(),
        standing: head.body().state,
        differential: None,
        show: pended.then(|| {
            SpiceCommand::try_from(vec![
                "trust".to_owned(),
                "show".to_owned(),
                head.body().digest.to_string(),
            ])
            .expect("trust show is one canonical command")
        }),
    }
}

fn next(
    ruling: TrustReportRuling,
    exhausted: Exhausted,
    limits: wire::output::Limits,
    after: Option<&TrustReportCursor>,
) -> Result<TrustReportNext, GuardError> {
    let next_bytes = wire::encoded_len(&ruling).map_err(|error| {
        GuardError::Invalid(format!("trust report cursor cannot encode: {error}"))
    })?;
    let bytes = limits.bytes.max(next_bytes);
    let mut arguments = vec!["trust".to_owned()];
    if let Some(after) = after {
        arguments.extend(["--after".to_owned(), after.to_string()]);
    }
    arguments.extend([
        "--limit".to_owned(),
        limits.rows.to_string(),
        "--max-bytes".to_owned(),
        bytes.to_string(),
        "--max-text".to_owned(),
        limits.text.to_string(),
    ]);
    let continuation = SpiceCommand::try_from(arguments).map_err(|error| {
        GuardError::Invalid(format!("trust report continuation is invalid: {error}"))
    })?;
    Ok(TrustReportNext {
        exhausted,
        next_bytes,
        limits,
        continuation,
    })
}

fn validate_documents(
    digest: &Digest,
    documents: Option<&wire::body::TrustDocuments>,
) -> Result<(), GuardError> {
    if documents
        .is_some_and(|documents| crate::trust_command::documents_digest(documents) != *digest)
    {
        return Err(GuardError::Invalid(format!(
            "the documents recorded for {digest} hash to another digest"
        )));
    }
    Ok(())
}
