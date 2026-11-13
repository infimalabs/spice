use wire::Media;
use wire::intent::{GlobalSettingsSet, TeamArrangement};
use wire::{CanonicalUuid, TaskHandle, TaskStamp};

pub(crate) type JobResponse = std::rc::Rc<dyn Fn(Result<wire::intent::LaneJobAnswer, String>)>;
pub(crate) type JobsResponse = std::rc::Rc<dyn Fn(Result<wire::intent::LaneJobsAnswer, String>)>;
pub(crate) type RackResponse = std::rc::Rc<dyn Fn(Result<wire::intent::RackAnswer, String>)>;
pub(crate) type ArrangementResponse = std::rc::Rc<dyn Fn(Result<(), String>)>;
pub(crate) type RoomPostResponse = std::rc::Rc<dyn Fn(Result<wire::rooms::RoomPostAnswer, String>)>;
pub(crate) type SteerLineResponse =
    std::rc::Rc<dyn Fn(Result<wire::intent::SteerLineAnswer, String>)>;
pub(crate) type DirectiveResponse =
    std::rc::Rc<dyn Fn(Result<wire::intent::DirectiveAnswer, String>)>;
pub(crate) type PreparationResponse = std::rc::Rc<dyn Fn(Result<wire::intent::TextAnswer, String>)>;
pub(crate) type TaskResponse = std::rc::Rc<dyn Fn(Result<wire::intent::TaskAnswer, String>)>;
pub(crate) type PrincipalResponse =
    std::rc::Rc<dyn Fn(Result<wire::principal::PrincipalStanding, String>)>;
pub(crate) type MessageResponse = DirectiveResponse;

pub use crate::runtime::TrustDecision;

/// One complete room publication request retained by the bounded action mailbox.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser room console and task detail post to rooms"
    )
)]
pub(crate) struct RoomPostAction {
    pub(crate) room: TaskHandle,
    pub(crate) head: wire::Seq,
    pub(crate) text: String,
    pub(crate) respond: RoomPostResponse,
}

impl RoomPostAction {
    /// Publish `text` in `room` at statement `head`, and run `then` only once
    /// the room records it; a refused post reports its reason to `refused`
    /// and nothing follows.
    pub(crate) fn then(
        room: TaskHandle,
        head: wire::Seq,
        text: String,
        then: impl FnOnce() + 'static,
        refused: impl Fn(String) + 'static,
    ) -> Self {
        let then = std::cell::Cell::new(Some(then));
        Self {
            room,
            head,
            text,
            respond: std::rc::Rc::new(move |result| match result {
                Ok(_) => {
                    if let Some(then) = then.take() {
                        then();
                    }
                }
                Err(issue) => refused(issue),
            }),
        }
    }
}

/// One principal control over an exact actor-addressed steering input.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser room conversation controls steering inputs"
    )
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SteerControlRequest {
    Line(wire::intent::SteerLineControl),
    Recover(wire::intent::DirectiveRef),
}

impl SteerControlRequest {
    pub(crate) fn line(
        input: wire::rooms::PrincipalInputRef,
        action: wire::intent::SteerLineAction,
        body: String,
    ) -> Self {
        let body = (!body.trim().is_empty()).then_some(body);
        Self::Line(wire::intent::SteerLineControl {
            input,
            action,
            body,
        })
    }
}

/// One preparation write with the heads its surface observed: an exact
/// proposal decision.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser approval control sends preparation requests"
    )
)]
pub(crate) enum PreparationRequest {
    Decide {
        request: Box<wire::rooms::PreparationDecide>,
        expected: Vec<wire::Witness>,
    },
}

/// One principal write, the same command `spice principal` sends.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser principals view writes principals"
    )
)]
pub(crate) enum PrincipalWrite {
    Admit(wire::principal::PrincipalAdmit),
    Edit(wire::principal::PrincipalEdit),
    Grant(wire::principal::PrincipalRoleRef),
    Revoke(wire::principal::PrincipalRoleRef),
    Deauthorize(wire::principal::PrincipalDeauthorize),
}

#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser connection asks a principal write for its command kind"
    )
)]
impl PrincipalWrite {
    pub(crate) const fn kind(&self) -> wire::CommandKind {
        match self {
            Self::Admit(_) => wire::CommandKind::PrincipalAdmit,
            Self::Edit(_) => wire::CommandKind::PrincipalEdit,
            Self::Grant(_) => wire::CommandKind::PrincipalGrant,
            Self::Revoke(_) => wire::CommandKind::PrincipalRevoke,
            Self::Deauthorize(_) => wire::CommandKind::PrincipalDeauthorize,
        }
    }

    /// Bytes of operator-supplied text the request carries.
    pub(crate) fn text_bytes(&self) -> usize {
        let fields = |name: &Option<String>, email: &Option<String>| {
            name.as_ref()
                .map_or(0, String::len)
                .saturating_add(email.as_ref().map_or(0, String::len))
        };
        match self {
            Self::Admit(admit) => fields(&admit.name, &admit.email),
            Self::Edit(edit) => fields(&edit.name, &edit.email),
            Self::Grant(_) | Self::Revoke(_) | Self::Deauthorize(_) => 0,
        }
    }
}

/// One typed task creation, statement revision, filing, or Draft image-set
/// replacement, with the heads it was drawn from.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser drafts view sends draft requests"
    )
)]
#[derive(Clone, Debug)]
pub(crate) enum DraftRequest {
    Create {
        request: wire::intent::TaskAdd,
        expected: Vec<wire::Witness>,
    },
    Edit {
        request: wire::intent::TaskEdit,
        expected: Vec<wire::Witness>,
    },
    File {
        request: wire::intent::TaskFile,
        expected: Vec<wire::Witness>,
    },
    Media {
        request: wire::intent::TaskMedia,
        expected: Vec<wire::Witness>,
    },
}

