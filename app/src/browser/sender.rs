//! Admission of app actions and composer submissions into the outbound
//! mailbox, and the refusal each rejected admission reports.

use std::cell::RefCell;
use std::rc::Rc;

use wire::CanonicalUuid;

use super::{SubmissionHandle, toast};
use crate::action_mailbox::{self, ActionQueue};
use crate::client_action::ClientAction;
use crate::composer_drafts::ComposerDraft;
use crate::composer_submissions::Submission;

#[derive(Clone)]
pub(super) struct SenderHandle {
    pub(super) queue: Rc<RefCell<Option<ActionQueue>>>,
    /// Each refusal is its own toast, so a repeat of the same refusal shows
    /// again instead of matching the last one.
    pub(super) refusals: toast::Toaster,
}

pub(super) fn random_uuid() -> Result<CanonicalUuid, String> {
    let mut bytes = [0_u8; 16];
    web_sys::window()
        .ok_or_else(|| "browser window is unavailable".to_owned())?
        .crypto()
        .map_err(|_| "browser crypto is unavailable".to_owned())?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| "browser random source failed".to_owned())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        concat!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-",
            "{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        ),
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
    .parse()
    .map_err(|error| format!("browser command identity was invalid: {error}"))
}

pub(super) fn admit_action(sender: &SenderHandle, action: ClientAction) -> bool {
    let topology_id = if action.is_topology_mutation() {
        let Some(id) = action_identity(sender) else {
            return false;
        };
        Some(id)
    } else {
        None
    };
    let result = sender
        .queue
        .borrow()
        .as_ref()
        .ok_or(action_mailbox::ActionAdmissionError::Unavailable)
        .and_then(|queue| queue.admit(action, topology_id));
    match result {
        Ok(()) => true,
        Err(error) => {
            sender.refusals.settled(error.to_string());
            false
        }
    }
}

pub(super) fn action_identity(sender: &SenderHandle) -> Option<CanonicalUuid> {
    match random_uuid() {
        Ok(id) => Some(id),
        Err(error) => {
            sender.refusals.settled(error);
            None
        }
    }
}

pub(super) fn queue_submission(
    sender: &SenderHandle,
    submissions: &SubmissionHandle,
    submission: Submission,
) -> bool {
    let id = submission.id;
    if let Err(error) = submissions.queue.borrow_mut().enqueue(submission) {
        sender.refusals.settled(error);
        return false;
    }
    submissions.publish();
    if admit_action(sender, ClientAction::DrainSubmissions) {
        return true;
    }
    let removed = submissions.queue.borrow_mut().rollback(id);
    assert!(
        removed.is_some(),
        "a failed submission wake rolls back the enqueue it just made"
    );
    submissions.publish();
    false
}

/// Queue a fanout batch whole. With `settled`, the batch is one composer's
/// send to several agents that holds that content until every branch has an
/// outcome.
pub(super) fn queue_submissions(
    sender: &SenderHandle,
    submissions: &SubmissionHandle,
    batch: Vec<Submission>,
    settled: Option<ComposerDraft>,
) -> bool {
    if batch.is_empty() {
        return false;
    }
    let identities = batch
        .iter()
        .map(|submission| submission.id)
        .collect::<Vec<_>>();
    let admitted = match settled {
        Some(snapshot) => submissions
            .queue
            .borrow_mut()
            .enqueue_settled(snapshot, batch),
        None => submissions.queue.borrow_mut().enqueue_all(batch),
    };
    if let Err(error) = admitted {
        sender.refusals.settled(error);
        return false;
    }
    submissions.publish();
    if admit_action(sender, ClientAction::DrainSubmissions) {
        return true;
    }
    for id in identities {
        let removed = submissions.queue.borrow_mut().rollback(id);
        assert!(
            removed.is_some(),
            "a failed submission wake rolls back every enqueue it just made"
        );
    }
    submissions.publish();
    false
}
