//! Closed command body and answer bindings.

mod answer;
mod principal;
mod rooms;

use serde::Serialize;

use crate::QuoteResolution;
use crate::command::CommandKind;
use crate::principal::{
    PrincipalAdmit, PrincipalDeauthorize, PrincipalDirectoryAnswer, PrincipalDirectoryRead,
    PrincipalEdit, PrincipalRead, PrincipalRoleRef, PrincipalStanding, PrincipalsAnswer,
};
use crate::rooms::{OutputAdopt, PreparationDecide, RoomPost, RoomPostAnswer};
use crate::topic::{TopicAnswer, TopicEdit, TopicRead};

use answer::AnswerSource;

use super::{
    BoardAnswer, BoardGraph, BoardHandout, BoardRead, ClaimAnswer, ClaimWin, ConsentAnswer,
    ConsentDecide, ConsentPolicy, ConsentPolicyAnswer, ConsentQueueAnswer, ConsentRead,
    DirectiveAnswer, DirectiveRef, DocLedger, GlobalSettingsAnswer, GlobalSettingsRead,
    GlobalSettingsSet, InboxAnswer, LaneAnswer, LaneChangeAnswer, LaneEffortRetune, LaneFastRetune,
    LaneJobAnswer, LaneJobRef, LaneJobsAnswer, LaneJobsRead, LaneRead, LaneRecall, LaneRenewal,
    LaneRenewalAnswer, LaneRetune, LaneSeat, LaneState, QuoteRead, RackAnswer, RackRead,
    RecallAnswer, RecognitionAnswer, Recognize, RoomContribute, RoomContributeAnswer,
    RoomContributionRead, RoomContributionReadAnswer, RoomConversationText,
    RoomConversationTextAnswer, RoomLeave, RoomLeaveAnswer, RoomScore, RoomScoreAnswer,
    StateAnswer, SteerInbox, SteerLineAnswer, SteerLineControl, SteerSend, TaskAdd, TaskAnswer,
    TaskClose, TaskConclude, TaskDefer, TaskDelete, TaskDepends, TaskDetail, TaskDetailAnswer,
    TaskDone, TaskEdit, TaskFile, TaskInvite, TaskInviteAnswer, TaskJoin, TaskMedia, TaskNote,
    TaskOopsAdd, TaskPlace, TaskPlaceAnswer, TaskRef, TaskRelations, TaskRelationsAnswer,
    TaskReview, TaskSweep, TaskTarget, TaskTimeline, TaskTimelineAnswer, TaskWake, TasteAnswer,
    TeamArrange, TeamConfigure, TextAnswer, TrustAnswer, TrustRef, TrustShowAnswer, UpChange,
    VendorCatalog, VendorCatalogAnswer,
};

/// Which intent DTO a command kind's body decodes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BodyShape {
    /// [`crate::outbox::Submit`]
    OutboxSubmit,
    /// [`crate::outbox::Read`]
    OutboxRead,
    /// [`BoardRead`]
    BoardRead,
    /// [`TaskDetail`]
    TaskDetail,
    /// [`TaskRelations`]
    TaskRelations,
    /// [`TaskTimeline`]
    TaskTimeline,
    /// [`BoardGraph`]
    BoardGraph,
    /// [`BoardHandout`]
    BoardHandout,
    /// [`DocLedger`]
    DocLedger,
    /// [`RoomScore`]
    RoomScore,
    /// [`RoomPost`]
    RoomPost,
    /// [`RoomConversationText`]
    RoomConversationText,
    /// [`RoomLeave`]
    RoomLeave,
    /// [`RoomContribute`]
    RoomContribute,
    /// [`RoomContributionRead`]
    RoomContributionRead,
    /// [`PreparationDecide`]
    PreparationDecide,
    /// [`OutputAdopt`]
    OutputAdopt,
    /// [`RackRead`]
    RackRead,
    /// [`GlobalSettingsRead`]
    GlobalSettingsRead,
    /// [`GlobalSettingsSet`]
    GlobalSettingsSet,
    /// [`VendorCatalog`]
    VendorCatalog,
    /// [`TaskAdd`]
    TaskAdd,
    /// [`TaskOopsAdd`], the strict body for OOPS Design line-item intake.
    TaskOopsAdd,
    /// [`TaskSweep`], the strict body for one previewed debt sweep.
    TaskSweep,
    /// [`TaskEdit`]
    TaskEdit,
    /// [`TaskDelete`]
    TaskDelete,
    /// [`TaskClose`]
    TaskClose,
    /// [`TaskConclude`]
    TaskConclude,
    /// [`TaskNote`]
    TaskNote,
    /// [`TaskDefer`]
    TaskDefer,
    /// [`TaskFile`]
    TaskFile,
    /// [`TaskPlace`]
    TaskPlace,
    /// [`TaskMedia`]
    TaskMedia,
    /// [`TaskWake`]
    TaskWake,
    /// [`TaskRef`]
    TaskRef,
    /// [`TaskDepends`]
    TaskDepends,
    /// [`TaskTarget`]
    TaskTarget,
    /// [`TaskJoin`]
    TaskJoin,
    /// [`TaskInvite`]
    TaskInvite,
    /// [`TaskDone`]
    TaskDone,
    /// [`TaskReview`]
    TaskReview,
    /// [`ClaimWin`]
    ClaimWin,
    /// [`SteerSend`]
    SteerSend,
    /// [`SteerInbox`]
    SteerInbox,
    /// [`SteerLineControl`]
    SteerLineControl,
    /// [`DirectiveRef`]
    DirectiveRef,
    /// [`QuoteRead`]
    QuoteRead,
    /// [`TopicRead`]
    TopicRead,
    /// [`TopicEdit`]
    TopicEdit,
    /// [`ConsentDecide`]
    ConsentDecide,
    /// [`ConsentRead`]
    ConsentRead,
    /// [`ConsentPolicy`]
    ConsentPolicy,
    /// [`TrustRef`]
    TrustRef,
    /// [`LaneRead`]
    LaneRead,
    /// [`LaneJobsRead`]
    LaneJobs,
    /// [`LaneJobRef`]
    LaneJob,
    /// [`LaneSeat`]
    LaneSeat,
    /// [`LaneState`]
    LaneState,
    /// [`LaneRenewal`]
    LaneRenewal,
    /// [`LaneRetune`]
    LaneRetune,
    /// [`LaneEffortRetune`]
    LaneEffortRetune,
    /// [`LaneFastRetune`]
    LaneFastRetune,
    /// [`TeamConfigure`]
    TeamConfigure,
    /// [`TeamArrange`]
    TeamArrange,
    /// [`LaneRecall`]
    LaneRecall,
    /// [`Recognize`]
    Recognize,
    /// [`UpChange`]
    UpChange,
    /// [`PrincipalDirectoryRead`]
    PrincipalDirectoryRead,
    /// [`PrincipalRead`]
    PrincipalRead,
    /// [`PrincipalAdmit`]
    PrincipalAdmit,
    /// [`PrincipalEdit`]
    PrincipalEdit,
    /// [`PrincipalRoleRef`]
    PrincipalRoleRef,
    /// [`PrincipalDeauthorize`]
    PrincipalDeauthorize,
}