/// Wake one deferred filed task now, or set the instant its deferral wakes at.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser deferral control sends deferral requests"
    )
)]
pub(crate) enum DeferralRequest {
    Wake(wire::intent::TaskWake),
    Defer(wire::intent::TaskDefer),
}

#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only the wasm32 browser connection dispatches client actions"
    )
)]
pub(crate) enum ClientAction {
    Rack {
        request: wire::intent::RackRead,
        respond: RackResponse,
    },
    Jobs {
        respond: JobsResponse,
    },
    Job {
        target: wire::intent::LaneJobRef,
        stop: bool,
        respond: JobResponse,
    },
    /// Publish principal-authored text to a task room.
    RoomPost(RoomPostAction),
    /// Settle or recover one exact principal-addressed steering input.
    SteerControl {
        request: SteerControlRequest,
        line_respond: SteerLineResponse,
        directive_respond: DirectiveResponse,
    },
    /// Decide a Design pass proposal or record review evidence over it.
    Preparation {
        request: PreparationRequest,
        respond: PreparationResponse,
    },
    /// Admit a principal, edit a profile, change a role, or retire a
    /// capability.
    Principal {
        request: PrincipalWrite,
        respond: PrincipalResponse,
    },
    /// Create or file an operator Draft, or revise a task statement.
    Draft {
        request: DraftRequest,
        respond: TaskResponse,
    },
    /// Conclude one open Design done or collapsed at the heads its view was
    /// drawn from, so a claim or transition landing since refuses it.
    Conclude {
        request: wire::intent::TaskConclude,
        expected: Vec<wire::Witness>,
        respond: TaskResponse,
    },
    /// Wake or reschedule one pending filed task at the workflow and deferral
    /// heads its view was drawn from.
    Deferral {
        request: DeferralRequest,
        expected: Vec<wire::Witness>,
        respond: TaskResponse,
    },
    /// Wake the connection vendor after a composer synchronously queues work.
    DrainSubmissions,
    GlobalSettings(GlobalSettingsSet),
    /// Wake the connection to send task detail reads the runtime owes.
    TaskDetails,
    TaskRelations {
        key: crate::TaskRelationsKey,
    },
    /// Allow or deny one queued consent request. The witnesses are derived
    /// from the queue current when the ruling is sent, not when it was chosen.
    ConsentRuling {
        request: wire::CanonicalUuid,
        answer: wire::Answer,
    },
    /// Approve or reject one exact executable digest required by a current
    /// task statement. Resident admission revalidates the statement and derives
    /// producing-worktree provenance; the browser supplies no filesystem path.
    TrustDecision {
        task: TaskStamp,
        revision: wire::Seq,
        digest: wire::Digest,
        decision: TrustDecision,
        expected: Vec<wire::Witness>,
    },
    MessageHistory {
        generation: u64,
        lane: CanonicalUuid,
        before: Option<wire::MessageLocus>,
    },
    RenewLane {
        topology_revision: u64,
        team: CanonicalUuid,
        lane: CanonicalUuid,
        requested: bool,
        /// The member reservation this renewal settles, when one issued it.
        member: Option<crate::runtime::MemberRequest>,
    },
    /// Wake, drain, or remove one resident member.
    LaneState {
        request: crate::runtime::MemberRequest,
    },
    /// Restate the model one seat's next occupant launches with.
    RetuneLane {
        topology_revision: u64,
        request: crate::runtime::MemberRequest,
    },
    ConfigureTeam {
        topology_revision: u64,
        team: CanonicalUuid,
        member: Option<CanonicalUuid>,
        /// Start a fresh team with this member rather than join the named team.
        new_team: bool,
        drive: Option<bool>,
        drain: Option<bool>,
        /// Replacement team fast-mode flag, or absent to preserve it.
        fast_mode: Option<bool>,
    },
    /// Where every team and every agent stands after one team drag.
    ///
    /// The whole topology travels because that is the only thing the resident
    /// admits: a drop states an arrangement, not a move, so the surface that
    /// drew the teams is the one that says where they all now stand.
    ArrangeTeams {
        topology_revision: u64,
        teams: Vec<TeamArrangement>,
        /// Optional result reporter for an explicit menu lifecycle action.
        respond: Option<ArrangementResponse>,
    },
    /// Close one task with the composed text as its validation.
    ///
    /// The witnesses are the heads of the detail the operator closed from, so
    /// a transition or claim landing after that render refuses the closure
    /// instead of settling work the operator never saw.
    Close {
        id: CanonicalUuid,
        task: TaskHandle,
        validation: String,
        expected: Vec<wire::Witness>,
        force_claimed: bool,
    },
    /// Withdraw one task with the composed text as its reason.
    Delete {
        id: CanonicalUuid,
        task: TaskHandle,
        reason: String,
        expected: Vec<wire::Witness>,
        force_claimed: bool,
    },
    /// Message composed at the shared entry point. An absent lane delegates
    /// recipient choice to the resident; an explicit lane is witnessed from
    /// the connection's current residency snapshot immediately before send.
    ComposeMessage {
        lane: Option<CanonicalUuid>,
        message: String,
        attachments: Vec<Media>,
        respond: MessageResponse,
    },
    /// Direction the operator aimed at one seated lane from its tile.
    ///
    /// The occupant and epoch travel with the message because the residency
    /// witness the resident re-proves is built from the same inbox snapshot the
    /// tile was drawn from. Recomputing them at the socket would let the tile
    /// name one lane while the command named another.
    Steer {
        id: CanonicalUuid,
        fanout: CanonicalUuid,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        epoch: CanonicalUuid,
        message: String,
        attachments: Vec<Media>,
    },
}

#[cfg(test)]
mod tests;
