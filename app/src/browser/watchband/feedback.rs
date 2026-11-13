//! Progress and outcomes of lane and team requests, reported through the root
//! toast.
//!
//! Member commands, team policy refusals, and consent rulings report here
//! rather than inside the card or control that issued them, so no lane menu or
//! team surface resizes while a request is in flight. A team policy send that
//! lands reports nothing: its control already holds the new stop. Each notice
//! names the member or team it concerns, the toast's live region announces it,
//! and a request's outcome replaces its progress. A request that disappears
//! while still in flight reports how it ended, so no progress message outlives
//! its request. A vendor refusing a member's fast mode reports here too, once
//! as it lands, since that member keeps working without it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use yew::prelude::*;

use crate::runtime::{ConsentRuling, MemberControlState};

use super::*;

/// The request a notice reports on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Subject {
    Member(CanonicalUuid),
    Team(CanonicalUuid),
    Consent(CanonicalUuid),
    FastRefusal(CanonicalUuid),
}

/// Who a notice is attributed to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Member(CanonicalUuid),
    Team(CanonicalUuid),
}

#[derive(Clone, PartialEq, Eq)]
struct Notice {
    owner: Owner,
    text: String,
    /// Distinguishes two identical outcomes of successive requests.
    generation: u64,
    /// What a request still in flight reports if it disappears unsettled.
    ended: Option<String>,
}

/// Every request's current notice, and the member and team labels they name.
#[derive(Clone, Default, PartialEq)]
pub(super) struct Notices {
    notices: BTreeMap<Subject, Notice>,
    labels: BTreeMap<Owner, String>,
}

impl Notices {
    pub(super) fn of(runtime: Option<&AppRuntime>, controls: &TeamControls) -> Self {
        let mut notices = Self::default();
        if let Some(runtime) = runtime {
            notices.read_runtime(runtime);
        }
        for (team, issue) in controls.refusals() {
            notices.notices.insert(
                Subject::Team(team),
                Notice {
                    owner: Owner::Team(team),
                    text: issue.to_owned(),
                    generation: 0,
                    ended: None,
                },
            );
        }
        notices
    }

    fn read_runtime(&mut self, runtime: &AppRuntime) {
        for team in runtime.lanes().into_iter().flat_map(|lanes| &lanes.lanes) {
            self.labels
                .insert(Owner::Team(team.lane), watchband::team_label(team));
            for member in &team.members {
                self.labels.insert(
                    Owner::Member(member.lane),
                    watchband::agent_message_label(&member.worktree, member.name.as_deref())
                        .to_owned(),
                );
                if let Some(refusal) = &member.fast_refusal {
                    self.notices.insert(
                        Subject::FastRefusal(member.lane),
                        Notice {
                            owner: Owner::Member(member.lane),
                            text: format!(
                                "the provider refused fast mode, so this lane continues without it: {refusal}"
                            ),
                            generation: 0,
                            ended: None,
                        },
                    );
                }
            }
        }
        for (lane, state) in runtime.member_controls() {
            if let Some(notice) = member_notice(lane, state) {
                self.notices.insert(Subject::Member(lane), notice);
            }
        }
        for pending in runtime
            .consent_queue()
            .into_iter()
            .flat_map(|queue| &queue.pending)
        {
            if let Some(ruling) = runtime.consent_ruling(pending.request) {
                self.notices.insert(
                    Subject::Consent(pending.request),
                    consent_notice(pending, ruling),
                );
            }
        }
    }
}

fn member_notice(lane: CanonicalUuid, state: &MemberControlState) -> Option<Notice> {
    let owner = Owner::Member(lane);
    if let Some(request) = state.pending() {
        return Some(Notice {
            owner,
            text: request.command.progress().to_owned(),
            generation: request.serial,
            // The runtime settles every reservation it drops.
            ended: None,
        });
    }
    let settled = state.settled()?;
    Some(Notice {
        owner,
        text: settled
            .issue
            .clone()
            .unwrap_or_else(|| settled.request.command.done().to_owned()),
        generation: settled.request.serial,
        ended: None,
    })
}

