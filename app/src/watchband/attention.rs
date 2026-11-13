//! What needs the operator in one team right now, listed per member in each
//! lane menu.
//!
//! A pure function of the admitted lane, board, inbox, and consent reads, with
//! no clock and no DOM. Time-dependent items carry the instant from which they
//! need the operator, so the band's own clock decides when they appear and a
//! lease or a waiting direction enters without any new read.

use std::collections::BTreeMap;

use wire::intent::{ConsentPendingAnswer, ConsentQueueAnswer, InboxAnswer};
use wire::{CanonicalUuid, TaskStamp};

use super::Standing;
use crate::work::{WorkAvailability, WorkProjection, WorkTask};

/// A held lease needs the operator once this little of it remains.
pub const LEASE_ATTENTION_MILLIS: i64 = 2 * 60 * 1_000;

/// Direction needs the operator once its oldest directive waited this long.
pub const DIRECTION_ATTENTION_MILLIS: i64 = 5 * 60 * 1_000;

/// Why a task a member holds needs the operator.
///
/// Only states a human must clear count: a task waiting on a sibling
/// dependency needs nobody, so dependency-blocked work is not attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskAttention {
    /// Claimed, but its requirements are not met by this environment, so the
    /// claim is iced until the operator changes that environment.
    Iced,
    /// Unclaimed and blocked on requirements only the operator can meet.
    Blocked,
    /// It waits in review and no claim is working on the verdict.
    Review,
    /// A Design whose proposal awaits the operator's decision, so no lane
    /// can move it.
    Selection,
}

impl TaskAttention {
    /// Stable presentation key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Iced => "iced",
            Self::Blocked => "blocked",
            Self::Review => "review",
            Self::Selection => "selection",
        }
    }

    /// Operator-facing name of the reason.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Iced => "Iced",
            Self::Blocked => "Blocked",
            Self::Review => "Review",
            Self::Selection => "Awaiting decision",
        }
    }
}

/// What one attention item is about, and so which control it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttentionSubject {
    /// A queued consent request, ruled on in place.
    Consent(ConsentPendingAnswer),
    /// A held task, opened in the detail layer.
    Task {
        task: TaskStamp,
        handle: String,
        title: String,
        reason: TaskAttention,
    },
    /// A held claim whose lease is about to run out.
    Lease {
        task: Option<TaskStamp>,
        handle: String,
        expires_at: i64,
    },
    /// Direction waiting on the member; `directive` is the oldest.
    Direction {
        directive: CanonicalUuid,
        pending: usize,
        oldest: i64,
        /// The oldest direction's authored Markdown preview.
        preview: String,
    },
}

/// One thing a team member needs from the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionItem {
    /// Member the item concerns.
    pub lane: CanonicalUuid,
    /// Member name as the roster shows it.
    pub label: String,
    /// Epoch millisecond from which the item needs the operator; absent when
    /// it already does.
    pub from: Option<i64>,
    pub subject: AttentionSubject,
}

impl AttentionItem {
    /// Identity that survives a refresh, so a band updates an item in place.
    #[must_use]
    pub fn key(&self) -> String {
        match &self.subject {
            AttentionSubject::Consent(pending) => format!("consent/{}", pending.request),
            AttentionSubject::Task { task, .. } => format!("task/{task}"),
            AttentionSubject::Lease { handle, .. } => format!("lease/{}/{handle}", self.lane),
            AttentionSubject::Direction { .. } => format!("direction/{}", self.lane),
        }
    }

    /// Whether the item needs the operator at `now`.
    #[must_use]
    pub fn due(&self, now: i64) -> bool {
        self.from.is_none_or(|from| from <= now)
    }
}

