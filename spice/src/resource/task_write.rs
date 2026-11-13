//! The task resources that take a write: a task text, replaced under the
//! evidence its read named, and a task's invite or join act, each write one
//! `task join` request. An invite URL names its recipient; its body is the
//! requester's message, authored from the captured write on the requester's
//! tape as an outbox body is.

use wire::{Digest, TaskHandle};

use super::{Resource, SCHEME, command_rendering, decode, settled_document};
use crate::board_command::{JoinRequest, TaskText};
use crate::command_client::Host;

/// The task resource `spice://task/TASK/REST` names when REST is a writable
/// leaf, or nothing when it is another task read.
pub(super) fn resource(
    task: &str,
    rest: &[&str],
    query: &str,
    url: &str,
) -> Option<Result<Resource, String>> {
    let field = match rest {
        ["join"] => return Some(Act::parse(task, None, query, url).map(Resource::Act)),
        ["invite", recipient @ ..] if recipient.len() <= 1 => {
            let invite = Invite {
                recipient: recipient.first().copied(),
            };
            return Some(Act::parse(task, Some(invite), query, url).map(Resource::Act));
        }
        ["description"] => Field::Whole(TaskText::Description),
        ["terms"] => Field::Whole(TaskText::Term),
        ["term", item] => Field::Term((*item).to_owned()),
        _ => return None,
    };
    Some(Text::parse(task, field, query, url).map(Resource::Text))
}

/// What one task text URL addresses.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Field {
    /// The description or the whole term list, replaced under the digest of
    /// the text its read answered.
    Whole(TaskText),
    /// One term at the item handle its own text derives. The handle names the
    /// text a write replaces, so the URL takes no digest.
    Term(String),
}

/// One replaceable task text, and the digest a write of it named.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Text {
    pub(super) handle: TaskHandle,
    pub(super) field: Field,
    pub(super) digest: Option<Digest>,
}

impl Text {
    /// The task text `spice://task/TASK/FIELD` names, with the digest its
    /// query carries.
    fn parse(task: &str, field: Field, query: &str, url: &str) -> Result<Self, String> {
        let handle = handle(task, url)?;
        let digest = match &field {
            Field::Whole(_) => digest(query, url)?,
            Field::Term(_) if query.is_empty() => None,
            Field::Term(_) => {
                return Err(format!(
                    "{url} takes no query; the item handle names the text a write replaces"
                ));
            }
        };
        Ok(Self {
            handle,
            field,
            digest,
        })
    }

    /// The URL that reads this text.
    pub(super) fn read_url(&self) -> String {
        match &self.field {
            Field::Whole(text) => text_url(&self.handle, text.name()),
            Field::Term(item) => term_url(&self.handle, item),
        }
    }

    /// The current text under a first line naming the URL that replaces it.
    pub(super) fn document(&self, repository: Host<'_>) -> Result<String, String> {
        let task = crate::board_command::current_task(repository, &self.handle)
            .map_err(|outcome| command_rendering(&outcome))?;
        let statement = &task.statement.body;
        match &self.field {
            Field::Whole(text) => Ok(format!(
                "write the whole new {} to {}?digest={}; it replaces every line below this one\n{}",
                text.name(),
                text_url(&statement.handle, text.name()),
                text.digest(statement),
                text.current(statement),
            )),
            Field::Term(item) => {
                let (item, text) = crate::board_command::term_text(statement, item)
                    .map_err(|outcome| command_rendering(&outcome))?;
                Ok(format!(
                    "write this term's new text to {}; it replaces every line below this one\n{text}",
                    term_url(&statement.handle, &item),
                ))
            }
        }
    }

    /// Replace the text through the `task modify` intent.
    pub(super) fn replace(&self, repository: Host<'_>, content: &str) -> Result<String, String> {
        let outcome = match &self.field {
            Field::Whole(text) => crate::board_command::replace_text(
                repository,
                &self.handle,
                *text,
                self.digest.as_ref(),
                content,
            ),
            Field::Term(item) => {
                crate::board_command::replace_term(repository, &self.handle, item, content)
            }
        };
        settled_document(&outcome, command_rendering(&outcome))
    }
}

fn text_url(handle: &TaskHandle, leaf: &str) -> String {
    format!("{SCHEME}task/{handle}/{leaf}")
}

fn term_url(handle: &TaskHandle, item: &str) -> String {
    format!("{SCHEME}task/{handle}/term/{item}")
}

/// One task's invite or join act.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Act {
    handle: TaskHandle,
    /// Present for an invite, naming the lane its URL addressed, if any.
    invite: Option<Option<wire::OccupantAddress>>,
}

/// The invite path's optional trailing address, still unparsed.
struct Invite<'a> {
    recipient: Option<&'a str>,
}

impl Act {
    /// The act `spice://task/TASK/join` or `spice://task/TASK/invite[/ADDR]`
    /// names; it takes no query. An address the command line's `--invite=ADDR`
    /// refuses earns that refusal, rendered alike.
    fn parse(
        task: &str,
        invite: Option<Invite<'_>>,
        query: &str,
        url: &str,
    ) -> Result<Self, String> {
        if !query.is_empty() {
            return Err(format!("{url} takes no query; write its body"));
        }
        let handle = handle(task, url)?;
        let invite = invite
            .map(|invite| {
                invite
                    .recipient
                    .map(|written| {
                        crate::board_command::invite_recipient(written)
                            .map_err(|outcome| command_rendering(&outcome))
                    })
                    .transpose()
            })
            .transpose()?;
        Ok(Self { handle, invite })
    }