/// Which answer DTO a command kind's reply carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnswerShape {
    /// [`crate::outbox::Receipt`]
    Outbox,
    /// [`BoardAnswer`]
    Board,
    /// [`TaskDetailAnswer`]
    TaskDetail,
    /// [`TaskRelationsAnswer`]
    TaskRelations,
    /// [`TaskTimelineAnswer`]
    TaskTimeline,
    /// [`TextAnswer`]
    Text,
    /// [`RoomScoreAnswer`]
    RoomScore,
    /// [`RoomPostAnswer`]
    RoomPost,
    /// [`RoomLeaveAnswer`]
    RoomLeave,
    /// [`RoomContributeAnswer`]
    RoomContribute,
    /// [`RoomContributionReadAnswer`]
    RoomContributionRead,
    /// [`RoomConversationTextAnswer`]
    RoomConversationText,
    /// [`TaskAnswer`]
    Task,
    /// [`TaskPlaceAnswer`]
    TaskPlace,
    /// [`ClaimAnswer`]
    Claim,
    /// [`TaskInviteAnswer`]
    TaskInvite,
    /// [`DirectiveAnswer`]
    Directive,
    /// [`InboxAnswer`]
    Inbox,
    /// [`SteerLineAnswer`]
    SteerLine,
    /// [`ConsentAnswer`]
    Consent,
    /// [`ConsentQueueAnswer`]
    ConsentQueue,
    /// [`ConsentPolicyAnswer`]
    ConsentPolicy,
    /// [`RackAnswer`]
    Rack,
    /// [`GlobalSettingsAnswer`]
    GlobalSettings,
    /// [`VendorCatalogAnswer`]
    VendorCatalog,
    /// [`TrustAnswer`]
    Trust,
    /// [`TrustShowAnswer`]
    TrustShow,
    /// [`LaneAnswer`]
    Lane,
    /// [`LaneJobsAnswer`]
    LaneJobs,
    /// [`LaneJobAnswer`]
    LaneJob,
    /// [`LaneChangeAnswer`]
    LaneChange,
    /// [`LaneRenewalAnswer`]
    LaneRenewal,
    /// [`StateAnswer`]
    State,
    /// [`RecallAnswer`]
    Recall,
    /// [`TasteAnswer`]
    Taste,
    /// [`RecognitionAnswer`]
    Recognition,
    /// [`QuoteResolution`]
    Quote,
    /// [`TopicAnswer`]
    Topic,
    /// [`PrincipalDirectoryAnswer`]
    PrincipalDirectory,
    /// [`PrincipalsAnswer`]
    Principals,
    /// [`PrincipalStanding`]
    Principal,
}

/// One decoded successful reply body, tagged by the shape its command selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommandAnswer {
    /// [`AnswerShape::Outbox`]
    Outbox(Box<crate::outbox::Receipt>),
    /// [`AnswerShape::Board`]
    Board(Box<BoardAnswer>),
    /// [`AnswerShape::TaskDetail`]
    TaskDetail(Box<TaskDetailAnswer>),
    /// [`AnswerShape::TaskRelations`]
    TaskRelations(Box<TaskRelationsAnswer>),
    /// [`AnswerShape::TaskTimeline`]
    TaskTimeline(Box<TaskTimelineAnswer>),
    /// [`AnswerShape::Text`]
    Text(TextAnswer),
    /// [`AnswerShape::RoomScore`]
    RoomScore(Box<RoomScoreAnswer>),
    /// [`AnswerShape::RoomPost`]
    RoomPost(RoomPostAnswer),
    /// [`AnswerShape::RoomLeave`]
    RoomLeave(RoomLeaveAnswer),
    /// [`AnswerShape::RoomContribute`]
    RoomContribute(Box<RoomContributeAnswer>),
    /// [`AnswerShape::RoomContributionRead`]
    RoomContributionRead(Box<RoomContributionReadAnswer>),
    /// [`AnswerShape::RoomConversationText`]
    RoomConversationText(Box<RoomConversationTextAnswer>),
    /// [`AnswerShape::Task`]
    Task(Box<TaskAnswer>),
    /// [`AnswerShape::TaskPlace`]
    TaskPlace(Box<TaskPlaceAnswer>),
    /// [`AnswerShape::Claim`]
    Claim(Box<ClaimAnswer>),
    /// [`AnswerShape::TaskInvite`]
    TaskInvite(Box<TaskInviteAnswer>),
    /// [`AnswerShape::Directive`]
    Directive(DirectiveAnswer),
    /// [`AnswerShape::Inbox`]
    Inbox(InboxAnswer),
    /// [`AnswerShape::SteerLine`]
    SteerLine(Box<SteerLineAnswer>),
    /// [`AnswerShape::Consent`]
    Consent(ConsentAnswer),
    /// [`AnswerShape::ConsentQueue`]
    ConsentQueue(ConsentQueueAnswer),
    /// [`AnswerShape::ConsentPolicy`]
    ConsentPolicy(ConsentPolicyAnswer),
    /// [`AnswerShape::Rack`]
    Rack(Box<RackAnswer>),
    /// [`AnswerShape::GlobalSettings`]
    GlobalSettings(GlobalSettingsAnswer),
    /// [`AnswerShape::VendorCatalog`]
    VendorCatalog(VendorCatalogAnswer),
    /// [`AnswerShape::Trust`]
    Trust(TrustAnswer),
    /// [`AnswerShape::TrustShow`]
    TrustShow(Box<TrustShowAnswer>),
    /// [`AnswerShape::Lane`]
    Lane(Box<LaneAnswer>),
    /// [`AnswerShape::LaneJobs`]
    LaneJobs(Box<LaneJobsAnswer>),
    /// [`AnswerShape::LaneJob`]
    LaneJob(Box<LaneJobAnswer>),
    /// [`AnswerShape::LaneChange`]
    LaneChange(Box<LaneChangeAnswer>),
    /// [`AnswerShape::LaneRenewal`]
    LaneRenewal(Box<LaneRenewalAnswer>),
    /// [`AnswerShape::State`]
    State(StateAnswer),
    /// [`AnswerShape::Recall`]
    Recall(Box<RecallAnswer>),
    /// [`AnswerShape::Taste`]
    Taste(TasteAnswer),
    /// [`AnswerShape::Recognition`]
    Recognition(RecognitionAnswer),
    /// [`AnswerShape::Quote`]
    Quote(Box<QuoteResolution>),
    /// [`AnswerShape::Topic`]
    Topic(Box<TopicAnswer>),
    /// [`AnswerShape::PrincipalDirectory`]
    PrincipalDirectory(PrincipalDirectoryAnswer),
    /// [`AnswerShape::Principals`]
    Principals(PrincipalsAnswer),
    /// [`AnswerShape::Principal`]
    Principal(Box<PrincipalStanding>),
}

