//! One composition sent to several agents as one fanout: a team or fleet
//! broadcast, or an Ask to chosen agents.

use super::*;

/// What a fanout detaches from its composer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::browser) enum Composition {
    /// A broadcast's primary body and direct images; its quotes stay drafted.
    Primary,
    /// An Ask's whole composition, quotes included.
    Whole,
}

/// Send `composition` from `source`'s composer to every target as one fanout.
/// Its carrying record is the source lane's own, or the only target's when
/// the creator sends to one agent. A creator send to several agents has no
/// carrier: it settles as one, holding the composition until every branch
/// has an outcome.
pub(in crate::browser) fn submit_broadcast(
    source: ComposerSource,
    composition: Composition,
    targets: &[BroadcastTarget],
    sender: &SenderHandle,
    drafts: &UseReducerHandle<ComposerDrafts>,
    submissions: &SubmissionHandle,
) -> bool {
    let carrier = match source {
        ComposerSource::Lane(lane) => {
            let Some(carrier) = targets.iter().position(|target| target.lane == lane) else {
                return false;
            };
            Some(carrier)
        }
        ComposerSource::Creator if targets.is_empty() => return false,
        ComposerSource::Creator => (targets.len() == 1).then_some(0),
    };
    if submissions.queue.borrow().holds(source) {
        return false;
    }
    let source_key = source_key(source);
    let mut current = drafts.0.clone();
    *current.draft_mut(source_key.clone()) = current_composer_draft(&source_key, drafts);
    let prepared = match composition {
        Composition::Primary => current.prepare_primary(&source_key),
        Composition::Whole => current.prepare(&source_key),
    };
    let prepared = match prepared {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return false,
        Err(error) => {
            report_refused_media(drafts, &error);
            return false;
        }
    };
    let message = prepared.message;
    let typed = crate::browser::input_history::typed_texts(&prepared.snapshot);
    let Ok(fanout) = random_uuid() else {
        return false;
    };
    let mut identities = Vec::with_capacity(targets.len());
    for _ in targets {
        let Ok(id) = random_uuid() else {
            return false;
        };
        identities.push(id);
    }
    let authored_millis = js_sys::Date::now() as i64;
    // One target is an ordinary send; several declare the identities whose
    // durable publication will prove delivery to the other recipients.
    let recipients = if targets.len() > 1 {
        targets
            .iter()
            .zip(&identities)
            .map(|(target, directive)| wire::body::SteeringRecipient {
                lane: target.lane,
                directive: *directive,
            })
            .collect()
    } else {
        Vec::new()
    };
    let payload = Submission::payload(
        fanout,
        authored_millis,
        message,
        prepared.attachments,
        recipients,
    );
    let mut snapshot = Some(prepared.snapshot);
    let mut batch = Vec::with_capacity(targets.len());
    for (index, (target, id)) in targets.iter().zip(identities).enumerate() {
        let submission = Submission::broadcast(
            id,
            fanout,
            target.lane,
            (target.occupant, target.epoch),
            Rc::clone(&payload),
            if Some(index) == carrier {
                snapshot.take()
            } else {
                None
            },
            source,
        );
        batch.push(submission);
    }
    if !queue_submissions(sender, submissions, batch, snapshot) {
        return false;
    }
    crate::browser::input_history::record(&source_key, &typed);
    true
}