/// Every item the team's members may need the operator for, in band order:
/// consent, then held tasks, leases, and direction, each in roster order.
#[must_use]
pub fn team_attention(
    standings: &[Standing],
    work: Option<&WorkProjection<'_>>,
    inbox: Option<&InboxAnswer>,
    consent: Option<&ConsentQueueAnswer>,
) -> Vec<AttentionItem> {
    let members = standings
        .iter()
        .enumerate()
        .map(|(order, standing)| (standing.lane, (order, standing)))
        .collect::<BTreeMap<_, _>>();
    let item = |lane: CanonicalUuid, from: Option<i64>, subject: AttentionSubject| {
        members.get(&lane).map(|(order, standing)| {
            (
                *order,
                AttentionItem {
                    lane,
                    label: standing.label.clone(),
                    from,
                    subject,
                },
            )
        })
    };
    let mut consents = consent
        .into_iter()
        .flat_map(|queue| &queue.pending)
        .filter_map(|pending| {
            item(
                pending.lane,
                None,
                AttentionSubject::Consent(pending.clone()),
            )
        })
        .collect::<Vec<_>>();
    let mut tasks = work
        .into_iter()
        .flat_map(WorkProjection::tasks)
        .filter_map(|task| {
            let (lane, reason) = held_attention(task, |lane| members.contains_key(&lane))?;
            let row = task.row();
            item(
                lane,
                None,
                AttentionSubject::Task {
                    task: row.statement.body.born.clone(),
                    handle: row.statement.body.handle.to_string(),
                    title: row.presentation_title.clone(),
                    reason,
                },
            )
        })
        .collect::<Vec<_>>();
    let mut leases = standings
        .iter()
        .filter_map(|standing| {
            let handle = standing.handle.clone()?;
            let expires_at = standing.expires_at?;
            let task = super::canonical_task_handle(&handle).and_then(|task| task.born().ok());
            item(
                standing.lane,
                Some(expires_at.saturating_sub(LEASE_ATTENTION_MILLIS)),
                AttentionSubject::Lease {
                    task,
                    handle,
                    expires_at,
                },
            )
        })
        .collect::<Vec<_>>();
    let mut directions = inbox
        .into_iter()
        .flat_map(|inbox| &inbox.lanes)
        .filter_map(|row| {
            let oldest = row.pending.first()?;
            let authored = oldest.authored.get().as_millisecond();
            let preview = oldest
                .preview
                .clone()
                .unwrap_or_else(|| oldest.text.clone());
            item(
                row.lane,
                Some(authored.saturating_add(DIRECTION_ATTENTION_MILLIS)),
                AttentionSubject::Direction {
                    directive: oldest.directive,
                    pending: row.pending.len(),
                    oldest: authored,
                    preview,
                },
            )
        })
        .collect::<Vec<_>>();
    for group in [&mut consents, &mut tasks, &mut leases, &mut directions] {
        group.sort_by_key(|(order, _)| *order);
    }
    [consents, tasks, leases, directions]
        .into_iter()
        .flatten()
        .map(|(_, item)| item)
        .collect()
}

/// The items due at `now`, and the next instant another one falls due.
#[must_use]
pub fn attention_at(items: &[AttentionItem], now: i64) -> (Vec<&AttentionItem>, Option<i64>) {
    let due = items.iter().filter(|item| item.due(now)).collect();
    let next = items
        .iter()
        .filter_map(|item| item.from.filter(|from| *from > now))
        .min();
    (due, next)
}

/// The member holding `task` and why it needs the operator, if it does.
///
/// A member holds a task as the lane on its claim, active or last, so
/// finished work waiting in review stays with the member who did it.
fn held_attention(
    task: &WorkTask<'_>,
    member: impl Fn(CanonicalUuid) -> bool,
) -> Option<(CanonicalUuid, TaskAttention)> {
    let row = task.row();
    let reason = match task.availability() {
        _ if task.awaiting_selection() => TaskAttention::Selection,
        WorkAvailability::InFlight if task.environment_blocked() => TaskAttention::Iced,
        WorkAvailability::Held => TaskAttention::Blocked,
        WorkAvailability::Blocked if task.environment_blocked() => TaskAttention::Blocked,
        WorkAvailability::Review => TaskAttention::Review,
        _ => return None,
    };
    let lane = row
        .latest_claim()
        .map(|claim| claim.claim.body.lane)
        .filter(|lane| member(*lane))?;
    Some((lane, reason))
}

#[cfg(test)]
mod tests {
    use wire::head::ClaimDisposition;
    use wire::intent::{ConsentPendingAnswer, ConsentQueueAnswer};
    use wire::{CanonicalUuid, EnvironmentRequirement, EnvironmentRequirements};

    use super::{
        AttentionSubject, LEASE_ATTENTION_MILLIS, TaskAttention, attention_at, team_attention,
    };
    use crate::watchband::Standing;
    use crate::work::WorkProjection;
    use crate::work::tests::{board, claim, retained, task};

    /// The lane `work::tests::claim` binds every claim to.
    const MEMBER: &str = "11111111-1111-4111-8111-111111111111";
    const OUTSIDER: &str = "44444444-4444-4444-8444-444444444444";
    const TEAM: &str = "55555555-5555-4555-8555-555555555555";

    fn uuid(text: &str) -> CanonicalUuid {
        text.parse().expect("fixture uuid")
    }