impl CommandAnswer {
    /// The shape this decoded answer carries.
    #[must_use]
    pub const fn shape(&self) -> AnswerShape {
        match self {
            Self::Outbox(_) => AnswerShape::Outbox,
            Self::Board(_)
            | Self::TaskDetail(_)
            | Self::TaskRelations(_)
            | Self::TaskTimeline(_)
            | Self::Text(_)
            | Self::RoomScore(_)
            | Self::RoomPost(_)
            | Self::RoomLeave(_)
            | Self::RoomContribute(_)
            | Self::RoomContributionRead(_)
            | Self::RoomConversationText(_)
            | Self::Task(_)
            | Self::TaskPlace(_)
            | Self::Claim(_)
            | Self::TaskInvite(_) => self.board_answer_shape(),
            Self::Directive(_) => AnswerShape::Directive,
            Self::Inbox(_) => AnswerShape::Inbox,
            Self::Consent(_) => AnswerShape::Consent,
            Self::SteerLine(_) => AnswerShape::SteerLine,
            Self::ConsentQueue(_) => AnswerShape::ConsentQueue,
            Self::ConsentPolicy(_) => AnswerShape::ConsentPolicy,
            Self::Rack(_) => AnswerShape::Rack,
            Self::GlobalSettings(_) => AnswerShape::GlobalSettings,
            Self::VendorCatalog(_) => AnswerShape::VendorCatalog,
            Self::Trust(_) => AnswerShape::Trust,
            Self::TrustShow(_) => AnswerShape::TrustShow,
            Self::Lane(_)
            | Self::LaneJobs(_)
            | Self::LaneJob(_)
            | Self::LaneChange(_)
            | Self::LaneRenewal(_) => self.lane_shape(),
            Self::State(_) => AnswerShape::State,
            Self::Recall(_) => AnswerShape::Recall,
            Self::Taste(_) => AnswerShape::Taste,
            Self::Recognition(_) => AnswerShape::Recognition,
            Self::Quote(_) => AnswerShape::Quote,
            Self::Topic(_) => AnswerShape::Topic,
            Self::PrincipalDirectory(_) => AnswerShape::PrincipalDirectory,
            Self::Principals(_) => AnswerShape::Principals,
            Self::Principal(_) => AnswerShape::Principal,
        }
    }
    const fn board_answer_shape(&self) -> AnswerShape {
        match self {
            Self::Board(_) => AnswerShape::Board,
            Self::TaskDetail(_) => AnswerShape::TaskDetail,
            Self::TaskRelations(_) => AnswerShape::TaskRelations,
            Self::TaskTimeline(_) => AnswerShape::TaskTimeline,
            Self::Text(_) => AnswerShape::Text,
            Self::RoomScore(_) => AnswerShape::RoomScore,
            Self::RoomPost(_) => AnswerShape::RoomPost,
            Self::RoomLeave(_) => AnswerShape::RoomLeave,
            Self::RoomContribute(_) => AnswerShape::RoomContribute,
            Self::RoomContributionRead(_) => AnswerShape::RoomContributionRead,
            Self::RoomConversationText(_) => AnswerShape::RoomConversationText,
            Self::Task(_) => AnswerShape::Task,
            Self::TaskPlace(_) => AnswerShape::TaskPlace,
            Self::Claim(_) => AnswerShape::Claim,
            Self::TaskInvite(_) => AnswerShape::TaskInvite,
            _ => panic!("board answer shape partition is closed"),
        }
    }

    const fn lane_shape(&self) -> AnswerShape {
        match self {
            Self::Lane(_) => AnswerShape::Lane,
            Self::LaneJobs(_) => AnswerShape::LaneJobs,
            Self::LaneJob(_) => AnswerShape::LaneJob,
            Self::LaneChange(_) => AnswerShape::LaneChange,
            Self::LaneRenewal(_) => AnswerShape::LaneRenewal,
            _ => panic!("lane answer shape partition is closed"),
        }
    }
}

/// The body and answer a command kind selects.
///
/// WIRE binds these together with the witness schema and the transition, so
/// they are read off one discriminant rather than three independent lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Intent DTO the caller sends.
    pub body: BodyShape,
    /// Answer DTO a successful reply carries.
    pub answer: AnswerShape,
}

impl Binding {
    /// Bind a body to an answer.
    const fn of(body: BodyShape, answer: AnswerShape) -> Self {
        Self { body, answer }
    }
}

impl CommandKind {
    /// The body and answer this kind selects.
    ///
    /// Exhaustive by construction: a kind added without a row here does not
    /// compile, which is the same law the witness registry carries.
    #[must_use]
    pub const fn binding(self) -> Binding {
        match self {
            Self::OutboxSubmit => Binding::of(BodyShape::OutboxSubmit, AnswerShape::Outbox),
            Self::OutboxRead => Binding::of(BodyShape::OutboxRead, AnswerShape::Outbox),
            Self::TopicRead => Binding::of(BodyShape::TopicRead, AnswerShape::Topic),
            Self::TopicEdit => Binding::of(BodyShape::TopicEdit, AnswerShape::Topic),
            Self::LaneJobsRead => Binding::of(BodyShape::LaneJobs, AnswerShape::LaneJobs),
            Self::LaneJobRead | Self::LaneJobStop => {
                Binding::of(BodyShape::LaneJob, AnswerShape::LaneJob)
            }
            Self::PrincipalDirectoryRead
            | Self::PrincipalRead
            | Self::PrincipalAdmit
            | Self::PrincipalEdit
            | Self::PrincipalGrant
            | Self::PrincipalRevoke
            | Self::PrincipalDeauthorize => self.principal_binding(),
            Self::RoomScore
            | Self::RoomPost
            | Self::RoomConversationText
            | Self::RoomLeave
            | Self::RoomContribute
            | Self::RoomContributionRead => self.room_binding(),
            Self::PreparationDecide
            | Self::OutputAdopt
            | Self::BoardRead
            | Self::TaskDetail
            | Self::TaskRelations
            | Self::TaskTimeline
            | Self::BoardGraph
            | Self::BoardHandout
            | Self::DocLedger
            | Self::TaskAdd
            | Self::TaskOopsAdd
            | Self::TaskSweep
            | Self::TaskEdit
            | Self::TaskDelete
            | Self::TaskClose
            | Self::TaskConclude
            | Self::TaskNote
            | Self::TaskDefer
            | Self::TaskWake
            | Self::TaskFile
            | Self::TaskPlace
            | Self::TaskMedia
            | Self::TaskDepends
            | Self::TaskTarget
            | Self::TaskJoin
            | Self::TaskInvite
            | Self::TaskDone
            | Self::TaskReview
            | Self::ClaimWin
            | Self::ClaimRefresh
            | Self::ClaimFree
            | Self::ClaimLapse => self.board_binding(),
            Self::SteerSend
            | Self::SteerInbox
            | Self::SteerLineControl
            | Self::SteerResend
            | Self::SteerRecover
            | Self::SteerCancel
            | Self::QuoteRead
            | Self::ConsentRead
            | Self::ConsentPolicy
            | Self::ConsentDecide
            | Self::TrustPend
            | Self::TrustGrant
            | Self::TrustRevoke
            | Self::TrustShow
            | Self::LaneRead
            | Self::LaneSeat
            | Self::LaneState
            | Self::LaneRenewal
            | Self::LaneRetune
            | Self::LaneEffortRetune
            | Self::LaneFastRetune
            | Self::TeamConfigure
            | Self::TeamArrange
            | Self::LaneRecall
            | Self::RackRead
            | Self::GlobalSettingsRead
            | Self::GlobalSettingsSet
            | Self::VendorCatalog
            | Self::ProofRecognize
            | Self::UpChange => self.runtime_binding(),
        }
    }