fn consent_notice(pending: &wire::intent::ConsentPendingAnswer, ruling: &ConsentRuling) -> Notice {
    let operation = &pending.operation;
    let (text, ended) = match ruling {
        ConsentRuling::Sending(answer) => (
            format!("{} {operation}…", answer_verbs(*answer).0),
            Some(format!(
                "The decision on {operation} ended without an answer"
            )),
        ),
        ConsentRuling::Ruled(answer) => (format!("{} {operation}", answer_verbs(*answer).1), None),
        ConsentRuling::Refused(reason) => (format!("Could not decide {operation}: {reason}"), None),
    };
    Notice {
        owner: Owner::Member(pending.lane),
        text,
        generation: 0,
        ended,
    }
}

/// The in-flight and settled verbs for one ruling.
const fn answer_verbs(answer: wire::Answer) -> (&'static str, &'static str) {
    match answer {
        wire::Answer::Granted => ("Allowing", "Allowed"),
        wire::Answer::Denied => ("Denying", "Denied"),
        wire::Answer::Asked | wire::Answer::Queued => ("Answering", "Answered"),
    }
}

/// The notices last reported, and the label each member and team last carried
/// so a removed owner's outcome still names it.
#[derive(Default)]
struct Reported {
    notices: BTreeMap<Subject, Notice>,
    labels: BTreeMap<Owner, String>,
}

impl Reported {
    /// The toasts one change calls for, in order: progress first, so an
    /// outcome that lands in the same change stays visible.
    fn advance(&mut self, current: &Notices) -> Vec<(String, bool)> {
        self.labels.extend(
            current
                .labels
                .iter()
                .map(|(owner, label)| (*owner, label.clone())),
        );
        let mut toasts = Vec::new();
        for (subject, notice) in &current.notices {
            if self.notices.get(subject) != Some(notice) {
                toasts.push((
                    self.attribute(notice.owner, &notice.text),
                    notice.ended.is_some(),
                ));
            }
        }
        for (subject, notice) in &self.notices {
            if let Some(ended) = &notice.ended
                && !current.notices.contains_key(subject)
            {
                toasts.push((self.attribute(notice.owner, ended), false));
            }
        }
        toasts.sort_by_key(|(_, pending)| !pending);
        self.notices.clone_from(&current.notices);
        toasts
    }

    fn attribute(&self, owner: Owner, text: &str) -> String {
        let label = self.labels.get(&owner).cloned();
        match owner {
            Owner::Member(lane) => {
                format!(
                    "{}: {text}",
                    label.unwrap_or_else(|| watchband::short(lane))
                )
            }
            Owner::Team(team) => format!(
                "Team {}: {text}",
                label.unwrap_or_else(|| watchband::short(team))
            ),
        }
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct RequestFeedbackProps {
    pub(super) notices: Notices,
}

/// Toast each change to a request's notice. The first render only
/// records what is already there, so remounting never replays old outcomes.
#[function_component(RequestFeedback)]
pub(super) fn request_feedback(props: &RequestFeedbackProps) -> Html {
    let toaster = crate::browser::toast::use_toaster();
    let reported = use_mut_ref(|| None::<Reported>);
    use_effect_with(props.notices.clone(), move |notices| {
        report(&reported, &toaster, notices);
    });
    Html::default()
}

fn report(
    reported: &Rc<RefCell<Option<Reported>>>,
    toaster: &crate::browser::toast::Toaster,
    notices: &Notices,
) {
    let mut slot = reported.borrow_mut();
    let Some(reported) = slot.as_mut() else {
        let mut first = Reported::default();
        let _replayed = first.advance(notices);
        *slot = Some(first);
        return;
    };
    for (message, pending) in reported.advance(notices) {
        if pending {
            toaster.pending(message);
        } else {
            toaster.settled(message);
        }
    }
}