    fn standing(expires_at: Option<i64>) -> Standing {
        Standing {
            lane: uuid(MEMBER),
            team: uuid(TEAM),
            name: "111111".to_owned(),
            worktree: "/work/member".to_owned(),
            label: "member".to_owned(),
            vendor: "omp".to_owned(),
            model: None,
            effort: None,
            fast_mode: false,
            thread: None,
            session: None,
            life: "docker".to_owned(),
            disposition: "seated".to_owned(),
            liveness: wire::intent::LaneLiveness::Working,
            jobs: Vec::new(),
            pending: 0,
            waking: 0,
            headline: None,
            handle: expires_at.map(|_| "alpha.core-dTaaaaaaa".to_owned()),
            task_title: None,
            phase: None,
            held_since: None,
            expires_at,
            accent: 2,
        }
    }

    fn consent(request: &str, lane: &str) -> ConsentPendingAnswer {
        ConsentPendingAnswer {
            request: uuid(request),
            lane: uuid(lane),
            operation: "git push".to_owned(),
            rule: "publish".parse().expect("fixture rule"),
            cause: wire::body::ConsentQueueCause::Policy {},
            budget: None,
        }
    }

    fn unmet() -> EnvironmentRequirements {
        EnvironmentRequirements::one(
            EnvironmentRequirement::journal_schema(2).expect("fixture requirement"),
        )
    }

    #[test]
    fn only_team_members_consent_and_operator_blocked_work_need_attention() {
        let mut iced = task(1, "/alpha/core");
        iced.statement.body.requirements = unmet();
        iced.claims = vec![retained(claim(&iced, ClaimDisposition::Active))];
        let mut blocked = task(2, "/alpha/core");
        blocked.statement.body.requirements = unmet();
        blocked.claims = vec![retained(claim(&blocked, ClaimDisposition::Freed))];
        let mut waiting_on_sibling = task(3, "/alpha/core");
        waiting_on_sibling.statement.body.dependencies =
            vec![task(9, "/alpha/core").statement.body.born];
        waiting_on_sibling.claims = vec![retained(claim(
            &waiting_on_sibling,
            ClaimDisposition::Freed,
        ))];
        let mut review = task(4, "/alpha/core");
        review.workflow.body.phase = "review".parse().expect("fixture phase");
        review.claims = vec![retained(claim(&review, ClaimDisposition::Freed))];
        let mut reviewing = review.clone();
        reviewing.statement.body.born = task(5, "/alpha/core").statement.body.born;
        reviewing.claims = vec![retained(claim(&reviewing, ClaimDisposition::Active))];
        let mut unheld = task(6, "/alpha/core");
        unheld.statement.body.requirements = unmet();
        let board = board(vec![
            iced,
            blocked,
            waiting_on_sibling,
            review,
            reviewing,
            unheld,
        ]);
        let work = WorkProjection::from_board_and_lanes(&board, None);
        let queue = ConsentQueueAnswer {
            pending: vec![
                consent("018f82d4-6f59-7d06-92e3-bce9131163d1", OUTSIDER),
                consent("018f82d4-6f59-7d06-92e3-bce9131163d2", MEMBER),
            ],
            budgets: Vec::new(),
        };

        let items = team_attention(&[standing(None)], Some(&work), None, Some(&queue));

        let subjects = items
            .iter()
            .map(|item| match &item.subject {
                AttentionSubject::Consent(pending) => format!("consent {}", pending.request),
                AttentionSubject::Task { title, reason, .. } => format!("{title} {reason:?}"),
                other => panic!("unexpected item {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            subjects,
            [
                "consent 018f82d4-6f59-7d06-92e3-bce9131163d2".to_owned(),
                format!("task 1 {:?}", TaskAttention::Iced),
                format!("task 2 {:?}", TaskAttention::Blocked),
                format!("task 4 {:?}", TaskAttention::Review),
            ]
        );
        assert!(
            items
                .iter()
                .all(|item| item.lane == uuid(MEMBER) && item.from.is_none())
        );
    }

    #[test]
    fn a_lease_enters_the_band_only_near_its_expiry() {
        let expiry = 1_000_000;
        let items = team_attention(&[standing(Some(expiry))], None, None, None);
        let lead = expiry - LEASE_ATTENTION_MILLIS;

        let (due, next) = attention_at(&items, lead - 1);
        assert!(due.is_empty());
        assert_eq!(next, Some(lead), "the band wakes when the lease falls due");
        let (due, next) = attention_at(&items, lead);
        assert!(matches!(
            due.as_slice(),
            [item] if matches!(item.subject, AttentionSubject::Lease { expires_at, .. } if expires_at == expiry)
        ));
        assert_eq!(next, None);
    }
}