    const fn board_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::BoardRead => Binding::of(B::BoardRead, A::Board),
            Self::TaskDetail => Binding::of(B::TaskDetail, A::TaskDetail),
            Self::TaskRelations => Binding::of(B::TaskRelations, A::TaskRelations),
            Self::TaskTimeline => Binding::of(B::TaskTimeline, A::TaskTimeline),
            Self::BoardGraph => Binding::of(B::BoardGraph, A::Text),
            Self::BoardHandout => Binding::of(B::BoardHandout, A::Text),
            Self::DocLedger => Binding::of(B::DocLedger, A::Text),
            Self::PreparationDecide | Self::OutputAdopt => self.preparation_binding(),
            Self::TaskAdd | Self::TaskOopsAdd | Self::TaskSweep => self.creation_binding(),
            Self::TaskEdit => Binding::of(B::TaskEdit, A::Task),
            Self::TaskDelete => Binding::of(B::TaskDelete, A::Task),
            Self::TaskClose => Binding::of(B::TaskClose, A::Task),
            Self::TaskConclude => Binding::of(B::TaskConclude, A::Task),
            Self::TaskNote => Binding::of(B::TaskNote, A::Task),
            Self::TaskDefer => Binding::of(B::TaskDefer, A::Task),
            Self::TaskWake => Binding::of(B::TaskWake, A::Task),
            Self::TaskFile => Binding::of(B::TaskFile, A::Task),
            Self::TaskPlace => Binding::of(B::TaskPlace, A::TaskPlace),
            Self::TaskMedia => Binding::of(B::TaskMedia, A::Task),
            Self::TaskDepends => Binding::of(B::TaskDepends, A::Task),
            Self::TaskTarget | Self::TaskJoin | Self::TaskInvite => self.staffing_binding(),
            Self::TaskDone => Binding::of(B::TaskDone, A::Task),
            Self::TaskReview => Binding::of(B::TaskReview, A::Task),
            Self::ClaimWin | Self::ClaimRefresh | Self::ClaimFree | Self::ClaimLapse => {
                self.claim_binding()
            }
            _ => panic!("command binding partition is closed"),
        }
    }
    const fn creation_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::TaskAdd => Binding::of(B::TaskAdd, A::Task),
            Self::TaskOopsAdd => Binding::of(B::TaskOopsAdd, A::Task),
            Self::TaskSweep => Binding::of(B::TaskSweep, A::Task),
            _ => panic!("creation command binding partition is closed"),
        }
    }
    const fn preparation_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::PreparationDecide => Binding::of(B::PreparationDecide, A::Text),
            Self::OutputAdopt => Binding::of(B::OutputAdopt, A::Text),
            _ => panic!("preparation command binding partition is closed"),
        }
    }

    const fn staffing_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::TaskTarget => Binding::of(B::TaskTarget, A::Task),
            Self::TaskJoin => Binding::of(B::TaskJoin, A::Claim),
            Self::TaskInvite => Binding::of(B::TaskInvite, A::TaskInvite),
            _ => panic!("staffing command binding partition is closed"),
        }
    }

    const fn claim_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::ClaimWin => Binding::of(B::ClaimWin, A::Claim),
            Self::ClaimRefresh => Binding::of(B::TaskRef, A::Claim),
            Self::ClaimFree | Self::ClaimLapse => Binding::of(B::TaskRef, A::Claim),
            _ => panic!("claim command binding partition is closed"),
        }
    }

    const fn settings_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::GlobalSettingsRead => Binding::of(B::GlobalSettingsRead, A::GlobalSettings),
            Self::GlobalSettingsSet => Binding::of(B::GlobalSettingsSet, A::GlobalSettings),
            _ => panic!("settings command binding partition is closed"),
        }
    }

    const fn runtime_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::SteerSend => Binding::of(B::SteerSend, A::Directive),
            Self::SteerInbox => Binding::of(B::SteerInbox, A::Inbox),
            Self::SteerLineControl => Binding::of(B::SteerLineControl, A::SteerLine),
            Self::SteerResend | Self::SteerRecover | Self::SteerCancel => {
                Binding::of(B::DirectiveRef, A::Directive)
            }
            Self::QuoteRead => Binding::of(B::QuoteRead, A::Quote),
            Self::ConsentDecide => Binding::of(B::ConsentDecide, A::Consent),
            Self::ConsentRead => Binding::of(B::ConsentRead, A::ConsentQueue),
            Self::ConsentPolicy => Binding::of(B::ConsentPolicy, A::ConsentPolicy),
            Self::TrustPend | Self::TrustGrant | Self::TrustRevoke => {
                Binding::of(B::TrustRef, A::Trust)
            }
            Self::TrustShow => Binding::of(B::TrustRef, A::TrustShow),
            Self::LaneRead => Binding::of(B::LaneRead, A::Lane),
            Self::LaneSeat => Binding::of(B::LaneSeat, A::LaneChange),
            Self::LaneState => Binding::of(B::LaneState, A::LaneChange),
            Self::LaneRenewal => Binding::of(B::LaneRenewal, A::LaneRenewal),
            Self::LaneRetune => Binding::of(B::LaneRetune, A::Lane),
            Self::LaneEffortRetune => Binding::of(B::LaneEffortRetune, A::Lane),
            Self::LaneFastRetune => Binding::of(B::LaneFastRetune, A::Lane),
            Self::TeamConfigure => Binding::of(B::TeamConfigure, A::Lane),
            Self::TeamArrange => Binding::of(B::TeamArrange, A::Lane),
            Self::LaneRecall => Binding::of(B::LaneRecall, A::Recall),
            Self::RackRead => Binding::of(B::RackRead, A::Rack),
            Self::GlobalSettingsRead | Self::GlobalSettingsSet => self.settings_binding(),
            Self::VendorCatalog => Binding::of(B::VendorCatalog, A::VendorCatalog),
            Self::ProofRecognize => Binding::of(B::Recognize, A::Recognition),
            Self::UpChange => Binding::of(B::UpChange, A::State),
            _ => panic!("command binding partition is closed"),
        }
    }
}