    /// What a read of the act's URL tells: the body its write takes.
    pub(super) fn grammar(&self) -> String {
        let Some(recipient) = self.invite else {
            return format!(
                "write an empty body to {SCHEME}task/{}/join to join the task's phase; accept an invitation by ACK or task join KEY",
                self.handle
            );
        };
        let url = format!("{SCHEME}task/{}/invite", self.handle);
        let reach = recipient.map_or_else(
            || {
                format!(
                    "write to {url} to recruit a teammate, or to {url}/ADDRESS to invite that lane"
                )
            },
            |address| format!("write to {url}/{address} to invite that lane"),
        );
        format!(
            "{reach}; the whole body, which may be empty, is your message to the invited lane, authored from your tape and delivered to that lane in the task's room; each write is one request"
        )
    }

    /// Whether the act is an invitation, whose written body is authored text.
    pub(super) const fn invites(&self) -> bool {
        self.invite.is_some()
    }

    /// The `task join` request a write spells, or why it spells none. An
    /// invite's whole body is its message, never a recipient, and only the
    /// captured host write of it proves those words; a join takes an empty
    /// body.
    pub(super) fn request(
        &self,
        body: &str,
        carrier: Option<wire::outbox::Carrier>,
    ) -> Result<JoinRequest, String> {
        let handle = self.handle.clone();
        let Some(recipient) = self.invite else {
            return if body.trim().is_empty() {
                Ok(JoinRequest::Task(handle))
            } else {
                Err(self.grammar())
            };
        };
        let message = if body.trim().is_empty() {
            None
        } else {
            Some(carrier.ok_or(
                "an invitation's message requires the exact captured host URI call, not an unproved tool body",
            )?)
        };
        Ok(JoinRequest::Invite {
            task: Some(handle),
            recipient,
            message,
        })
    }

    /// Submit the request a write spells as the lane's actor. An invitation
    /// that admitted its message answers with that submission's receipt
    /// after its own document, as an outbox write answers.
    pub(super) fn submit(
        &self,
        repository: Host<'_>,
        lane: wire::CanonicalUuid,
        body: &str,
        carrier: Option<wire::outbox::Carrier>,
    ) -> Result<String, String> {
        let mut message = None;
        let answer = self.submit_with(body, carrier, |request| {
            let submission = crate::board_command::join_task(repository, request);
            if let wire::Outcome::Reply(reply) = &submission.outcome
                && let wire::intent::CommandAnswer::TaskInvite(invited) = &reply.body
            {
                message = invited.message;
            }
            submission
        })?;
        let (Some(submission), Some(carrier)) = (message, carrier) else {
            return Ok(answer);
        };
        let mut receipt = crate::command_client::execute_outbox(
            repository,
            lane,
            wire::CommandKind::OutboxRead,
            &wire::outbox::Read {
                submission,
                occupant: carrier.occupant,
            },
        );
        // The writer holds the text it just wrote; the receipt names where it
        // went.
        if let wire::Outcome::Reply(reply) = &mut receipt
            && let wire::intent::CommandAnswer::Outbox(receipt) = &mut reply.body
        {
            receipt.text = None;
        }
        let receipt = serde_json::to_string(&receipt)
            .map_err(|error| format!("outbox outcome cannot be represented: {error}"))?;
        Ok(format!("{answer}\nmessage: {receipt}"))
    }

    /// Render the current handle and answer returned by one authoritative
    /// submission together, with no independent task preflight.
    pub(super) fn submit_with(
        &self,
        body: &str,
        carrier: Option<wire::outbox::Carrier>,
        submit: impl FnOnce(JoinRequest) -> crate::board_command::JoinSubmission,
    ) -> Result<String, String> {
        let submission = submit(self.request(body, carrier)?);
        let answer = settled_document(&submission.outcome, command_rendering(&submission.outcome));
        self.noticed(submission.current.as_ref(), answer)
    }

    /// `answer` as the writer reads it: led, as the CLI's stderr leads its
    /// document, by the notice naming the current handle when the URL named
    /// one a topic move retired.
    pub(super) fn noticed(
        &self,
        current: Option<&TaskHandle>,
        answer: Result<String, String>,
    ) -> Result<String, String> {
        let Some(notice) =
            current.and_then(|current| crate::board_command::rename_notice(&self.handle, current))
        else {
            return answer;
        };
        match answer {
            Ok(text) => Ok(format!("{notice}\n{text}")),
            Err(text) => Err(format!("{notice}\n{text}")),
        }
    }
}

fn handle(task: &str, url: &str) -> Result<TaskHandle, String> {
    task.parse::<TaskHandle>()
        .map_err(|error| format!("{url} names no task handle {task}: {error}"))
}

/// The digest a text URL's query names: `digest=DIGEST` alone, or nothing.
fn digest(query: &str, url: &str) -> Result<Option<Digest>, String> {
    if query.is_empty() {
        return Ok(None);
    }
    let Some(written) = query.strip_prefix("digest=") else {
        return Err(format!("{url} takes only ?digest=DIGEST, from its read"));
    };
    decode(written)?
        .parse::<Digest>()
        .map(Some)
        .map_err(|error| format!("{url} names no digest: {error}"))
}
