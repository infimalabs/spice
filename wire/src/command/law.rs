use super::*;
/// The capability a connection must carry to submit one command.
///
/// identity-law: non-crossing - registry metadata held in static process memory.
///
/// This is law rather than witness. A witness proves a fact about the board
/// and travels in the command's `expected` list; authority proves who is
/// asking, and the resident derives it from the authenticated capability that
/// opened the session. Nothing here is serializable for that reason: a client
/// that could author its own authority would be answering the question the
/// capability exists to answer.
///
/// A client cannot encode an authority into a request:
///
/// ```compile_fail
/// let _ = wire::encode(&wire::Authority::Connected);
/// ```
///
/// Nor can bytes crossing the socket manufacture one:
///
/// ```compile_fail
/// let _ = wire::decode::<wire::Authority>(b"null");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Authority {
    /// Any authenticated connection may submit, operator or lane alike.
    Connected,
    /// Only an operator connection may submit.
    Operator,
}

/// The law one command carries.
///
/// identity-law: non-crossing - registry metadata held in static process memory.
///
/// `required` must all appear in a command's `expected` list. `optional`
/// names predicates the row admits conditionally — an assignment witness on
/// the review path, a budget predicate when a rule spends one. Anything
/// outside the union is refused, because a command witnesses only the
/// subjects it relies on.
///
/// `authority` states the same row's capability requirement. It sits here
/// rather than beside a reader because a row that named its witnesses and left
/// its authority to prose would be restated by everyone who consumed it, and
/// restatements drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Law {
    /// Predicates every instance of this command must prove.
    pub required: &'static [WitnessKind],
    /// Predicates this command admits but does not always require.
    pub optional: &'static [WitnessKind],
    /// The capability a connection must carry to submit this command.
    pub authority: Authority,
}

impl Law {
    /// Authenticated read with no caller-proposed authority predicate.
    pub const READ: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Operator-only authenticated read.
    pub const OPERATOR_READ: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Snapshot-only operator inspection carries no mutable predicate.
    pub const INSPECT: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Operator-authored global settings need no caller-proposed predicate.
    pub const SETTINGS: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Operator-authored principal birth, profile, and role changes. The
    /// resident guards every principal head it read, so the operator proposes
    /// no board predicate.
    pub const PRINCIPAL: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// True when this law admits `kind` at all.
    #[must_use]
    pub fn admits(&self, kind: WitnessKind) -> bool {
        self.required.contains(&kind) || self.optional.contains(&kind)
    }
}