/// One decoded command body, tagged by the shape its kind selected.
///
/// The resident decodes a frame before it knows which handler runs, so the
/// union exists to carry a body that is already typed but not yet dispatched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Intent {
    /// [`BodyShape::OutboxSubmit`]
    OutboxSubmit(crate::outbox::Submit),
    /// [`BodyShape::OutboxRead`]
    OutboxRead(crate::outbox::Read),
    /// [`BodyShape::BoardRead`]
    BoardRead(BoardRead),
    /// [`BodyShape::TaskDetail`]
    TaskDetail(TaskDetail),
    /// [`BodyShape::TaskRelations`]
    TaskRelations(TaskRelations),
    /// [`BodyShape::TaskTimeline`]
    TaskTimeline(TaskTimeline),
    /// [`BodyShape::BoardGraph`]
    BoardGraph(BoardGraph),
    /// [`BodyShape::BoardHandout`]
    BoardHandout(BoardHandout),
    /// [`BodyShape::DocLedger`]
    DocLedger(DocLedger),
    /// [`BodyShape::RoomScore`]
    RoomScore(RoomScore),
    /// [`BodyShape::RoomPost`]
    RoomPost(RoomPost),
    /// [`BodyShape::RoomConversationText`]
    RoomConversationText(RoomConversationText),
    /// [`BodyShape::RoomLeave`]
    RoomLeave(RoomLeave),
    /// [`BodyShape::RoomContribute`]
    RoomContribute(RoomContribute),
    /// [`BodyShape::RoomContributionRead`]
    RoomContributionRead(RoomContributionRead),
    /// [`BodyShape::PreparationDecide`]
    PreparationDecide(PreparationDecide),
    /// [`BodyShape::OutputAdopt`]
    OutputAdopt(OutputAdopt),
    /// [`BodyShape::RackRead`]
    RackRead(RackRead),
    /// [`BodyShape::GlobalSettingsRead`]
    GlobalSettingsRead(GlobalSettingsRead),
    /// [`BodyShape::GlobalSettingsSet`]
    GlobalSettingsSet(GlobalSettingsSet),
    /// [`BodyShape::VendorCatalog`]
    VendorCatalog(VendorCatalog),
    /// [`BodyShape::TaskAdd`]
    TaskAdd(TaskAdd),
    /// [`BodyShape::TaskOopsAdd`]
    TaskOopsAdd(TaskOopsAdd),
    /// [`BodyShape::TaskSweep`]
    TaskSweep(TaskSweep),
    /// [`BodyShape::TaskEdit`]
    TaskEdit(TaskEdit),
    /// [`BodyShape::TaskDelete`]
    TaskDelete(TaskDelete),
    /// [`BodyShape::TaskClose`]
    TaskClose(TaskClose),
    /// [`BodyShape::TaskConclude`]
    TaskConclude(TaskConclude),
    /// [`BodyShape::TaskNote`]
    TaskNote(TaskNote),
    /// [`BodyShape::TaskDefer`]
    TaskDefer(TaskDefer),
    /// [`BodyShape::TaskWake`]
    TaskWake(TaskWake),
    /// [`BodyShape::TaskFile`]
    TaskFile(TaskFile),
    /// [`BodyShape::TaskPlace`]
    TaskPlace(TaskPlace),
    /// [`BodyShape::TaskMedia`]
    TaskMedia(TaskMedia),
    /// [`BodyShape::TaskRef`]
    TaskRef(TaskRef),
    /// [`BodyShape::TaskDepends`]
    TaskDepends(TaskDepends),
    /// [`BodyShape::TaskTarget`]
    TaskTarget(TaskTarget),
    /// [`BodyShape::TaskJoin`]
    TaskJoin(TaskJoin),
    /// [`BodyShape::TaskInvite`]
    TaskInvite(TaskInvite),
    /// [`BodyShape::TaskDone`]
    TaskDone(TaskDone),
    /// [`BodyShape::TaskReview`]
    TaskReview(TaskReview),
    /// [`BodyShape::ClaimWin`]
    ClaimWin(ClaimWin),
    /// [`BodyShape::SteerSend`]
    SteerSend(SteerSend),
    /// [`BodyShape::SteerInbox`]
    SteerInbox(SteerInbox),
    /// [`BodyShape::SteerLineControl`]
    SteerLineControl(SteerLineControl),
    /// [`BodyShape::DirectiveRef`]
    DirectiveRef(DirectiveRef),
    /// [`BodyShape::QuoteRead`]
    QuoteRead(QuoteRead),
    /// [`BodyShape::TopicRead`]
    TopicRead(TopicRead),
    /// [`BodyShape::TopicEdit`]
    TopicEdit(TopicEdit),
    /// [`BodyShape::ConsentDecide`]
    ConsentDecide(ConsentDecide),
    /// [`BodyShape::ConsentRead`]
    ConsentRead(ConsentRead),
    /// [`BodyShape::ConsentPolicy`]
    ConsentPolicy(ConsentPolicy),
    /// [`BodyShape::TrustRef`]
    TrustRef(TrustRef),
    /// [`BodyShape::LaneRead`]
    LaneRead(LaneRead),
    /// [`BodyShape::LaneJobs`]
    LaneJobs(LaneJobsRead),
    /// [`BodyShape::LaneJob`]
    LaneJob(LaneJobRef),
    /// [`BodyShape::LaneSeat`]
    LaneSeat(LaneSeat),
    /// [`BodyShape::LaneState`]
    LaneState(LaneState),
    /// [`BodyShape::LaneRenewal`]
    LaneRenewal(LaneRenewal),
    /// [`BodyShape::LaneRetune`]
    LaneRetune(LaneRetune),
    /// [`BodyShape::LaneEffortRetune`]
    LaneEffortRetune(LaneEffortRetune),
    /// [`BodyShape::LaneFastRetune`]
    LaneFastRetune(LaneFastRetune),
    /// [`BodyShape::TeamConfigure`]
    TeamConfigure(TeamConfigure),
    /// [`BodyShape::TeamArrange`]
    TeamArrange(TeamArrange),
    /// [`BodyShape::LaneRecall`]
    LaneRecall(LaneRecall),
    /// [`BodyShape::Recognize`]
    Recognize(Recognize),
    /// [`BodyShape::UpChange`]
    UpChange(UpChange),
    /// [`BodyShape::PrincipalDirectoryRead`]
    PrincipalDirectoryRead(PrincipalDirectoryRead),
    /// [`BodyShape::PrincipalRead`]
    PrincipalRead(PrincipalRead),
    /// [`BodyShape::PrincipalAdmit`]
    PrincipalAdmit(PrincipalAdmit),
    /// [`BodyShape::PrincipalEdit`]
    PrincipalEdit(PrincipalEdit),
    /// [`BodyShape::PrincipalRoleRef`]
    PrincipalRoleRef(PrincipalRoleRef),
    /// [`BodyShape::PrincipalDeauthorize`]
    PrincipalDeauthorize(PrincipalDeauthorize),
}

impl Intent {
    /// The shape this decoded body carries.
    #[must_use]
    pub const fn shape(&self) -> BodyShape {
        match self {
            Self::OutboxSubmit(_) => BodyShape::OutboxSubmit,
            Self::OutboxRead(_) => BodyShape::OutboxRead,
            Self::TopicRead(_) => BodyShape::TopicRead,
            Self::TopicEdit(_) => BodyShape::TopicEdit,
            Self::LaneJobs(_) => BodyShape::LaneJobs,
            Self::LaneJob(_) => BodyShape::LaneJob,
            Self::PrincipalDirectoryRead(_)
            | Self::PrincipalRead(_)
            | Self::PrincipalAdmit(_)
            | Self::PrincipalEdit(_)
            | Self::PrincipalRoleRef(_)
            | Self::PrincipalDeauthorize(_) => self.principal_shape(),
            Self::RoomScore(_)
            | Self::RoomPost(_)
            | Self::RoomConversationText(_)
            | Self::RoomLeave(_)
            | Self::RoomContribute(_)
            | Self::RoomContributionRead(_) => self.room_shape(),
            Self::PreparationDecide(_)
            | Self::OutputAdopt(_)
            | Self::BoardRead(_)
            | Self::TaskDetail(_)
            | Self::TaskRelations(_)
            | Self::TaskTimeline(_)
            | Self::BoardGraph(_)
            | Self::BoardHandout(_)
            | Self::DocLedger(_)
            | Self::TaskAdd(_)
            | Self::TaskOopsAdd(_)
            | Self::TaskSweep(_)
            | Self::TaskEdit(_)
            | Self::TaskDelete(_)
            | Self::TaskClose(_)
            | Self::TaskConclude(_)
            | Self::TaskNote(_)
            | Self::TaskDefer(_)
            | Self::TaskWake(_)
            | Self::TaskFile(_)
            | Self::TaskPlace(_)
            | Self::TaskMedia(_)
            | Self::TaskRef(_)
            | Self::TaskDepends(_)
            | Self::TaskTarget(_)
            | Self::TaskJoin(_)
            | Self::TaskInvite(_)
            | Self::TaskDone(_)
            | Self::TaskReview(_)
            | Self::ClaimWin(_) => self.board_shape(),
            Self::SteerSend(_)
            | Self::SteerInbox(_)
            | Self::SteerLineControl(_)
            | Self::DirectiveRef(_)
            | Self::QuoteRead(_)
            | Self::ConsentDecide(_)
            | Self::ConsentRead(_)
            | Self::ConsentPolicy(_)
            | Self::TrustRef(_)
            | Self::LaneRead(_)
            | Self::LaneSeat(_)
            | Self::LaneState(_)
            | Self::LaneRenewal(_)
            | Self::LaneRetune(_)
            | Self::LaneEffortRetune(_)
            | Self::LaneFastRetune(_)
            | Self::TeamConfigure(_)
            | Self::TeamArrange(_)
            | Self::LaneRecall(_)
            | Self::RackRead(_)
            | Self::GlobalSettingsRead(_)
            | Self::GlobalSettingsSet(_)
            | Self::VendorCatalog(_)
            | Self::Recognize(_)
            | Self::UpChange(_) => self.runtime_shape(),
        }
    }

    const fn board_shape(&self) -> BodyShape {
        match self {
            Self::BoardRead(_) => BodyShape::BoardRead,
            Self::TaskDetail(_) => BodyShape::TaskDetail,
            Self::TaskRelations(_) => BodyShape::TaskRelations,
            Self::TaskTimeline(_) => BodyShape::TaskTimeline,
            Self::BoardGraph(_) => BodyShape::BoardGraph,
            Self::BoardHandout(_) => BodyShape::BoardHandout,
            Self::DocLedger(_) => BodyShape::DocLedger,
            Self::PreparationDecide(_) | Self::OutputAdopt(_) => self.preparation_shape(),
            Self::TaskAdd(_) | Self::TaskOopsAdd(_) | Self::TaskSweep(_) => self.creation_shape(),
            Self::TaskEdit(_) => BodyShape::TaskEdit,
            Self::TaskDelete(_) => BodyShape::TaskDelete,
            Self::TaskClose(_) => BodyShape::TaskClose,
            Self::TaskConclude(_) => BodyShape::TaskConclude,
            Self::TaskNote(_) => BodyShape::TaskNote,
            Self::TaskDefer(_) => BodyShape::TaskDefer,
            Self::TaskWake(_) => BodyShape::TaskWake,
            Self::TaskFile(_) => BodyShape::TaskFile,
            Self::TaskPlace(_) => BodyShape::TaskPlace,
            Self::TaskMedia(_) => BodyShape::TaskMedia,
            Self::TaskRef(_) => BodyShape::TaskRef,
            Self::TaskDepends(_) => BodyShape::TaskDepends,
            Self::TaskJoin(_) => BodyShape::TaskJoin,
            Self::TaskInvite(_) => BodyShape::TaskInvite,
            Self::TaskTarget(_) | Self::TaskDone(_) | Self::TaskReview(_) => self.workflow_shape(),
            Self::ClaimWin(_) => BodyShape::ClaimWin,
            _ => panic!("intent shape partition is closed"),
        }
    }
    const fn creation_shape(&self) -> BodyShape {
        match self {
            Self::TaskAdd(_) => BodyShape::TaskAdd,
            Self::TaskOopsAdd(_) => BodyShape::TaskOopsAdd,
            Self::TaskSweep(_) => BodyShape::TaskSweep,
            _ => panic!("creation intent shape partition is closed"),
        }
    }