/// The rows of the command registry, one constant apiece.
///
/// Each row is named for the move it governs rather than for a single
/// command, because several commands share one law: possession changes hands
/// under the same predicates whether it is carried, renewed, freed, or
/// lapsed. Every doc comment restates the row it implements.
impl Law {
    /// Operator termination of an immutable lane-scoped keeper job. The
    /// command envelope binds the resident epoch; no board head is mutated.
    pub const JOB_STOP: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Exact current room-anchor statement. Any connected principal may publish
    /// an authored observation.
    pub const ROOM_POST: Self = Self {
        required: &[WitnessKind::Task],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Authenticated topic descriptions and topology changes. The resident
    /// guards the complete topic revision, while topology changes tighten to
    /// operator authority after body admission.
    pub const TOPIC_EDIT: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Connected,
    };
    /// A lane leaves one room. Any connection may submit; the resident admits
    /// only a lane principal and guards nothing a caller could propose.
    pub const ROOM_LEAVE: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Connected,
    };
    /// A lane contributes to one open Design room. It binds the Design's exact
    /// statement and workflow heads plus every exact source it cites; any
    /// connection may submit, and the resident admits only a lane principal.
    pub const ROOM_CONTRIBUTE: Self = Self {
        required: &[WitnessKind::Task, WitnessKind::Workflow],
        optional: &[WitnessKind::Source],
        authority: Authority::Connected,
    };
    /// Opportunistic OOPS intake names an absent report and the exact
    /// repository generation; a current generation additionally carries its
    /// exact task and workflow heads.
    pub const OOPS_ADD: Self = Self {
        required: &[WitnessKind::Absence, WitnessKind::OopsGeneration],
        optional: &[WitnessKind::Task, WitnessKind::Workflow],
        authority: Authority::Connected,
    };
    /// A debt sweep files its previewed texts against the board barrier the
    /// preview read, naming every intended absent birth.
    pub const SWEEP: Self = Self {
        required: &[
            WitnessKind::Preview,
            WitnessKind::Barrier,
            WitnessKind::Absence,
        ],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Direct task creation may also guard the statement, workflow, and claim
    /// of an ambient planning task that it atomically links to the new child.
    /// Atomic create-and-claim retains the ordinary eligibility and residency
    /// predicates.
    pub const TASK_ADD: Self = Self {
        required: &[WitnessKind::Absence],
        optional: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Eligibility,
            WitnessKind::Residency,
        ],
        authority: Authority::Connected,
    };
    /// Exact task statement head. Route edits additionally bind the workflow,
    /// completion head and, when authority comes from possession, an exact
    /// active claim. Requirement edits retain their claim and lease predicates.
    pub const EDIT: Self = Self {
        required: &[WitnessKind::Task],
        optional: &[
            WitnessKind::Workflow,
            WitnessKind::Completion,
            WitnessKind::Claim,
            WitnessKind::Lease,
        ],
        authority: Authority::Connected,
    };
    /// Placement previews and applies bind the task and phase they route.
    /// The resident additionally guards the captured routing inputs.
    pub const PLACE: Self = Self {
        required: &[WitnessKind::Task, WitnessKind::Workflow],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact task statement and workflow heads plus the current claim
    /// disposition.
    ///
    /// The claim predicate is what makes the refusal honest rather than
    /// advisory. Withdrawal decides against possession, so reading the claim
    /// from a board snapshot and deciding afterwards would let a claim landing
    /// in between take the decision's basis away; naming the head the decision
    /// was made against moves that race into the admission door.
    pub const DELETE: Self = Self {
        required: &[WitnessKind::Task, WitnessKind::Workflow, WitnessKind::Claim],
        optional: &[],
        authority: Authority::Connected,
    };
    /// The withdrawal witnesses, because closing a task or concluding a Design
    /// likewise decides against the current workflow and possession; only the
    /// operator settles work this way.
    pub const CLOSE: Self = Self {
        required: &[WitnessKind::Task, WitnessKind::Workflow, WitnessKind::Claim],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Task identity; no statement revision witness, because notes append
    /// rather than replace.
    pub const NOTE: Self = Self {
        required: &[WitnessKind::Existence],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Task identity only. Draft images live in the artifact store rather than
    /// the journal, so the command moves no head a caller could witness; the
    /// resident refuses any task that is not an unfiled pending Draft.
    pub const MEDIA: Self = Self {
        required: &[WitnessKind::Existence],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact workflow and defer disposition. A holder deferring its own task
    /// additionally witnesses the claim, lease, and occupied residency it releases.
    pub const DEFER: Self = Self {
        required: &[WitnessKind::Workflow, WitnessKind::Deferral],
        optional: &[
            WitnessKind::Claim,
            WitnessKind::Lease,
            WitnessKind::Residency,
        ],
        authority: Authority::Connected,
    };
    /// Exact workflow and defer disposition for ordinary deferred work.
    /// Atomic claim additionally carries capture eligibility and residency.
    pub const WAKE: Self = Self {
        required: &[WitnessKind::Workflow, WitnessKind::Deferral],
        optional: &[
            WitnessKind::Claim,
            WitnessKind::Eligibility,
            WitnessKind::Residency,
        ],
        authority: Authority::Connected,
    };
    /// Phase-zero advance at current workflow, deferral and statement heads.
    /// Atomic claim additionally carries capture eligibility and residency.
    pub const FILE: Self = Self {
        required: &[WitnessKind::Workflow, WitnessKind::Deferral],
        optional: &[
            WitnessKind::Task,
            WitnessKind::Claim,
            WitnessKind::Eligibility,
            WitnessKind::Residency,
        ],
        authority: Authority::Connected,
    };
    /// Exact task statement, workflow, and claim heads plus each new edge.
    /// Active claimant edits additionally carry their lease, residency, and
    /// review assignment where applicable.
    pub const DEPENDS: Self = Self {
        required: &[WitnessKind::Task, WitnessKind::Workflow, WitnessKind::Claim],
        optional: &[
            WitnessKind::Edge,
            WitnessKind::Lease,
            WitnessKind::Residency,
            WitnessKind::Assignment,
        ],
        authority: Authority::Connected,
    };
    /// Explicit target edits are operator authority and witness only the
    /// workflow head. Invitation admission owns its separate atomic growth.
    pub const TARGET: Self = Self {
        required: &[WitnessKind::Workflow],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Exact task, workflow and claim heads, ordinary join eligibility and
    /// current lane residency. Invitation acceptance additionally witnesses
    /// the live steering directive it retires.
    pub const JOIN: Self = Self {
        required: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Eligibility,
            WitnessKind::Residency,
        ],
        optional: &[WitnessKind::Directive],
        authority: Authority::Connected,
    };
    /// Exact task, workflow and inviter claim heads plus the inviter's
    /// current residency. The recipient is chosen against the resident's
    /// whole snapshot, so no recipient predicate travels with the request.
    pub const INVITE: Self = Self {
        required: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Residency,
        ],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact task statement and workflow heads, winning claim head, live
    /// lease, assignment or review witness where required, bound lane and
    /// occupant, and resident epoch.
    pub const PROGRESS: Self = Self {
        required: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Lease,
            WitnessKind::Residency,
        ],
        optional: &[WitnessKind::Assignment],
        authority: Authority::Connected,
    };
    /// The review path is the one that always carries the assignment, and the
    /// only one that may also carry follow-up tracking: the absence of each
    /// stamp it mints and of each edge it wires, exactly the predicates
    /// [`Self::ADD`] and [`Self::DEPENDS`] require of those moves alone.
    pub const REVIEW: Self = Self {
        required: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Lease,
            WitnessKind::Residency,
            WitnessKind::Assignment,
        ],
        optional: &[WitnessKind::Absence, WitnessKind::Edge],
        authority: Authority::Connected,
    };
    /// Exact task statement and workflow heads, current claim head,
    /// eligibility rule, lane occupancy, and resident epoch.
    pub const WIN: Self = Self {
        required: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Eligibility,
            WitnessKind::Residency,
        ],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact statement, workflow, claim, and lease heads plus current lane
    /// occupancy and resident epoch.
    pub const REFRESH: Self = Self {
        required: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Lease,
            WitnessKind::Residency,
        ],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact claim head, lease head, bound lane and occupant, resident epoch,
    /// and optionally the workflow head when the release also unassigns
    /// review. A free and a lapse release the same way, so both carry it.
    pub const FREE: Self = Self {
        required: &[
            WitnessKind::Claim,
            WitnessKind::Lease,
            WitnessKind::Residency,
        ],
        optional: &[WitnessKind::Workflow],
        authority: Authority::Connected,
    };
    /// Exact operator disposition of one completed Design pass. Every decision
    /// witnesses its absent decision head, the Design's statement and room
    /// heads, and each offered output's statement and workflow heads;
    /// selection additionally witnesses claim absence and prior authorization
    /// absence.
    pub const PREPARATION_DECIDE: Self = Self {
        required: &[WitnessKind::Proposal],
        optional: &[
            WitnessKind::Task,
            WitnessKind::Workflow,
            WitnessKind::Claim,
            WitnessKind::Room,
            WitnessKind::Authorization,
        ],
        authority: Authority::Operator,
    };
    /// Pending, unclaimed output and its exact preparation task.
    pub const ADOPT: Self = Self {
        required: &[WitnessKind::Task, WitnessKind::Workflow, WitnessKind::Claim],
        optional: &[],
        authority: Authority::Operator,
    };

    /// An explicitly addressed directive names the lane it lands on as it
    /// found it, occupied or vacant. Automatic selection carries no client
    /// residency predicate; the resident binds the selected lane atomically.
    pub const SEND: Self = Self {
        required: &[],
        optional: &[WitnessKind::Residency],
        authority: Authority::Operator,
    };
    /// An authenticated principal settles only an input addressed to itself;
    /// the resident derives and guards that exact input authority.
    pub const LINE_CONTROL: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Connected,
    };

    /// Exact live directive disposition for a time-derived re-presentation.
    pub const RESEND: Self = Self {
        required: &[WitnessKind::Directive],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Exact directive identity in either recoverable failure state.
    pub const RECOVER: Self = Self {
        required: &[WitnessKind::Recoverable],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Exact live directive, canceled by the operator or by the sender of a
    /// typed task invitation. The transition checks sender ownership.
    pub const CANCEL: Self = Self {
        required: &[WitnessKind::Directive],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact request disposition, named rule, and budget predicate when a rule
    /// spends or renews a budget, decided as the operator.
    ///
    /// This row states the predicates without naming a capability. Consent is
    /// the operator's answer to a request raised on their behalf, and a lane
    /// deciding its own request would be granting itself the consent it was
    /// asked to wait for.
    pub const DECIDE: Self = Self {
        required: &[WitnessKind::Consent, WitnessKind::Rule],
        optional: &[WitnessKind::Budget],
        authority: Authority::Operator,
    };
    /// Exact executable digest and its current trust state.
    ///
    /// Pending quarantines a digest, which is a report that a surface changed
    /// rather than a judgment about it.
    pub const PEND: Self = Self {
        required: &[WitnessKind::Trust],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact executable digest and its current trust state, judged as the
    /// operator. CLI rulings carry the current trust head. Native app rulings
    /// additionally bind the task statement that owns the blocker; an
    /// uncaptured digest has no trust head yet.
    pub const TRUST: Self = Self {
        required: &[],
        optional: &[WitnessKind::Trust, WitnessKind::Task],
        authority: Authority::Operator,
    };
    /// Operator-authored lane lifecycle intent. The command envelope binds the
    /// current resident epoch; the handler compares the named lane with the
    /// current residency head before changing it. A residency witness cannot
    /// represent either a lane that does not exist yet or a vacant lane.
    pub const LANE: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Read the lane table as the operator without authoring a residency
    /// witness for the state being observed.
    pub const LANE_READ: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Operator,
    };
    /// Read one bounded continuity account as either the operator or the lane
    /// capability whose view the resident filters.
    pub const LANE_RECALL: Self = Self {
        required: &[],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Exact observation key and the current head that makes recognition
    /// idempotent.
    pub const RECOGNIZE: Self = Self {
        required: &[WitnessKind::Observation],
        optional: &[],
        authority: Authority::Connected,
    };
    /// Current lifecycle edge. The command envelope separately binds the
    /// resident epoch, so an operator lifecycle request does not fabricate a
    /// lane-and-occupant residency witness.
    pub const UP: Self = Self {
        required: &[WitnessKind::Transition],
        optional: &[],
        authority: Authority::Operator,
    };
}

/// A command whose `expected` list does not satisfy its registry row.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LawError {
    /// A required predicate is absent from `expected`.
    #[error("{command} requires a {missing:?} witness")]
    Missing {
        /// Command whose law was violated.
        command: CommandKind,
        /// Predicate class the row requires.
        missing: WitnessKind,
    },
    /// `expected` carries a predicate the row does not admit.
    #[error("{command} does not witness {extra:?}")]
    Unadmitted {
        /// Command whose law was violated.
        command: CommandKind,
        /// Predicate class the row does not name.
        extra: WitnessKind,
    },
    /// A required predicate names another subject than the command body.
    #[error("{command} requires a {kind:?} witness bound to {target}")]
    SubjectMismatch {
        /// Command whose law was violated.
        command: CommandKind,
        /// Predicate class whose subject does not match the body.
        kind: WitnessKind,
        /// Body subject the predicate must name.
        target: String,
    },
}
impl CommandKind {
    /// The witness law this command carries.
    ///
    /// This is the registry. The match is exhaustive, so a command added
    /// without an explicit row fails to compile — the law cannot be forgotten
    /// or defaulted, only stated. Each row it names states its predicates in
    /// full rather than deriving them from a neighbour, so the law governing
    /// one command is read in one place.
    #[must_use]
    pub const fn law(self) -> Law {
        match self {
            Self::OutboxSubmit | Self::OutboxRead => Law::READ,
            Self::RoomScore
            | Self::BoardRead
            | Self::TaskDetail
            | Self::TaskRelations
            | Self::TaskTimeline
            | Self::BoardGraph
            | Self::DocLedger
            | Self::RoomContributionRead
            | Self::TrustShow => Law::READ,
            Self::RackRead
            | Self::GlobalSettingsRead
            | Self::VendorCatalog
            | Self::BoardHandout
            | Self::RoomConversationText => Law::OPERATOR_READ,
            Self::GlobalSettingsSet => Law::SETTINGS,
            Self::RoomPost => Law::ROOM_POST,
            Self::RoomContribute => Law::ROOM_CONTRIBUTE,
            Self::PreparationDecide => Law::PREPARATION_DECIDE,
            Self::OutputAdopt => Law::ADOPT,
            _ => self.task_law(),
        }
    }

    const fn task_law(self) -> Law {
        match self {
            Self::TaskAdd => Law::TASK_ADD,
            Self::TaskOopsAdd => Law::OOPS_ADD,
            Self::TaskSweep => Law::SWEEP,
            Self::TaskEdit => Law::EDIT,
            Self::TaskDelete => Law::DELETE,
            Self::TaskClose | Self::TaskConclude => Law::CLOSE,
            Self::TaskNote => Law::NOTE,
            Self::TaskDefer => Law::DEFER,
            Self::TaskWake => Law::WAKE,
            Self::TaskFile => Law::FILE,
            Self::TaskPlace => Law::PLACE,
            Self::TaskMedia => Law::MEDIA,
            Self::TaskDepends => Law::DEPENDS,
            Self::TaskDone => Law::PROGRESS,
            Self::TaskTarget => Law::TARGET,
            Self::TaskJoin => Law::JOIN,
            Self::TaskInvite => Law::INVITE,
            Self::TaskReview => Law::REVIEW,
            Self::ClaimWin => Law::WIN,
            Self::ClaimFree | Self::ClaimLapse => Law::FREE,
            Self::ClaimRefresh => Law::REFRESH,
            _ => self.non_task_law(),
        }
    }

    pub(super) fn authority_for_intent(self, body: &Intent) -> Authority {
        if matches!(
            (self, body),
            (Self::TopicEdit, Intent::TopicEdit(edit))
                if matches!(&edit.change, crate::topic::TopicChange::Move { .. })
        ) {
            Authority::Operator
        } else {
            self.law().authority
        }
    }

    pub(super) fn required_for_intent(self, body: &Intent) -> &'static [WitnessKind] {
        if matches!(
            (self, body),
            (Self::TaskAdd, Intent::TaskAdd(add)) if add.claim
        ) {
            return &[
                WitnessKind::Absence,
                WitnessKind::Eligibility,
                WitnessKind::Residency,
            ];
        }
        if let (Self::SteerSend, Intent::SteerSend(send)) = (self, body) {
            return if send.lane.is_some() {
                &[WitnessKind::Residency]
            } else {
                &[]
            };
        }
        if let (Self::TaskWake, Intent::TaskWake(wake)) = (self, body) {
            return if wake.claim {
                &[
                    WitnessKind::Workflow,
                    WitnessKind::Deferral,
                    WitnessKind::Eligibility,
                    WitnessKind::Residency,
                ]
            } else {
                Law::WAKE.required
            };
        }
        if let (Self::TaskFile, Intent::TaskFile(file)) = (self, body) {
            return if file.claim {
                &[
                    WitnessKind::Workflow,
                    WitnessKind::Deferral,
                    WitnessKind::Task,
                    WitnessKind::Eligibility,
                    WitnessKind::Residency,
                ]
            } else {
                &[
                    WitnessKind::Workflow,
                    WitnessKind::Deferral,
                    WitnessKind::Task,
                ]
            };
        }
        if matches!(
            (self, body),
            (
                Self::TaskJoin,
                Intent::TaskJoin(crate::intent::TaskJoin::Invitation { .. })
            )
        ) {
            return &[
                WitnessKind::Task,
                WitnessKind::Workflow,
                WitnessKind::Claim,
                WitnessKind::Eligibility,
                WitnessKind::Residency,
                WitnessKind::Directive,
            ];
        }
        if let (Self::TaskEdit, Intent::TaskEdit(edit)) = (self, body) {
            return match (edit.requirements.is_some(), edit.flow.is_some()) {
                (true, true) => &[
                    WitnessKind::Task,
                    WitnessKind::Workflow,
                    WitnessKind::Completion,
                    WitnessKind::Claim,
                ],
                (false, true) => &[
                    WitnessKind::Task,
                    WitnessKind::Workflow,
                    WitnessKind::Completion,
                ],
                (true, false) => &[WitnessKind::Task, WitnessKind::Claim],
                (false, false) => self.law().required,
            };
        }
        self.law().required
    }

    pub(super) fn witnessed_intent(
        self,
        expected: &[Witness],
        body: &Intent,
    ) -> Result<(), LawError> {
        for required in self.required_for_intent(body) {
            if !expected.iter().any(|witness| witness.kind() == *required) {
                return Err(LawError::Missing {
                    command: self,
                    missing: *required,
                });
            }
        }
        if let (Self::TaskFile, Intent::TaskFile(file)) = (self, body) {
            let target = file.task.born().ok();
            let target_is_witnessed = expected.iter().any(|witness| {
                matches!(
                    (witness, target.as_ref()),
                    (Witness::Task { task, .. }, Some(target)) if task == target
                )
            });
            if !target_is_witnessed {
                return Err(LawError::SubjectMismatch {
                    command: self,
                    kind: WitnessKind::Task,
                    target: file.task.to_string(),
                });
            }
        }
        self.witnessed_trust(expected)?;
        self.witnessed_room(expected, body)?;
        if let (Self::TaskEdit, Intent::TaskEdit(edit)) = (self, body)
            && edit.flow.is_some()
        {
            let target = edit.task.born().ok();
            for kind in [
                WitnessKind::Task,
                WitnessKind::Workflow,
                WitnessKind::Completion,
            ] {
                let target_is_witnessed = expected.iter().any(|witness| {
                    matches!(
                        (kind, witness, target.as_ref()),
                        (WitnessKind::Task, Witness::Task { task, .. }, Some(target))
                            | (
                                WitnessKind::Workflow,
                                Witness::Workflow { task, .. },
                                Some(target)
                            )
                            | (
                                WitnessKind::Completion,
                                Witness::Completion { task, .. },
                                Some(target)
                            )
                            if task == target
                    )
                });
                if !target_is_witnessed {
                    return Err(LawError::SubjectMismatch {
                        command: self,
                        kind,
                        target: edit.task.to_string(),
                    });
                }
            }
        }
        // Requirement and intent edits are guarded authority changes, so the
        // statement witness they carry must name the edited task itself.
        if let (Self::TaskEdit, Intent::TaskEdit(edit)) = (self, body)
            && (edit.requirements.is_some() || edit.intent.is_some())
        {
            let target = edit.task.born().ok();
            let kinds: &[WitnessKind] = if edit.requirements.is_some() {
                &[WitnessKind::Task, WitnessKind::Claim]
            } else {
                &[WitnessKind::Task]
            };
            for &kind in kinds {
                let target_is_witnessed = expected.iter().any(|witness| {
                    matches!(
                        (kind, witness, target.as_ref()),
                        (WitnessKind::Task, Witness::Task { task, .. }, Some(target))
                            | (WitnessKind::Claim, Witness::Claim { task, .. }, Some(target))
                            if task == target
                    )
                });
                if !target_is_witnessed {
                    return Err(LawError::SubjectMismatch {
                        command: self,
                        kind,
                        target: edit.task.to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    fn witnessed_trust(self, expected: &[Witness]) -> Result<(), LawError> {
        if !matches!(self, Self::TrustGrant | Self::TrustRevoke) {
            return Ok(());
        }
        let task = expected
            .iter()
            .any(|witness| matches!(witness, Witness::Task { .. }));
        let trust = expected
            .iter()
            .any(|witness| matches!(witness, Witness::Trust { .. }));
        if task || trust {
            return Ok(());
        }
        Err(LawError::Missing {
            command: self,
            missing: WitnessKind::Trust,
        })
    }

    fn witnessed_room(self, expected: &[Witness], body: &Intent) -> Result<(), LawError> {
        let room = match (self, body) {
            (Self::RoomPost, Intent::RoomPost(post)) => Some(&post.room),
            (Self::RoomContribute, Intent::RoomContribute(request)) => Some(&request.room),
            _ => None,
        };
        let Some(room) = room else {
            return Ok(());
        };
        let target = room.born().ok();
        let kinds: &[WitnessKind] = match self {
            Self::RoomPost => &[WitnessKind::Task],
            Self::RoomContribute => &[WitnessKind::Task, WitnessKind::Workflow],
            _ => &[WitnessKind::Task, WitnessKind::Workflow, WitnessKind::Claim],
        };
        for &kind in kinds {
            let matched = expected.iter().any(|witness| {
                matches!(
                    (kind, witness, target.as_ref()),
                    (WitnessKind::Task, Witness::Task { task, .. }, Some(target))
                        | (WitnessKind::Workflow, Witness::Workflow { task, .. }, Some(target))
                        | (WitnessKind::Claim, Witness::Claim { task, .. }, Some(target))
                        if task == target
                )
            });
            if !matched {
                return Err(LawError::SubjectMismatch {
                    command: self,
                    kind,
                    target: room.to_string(),
                });
            }
        }
        Ok(())
    }

    const fn non_task_law(self) -> Law {
        match self {
            Self::TopicRead => Law::OPERATOR_READ,
            Self::TopicEdit => Law::TOPIC_EDIT,
            Self::RoomPost => Law::ROOM_POST,
            Self::RoomConversationText => Law::OPERATOR_READ,
            Self::RoomLeave => Law::ROOM_LEAVE,
            Self::SteerSend => Law::SEND,
            Self::RoomScore | Self::SteerInbox | Self::QuoteRead | Self::RoomContributionRead => {
                Law::READ
            }
            Self::SteerResend => Law::RESEND,
            Self::SteerRecover => Law::RECOVER,
            Self::SteerCancel => Law::CANCEL,
            Self::SteerLineControl => Law::LINE_CONTROL,
            Self::LaneRead => Law::LANE_READ,
            Self::LaneJobsRead | Self::LaneJobRead => Law::OPERATOR_READ,
            Self::LaneJobStop => Law::JOB_STOP,
            Self::LaneSeat
            | Self::LaneState
            | Self::LaneRenewal
            | Self::LaneRetune
            | Self::LaneEffortRetune
            | Self::LaneFastRetune
            | Self::TeamConfigure
            | Self::TeamArrange => Law::LANE,
            Self::LaneRecall => Law::LANE_RECALL,
            Self::ProofRecognize => Law::RECOGNIZE,
            Self::UpChange => Law::UP,
            _ => self.authority_law(),
        }
    }

    const fn authority_law(self) -> Law {
        match self {
            Self::ConsentRead | Self::ConsentPolicy => Law::INSPECT,
            Self::ConsentDecide => Law::DECIDE,
            Self::TrustPend => Law::PEND,
            Self::TrustGrant | Self::TrustRevoke => Law::TRUST,
            Self::PrincipalDirectoryRead => Law::READ,
            Self::PrincipalRead => Law::OPERATOR_READ,
            Self::PrincipalAdmit
            | Self::PrincipalEdit
            | Self::PrincipalGrant
            | Self::PrincipalRevoke
            | Self::PrincipalDeauthorize => Law::PRINCIPAL,
            _ => panic!("command law partition is closed"),
        }
    }
}