    const fn preparation_shape(&self) -> BodyShape {
        match self {
            Self::PreparationDecide(_) => BodyShape::PreparationDecide,
            Self::OutputAdopt(_) => BodyShape::OutputAdopt,
            _ => panic!("preparation intent shape partition is closed"),
        }
    }

    const fn workflow_shape(&self) -> BodyShape {
        match self {
            Self::TaskTarget(_) => BodyShape::TaskTarget,
            Self::TaskDone(_) => BodyShape::TaskDone,
            Self::TaskReview(_) => BodyShape::TaskReview,
            _ => panic!("workflow intent shape partition is closed"),
        }
    }

    const fn settings_shape(&self) -> BodyShape {
        match self {
            Self::GlobalSettingsRead(_) => BodyShape::GlobalSettingsRead,
            Self::GlobalSettingsSet(_) => BodyShape::GlobalSettingsSet,
            _ => panic!("settings intent shape partition is closed"),
        }
    }

    const fn runtime_shape(&self) -> BodyShape {
        match self {
            Self::SteerSend(_) => BodyShape::SteerSend,
            Self::SteerInbox(_) => BodyShape::SteerInbox,
            Self::SteerLineControl(_) => BodyShape::SteerLineControl,
            Self::DirectiveRef(_) => BodyShape::DirectiveRef,
            Self::QuoteRead(_) => BodyShape::QuoteRead,
            Self::ConsentDecide(_) => BodyShape::ConsentDecide,
            Self::ConsentRead(_) => BodyShape::ConsentRead,
            Self::ConsentPolicy(_) => BodyShape::ConsentPolicy,
            Self::TrustRef(_) => BodyShape::TrustRef,
            Self::LaneRead(_) => BodyShape::LaneRead,
            Self::LaneSeat(_) => BodyShape::LaneSeat,
            Self::LaneState(_) => BodyShape::LaneState,
            Self::LaneRenewal(_) => BodyShape::LaneRenewal,
            Self::LaneRetune(_) => BodyShape::LaneRetune,
            Self::LaneEffortRetune(_) => BodyShape::LaneEffortRetune,
            Self::LaneFastRetune(_) => BodyShape::LaneFastRetune,
            Self::TeamConfigure(_) => BodyShape::TeamConfigure,
            Self::TeamArrange(_) => BodyShape::TeamArrange,
            Self::LaneRecall(_) => BodyShape::LaneRecall,
            Self::RackRead(_) => BodyShape::RackRead,
            Self::GlobalSettingsRead(_) | Self::GlobalSettingsSet(_) => self.settings_shape(),
            Self::VendorCatalog(_) => BodyShape::VendorCatalog,
            Self::Recognize(_) => BodyShape::Recognize,
            Self::UpChange(_) => BodyShape::UpChange,
            _ => panic!("intent shape partition is closed"),
        }
    }
}

impl BodyShape {
    /// Decode a command body into the intent this shape names.
    ///
    /// # Errors
    ///
    /// Returns the decoder error when the bytes are not that shape. Bodies
    /// refuse unknown fields, so a body spelled for the wrong kind is a
    /// decode failure rather than a silently truncated value.
    pub fn decode(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        match self {
            Self::OutboxSubmit => Ok(Intent::OutboxSubmit(crate::decode(bytes)?)),
            Self::OutboxRead => Ok(Intent::OutboxRead(crate::decode(bytes)?)),
            Self::TopicRead => Ok(Intent::TopicRead(crate::decode(bytes)?)),
            Self::TopicEdit => Ok(Intent::TopicEdit(crate::decode(bytes)?)),
            _ if self.is_board_body() => self.decode_board_body(bytes),
            _ => self.decode_runtime_body(bytes),
        }
    }

    const fn is_board_body(self) -> bool {
        matches!(
            self,
            Self::BoardRead
                | Self::TaskDetail
                | Self::TaskRelations
                | Self::TaskTimeline
                | Self::BoardGraph
                | Self::BoardHandout
                | Self::DocLedger
                | Self::RoomScore
                | Self::RoomPost
                | Self::RoomConversationText
                | Self::RoomLeave
                | Self::RoomContribute
                | Self::RoomContributionRead
                | Self::PreparationDecide
                | Self::OutputAdopt
                | Self::TaskAdd
                | Self::TaskOopsAdd
                | Self::TaskSweep
                | Self::TaskEdit
                | Self::TaskDelete
                | Self::TaskClose
                | Self::TaskConclude
                | Self::TaskNote
                | Self::TaskDefer
                | Self::TaskWake
                | Self::TaskFile
                | Self::TaskPlace
                | Self::TaskMedia
                | Self::TaskRef
                | Self::TaskDepends
                | Self::TaskTarget
                | Self::TaskJoin
                | Self::TaskInvite
                | Self::TaskDone
                | Self::TaskReview
                | Self::ClaimWin
        )
    }

    fn decode_board_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        if matches!(
            self,
            Self::BoardRead
                | Self::TaskDetail
                | Self::TaskRelations
                | Self::TaskTimeline
                | Self::BoardGraph
                | Self::BoardHandout
                | Self::DocLedger
                | Self::RoomScore
                | Self::RoomPost
                | Self::RoomConversationText
                | Self::RoomLeave
                | Self::RoomContribute
                | Self::RoomContributionRead
                | Self::PreparationDecide
                | Self::OutputAdopt
                | Self::TaskAdd
                | Self::TaskOopsAdd
                | Self::TaskSweep
                | Self::TaskEdit
                | Self::TaskDelete
                | Self::TaskClose
                | Self::TaskConclude
                | Self::TaskNote
        ) {
            self.decode_board_query_body(bytes)
        } else {
            self.decode_board_transition_body(bytes)
        }
    }

    fn decode_board_query_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::BoardRead => Intent::BoardRead(crate::decode(bytes)?),
            Self::TaskDetail => Intent::TaskDetail(crate::decode(bytes)?),
            Self::TaskRelations => Intent::TaskRelations(crate::decode(bytes)?),
            Self::TaskTimeline => Intent::TaskTimeline(crate::decode(bytes)?),
            Self::BoardGraph => Intent::BoardGraph(crate::decode(bytes)?),
            Self::BoardHandout => Intent::BoardHandout(crate::decode(bytes)?),
            Self::DocLedger => Intent::DocLedger(crate::decode(bytes)?),
            Self::RoomScore
            | Self::RoomPost
            | Self::RoomConversationText
            | Self::RoomLeave
            | Self::RoomContribute
            | Self::RoomContributionRead => return self.decode_room_body(bytes),
            Self::PreparationDecide | Self::OutputAdopt => {
                return self.decode_preparation_body(bytes);
            }
            _ => return self.decode_task_statement_body(bytes),
        })
    }

    fn decode_preparation_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::PreparationDecide => Intent::PreparationDecide(crate::decode(bytes)?),
            Self::OutputAdopt => Intent::OutputAdopt(crate::decode(bytes)?),
            _ => unreachable!("preparation body partition is closed"),
        })
    }

    fn decode_task_statement_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::TaskAdd => Intent::TaskAdd(crate::decode(bytes)?),
            Self::TaskOopsAdd => Intent::TaskOopsAdd(crate::decode(bytes)?),
            Self::TaskSweep => Intent::TaskSweep(crate::decode(bytes)?),
            Self::TaskEdit => Intent::TaskEdit(crate::decode(bytes)?),
            Self::TaskDelete => Intent::TaskDelete(crate::decode(bytes)?),
            Self::TaskClose => Intent::TaskClose(crate::decode(bytes)?),
            Self::TaskConclude => Intent::TaskConclude(crate::decode(bytes)?),
            Self::TaskNote => Intent::TaskNote(crate::decode(bytes)?),
            _ => unreachable!("board body partition is closed"),
        })
    }

    fn decode_board_transition_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::TaskDefer => Intent::TaskDefer(crate::decode(bytes)?),
            Self::TaskWake => Intent::TaskWake(crate::decode(bytes)?),
            Self::TaskFile => Intent::TaskFile(crate::decode(bytes)?),
            Self::TaskPlace => Intent::TaskPlace(crate::decode(bytes)?),
            Self::TaskMedia => Intent::TaskMedia(crate::decode(bytes)?),
            Self::TaskRef => Intent::TaskRef(crate::decode(bytes)?),
            Self::TaskDepends => Intent::TaskDepends(crate::decode(bytes)?),
            Self::TaskJoin => Intent::TaskJoin(crate::decode(bytes)?),
            Self::TaskInvite => Intent::TaskInvite(crate::decode(bytes)?),
            Self::TaskTarget | Self::TaskDone | Self::TaskReview => {
                return self.decode_workflow_body(bytes);
            }
            Self::ClaimWin => Intent::ClaimWin(crate::decode(bytes)?),
            _ => unreachable!("board body partition is closed"),
        })
    }

    fn decode_workflow_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::TaskTarget => Intent::TaskTarget(crate::decode(bytes)?),
            Self::TaskDone => Intent::TaskDone(crate::decode(bytes)?),
            Self::TaskReview => Intent::TaskReview(crate::decode(bytes)?),
            _ => unreachable!("workflow body partition is closed"),
        })
    }

    fn decode_runtime_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        if matches!(
            self,
            Self::SteerSend
                | Self::SteerInbox
                | Self::SteerLineControl
                | Self::DirectiveRef
                | Self::QuoteRead
                | Self::ConsentDecide
                | Self::ConsentRead
                | Self::ConsentPolicy
                | Self::RackRead
                | Self::GlobalSettingsRead
                | Self::GlobalSettingsSet
                | Self::VendorCatalog
                | Self::TrustRef
        ) {
            self.decode_coordination_body(bytes)
        } else if matches!(
            self,
            Self::LaneRead
                | Self::LaneJobs
                | Self::LaneJob
                | Self::LaneSeat
                | Self::LaneState
                | Self::LaneRenewal
                | Self::LaneRetune
                | Self::LaneEffortRetune
                | Self::LaneFastRetune
                | Self::TeamConfigure
                | Self::TeamArrange
        ) {
            self.decode_lane_body(bytes)
        } else {
            self.decode_runtime_control_body(bytes)
        }
    }

    fn decode_coordination_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::SteerSend => Intent::SteerSend(crate::decode(bytes)?),
            Self::SteerInbox => Intent::SteerInbox(crate::decode(bytes)?),
            Self::SteerLineControl => Intent::SteerLineControl(crate::decode(bytes)?),
            Self::DirectiveRef => Intent::DirectiveRef(crate::decode(bytes)?),
            Self::QuoteRead => Intent::QuoteRead(crate::decode(bytes)?),
            Self::ConsentDecide => Intent::ConsentDecide(crate::decode(bytes)?),
            Self::ConsentRead => Intent::ConsentRead(crate::decode(bytes)?),
            Self::ConsentPolicy => Intent::ConsentPolicy(crate::decode(bytes)?),
            Self::RackRead => Intent::RackRead(crate::decode(bytes)?),
            Self::GlobalSettingsRead => Intent::GlobalSettingsRead(crate::decode(bytes)?),
            Self::GlobalSettingsSet => Intent::GlobalSettingsSet(crate::decode(bytes)?),
            Self::VendorCatalog => Intent::VendorCatalog(crate::decode(bytes)?),
            Self::TrustRef => Intent::TrustRef(crate::decode(bytes)?),
            _ => unreachable!("runtime body partition is closed"),
        })
    }

    fn decode_lane_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::LaneRead => Intent::LaneRead(crate::decode(bytes)?),
            Self::LaneJobs => Intent::LaneJobs(crate::decode(bytes)?),
            Self::LaneJob => Intent::LaneJob(crate::decode(bytes)?),
            Self::LaneSeat => Intent::LaneSeat(crate::decode(bytes)?),
            Self::LaneState => Intent::LaneState(crate::decode(bytes)?),
            Self::LaneRenewal => Intent::LaneRenewal(crate::decode(bytes)?),
            Self::LaneRetune => Intent::LaneRetune(crate::decode(bytes)?),
            Self::LaneEffortRetune => Intent::LaneEffortRetune(crate::decode(bytes)?),
            Self::LaneFastRetune => Intent::LaneFastRetune(crate::decode(bytes)?),
            Self::TeamConfigure => Intent::TeamConfigure(crate::decode(bytes)?),
            Self::TeamArrange => Intent::TeamArrange(crate::decode(bytes)?),
            _ => unreachable!("lane body partition is closed"),
        })
    }

    fn decode_runtime_control_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::LaneRecall => Intent::LaneRecall(crate::decode(bytes)?),
            Self::PrincipalDirectoryRead
            | Self::PrincipalRead
            | Self::PrincipalAdmit
            | Self::PrincipalEdit
            | Self::PrincipalRoleRef
            | Self::PrincipalDeauthorize => return self.decode_principal_body(bytes),
            Self::Recognize => Intent::Recognize(crate::decode(bytes)?),
            Self::UpChange => Intent::UpChange(crate::decode(bytes)?),
            _ => unreachable!("runtime body partition is closed"),
        })
    }
}
