//! Closed command spellings and serialization.

use std::{fmt, str::FromStr};

use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use super::{CommandKind, CommandKindError};

impl CommandKind {
    /// Every command in the closed registry, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::OutboxSubmit,
        Self::OutboxRead,
        Self::BoardRead,
        Self::TaskDetail,
        Self::TaskRelations,
        Self::TaskTimeline,
        Self::BoardGraph,
        Self::BoardHandout,
        Self::DocLedger,
        Self::RoomScore,
        Self::RoomPost,
        Self::RoomConversationText,
        Self::RoomLeave,
        Self::RoomContribute,
        Self::RoomContributionRead,
        Self::PreparationDecide,
        Self::OutputAdopt,
        Self::RackRead,
        Self::GlobalSettingsRead,
        Self::GlobalSettingsSet,
        Self::VendorCatalog,
        Self::TaskAdd,
        Self::TaskOopsAdd,
        Self::TaskSweep,
        Self::TaskEdit,
        Self::TaskDelete,
        Self::TaskClose,
        Self::TaskConclude,
        Self::TaskNote,
        Self::TaskDefer,
        Self::TaskWake,
        Self::TaskFile,
        Self::TaskPlace,
        Self::TaskMedia,
        Self::TaskDepends,
        Self::TaskTarget,
        Self::TaskJoin,
        Self::TaskInvite,
        Self::TaskDone,
        Self::TaskReview,
        Self::ClaimWin,
        Self::ClaimRefresh,
        Self::ClaimFree,
        Self::ClaimLapse,
        Self::SteerSend,
        Self::SteerInbox,
        Self::SteerResend,
        Self::SteerRecover,
        Self::SteerCancel,
        Self::SteerLineControl,
        Self::QuoteRead,
        Self::TopicRead,
        Self::TopicEdit,
        Self::ConsentRead,
        Self::ConsentPolicy,
        Self::ConsentDecide,
        Self::TrustPend,
        Self::TrustGrant,
        Self::TrustRevoke,
        Self::TrustShow,
        Self::PrincipalDirectoryRead,
        Self::PrincipalRead,
        Self::PrincipalAdmit,
        Self::PrincipalEdit,
        Self::PrincipalGrant,
        Self::PrincipalRevoke,
        Self::PrincipalDeauthorize,
        Self::LaneRead,
        Self::LaneJobsRead,
        Self::LaneJobRead,
        Self::LaneJobStop,
        Self::LaneSeat,
        Self::LaneState,
        Self::LaneRenewal,
        Self::LaneRetune,
        Self::LaneEffortRetune,
        Self::LaneFastRetune,
        Self::TeamConfigure,
        Self::TeamArrange,
        Self::LaneRecall,
        Self::ProofRecognize,
        Self::UpChange,
    ];

    /// The wire spelling of this command.
    ///
    /// The outer partition is exhaustive, so a new variant must state which
    /// closed registry section owns its spelling. The section matches remain
    /// short enough to review as bounded tables.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutboxSubmit => "outbox::submit",
            Self::OutboxRead => "outbox::read",
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
            | Self::TaskDepends
            | Self::TaskTarget
            | Self::TaskJoin
            | Self::TaskInvite
            | Self::TaskDone
            | Self::TaskReview => self.board_spelling(),
            Self::ClaimWin
            | Self::ClaimRefresh
            | Self::ClaimFree
            | Self::ClaimLapse
            | Self::SteerSend
            | Self::SteerInbox
            | Self::SteerResend
            | Self::SteerRecover
            | Self::SteerCancel
            | Self::SteerLineControl
            | Self::QuoteRead
            | Self::TopicRead
            | Self::TopicEdit
            | Self::ConsentRead
            | Self::ConsentPolicy
            | Self::ConsentDecide => self.coordination_spelling(),
            Self::TrustPend
            | Self::TrustGrant
            | Self::TrustRevoke
            | Self::TrustShow
            | Self::LaneRead
            | Self::LaneJobsRead
            | Self::LaneJobRead
            | Self::LaneJobStop
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
            | Self::UpChange => self.runtime_spelling(),
            Self::PrincipalDirectoryRead
            | Self::PrincipalRead
            | Self::PrincipalAdmit
            | Self::PrincipalEdit
            | Self::PrincipalGrant
            | Self::PrincipalRevoke
            | Self::PrincipalDeauthorize => self.principal_spelling(),
        }
    }

    const fn board_spelling(self) -> &'static str {
        match self {
            Self::BoardRead => "board::read",
            Self::TaskDetail => "task::detail",
            Self::TaskRelations => "task::relations",
            Self::TaskTimeline => "task::timeline",
            Self::BoardGraph => "board::graph",
            Self::BoardHandout => "board::handout",
            Self::DocLedger => "doc::ledger",
            Self::RoomScore
            | Self::RoomPost
            | Self::RoomConversationText
            | Self::RoomLeave
            | Self::RoomContribute
            | Self::RoomContributionRead => self.room_spelling(),
            Self::PreparationDecide | Self::OutputAdopt => self.preparation_spelling(),
            Self::TaskAdd
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
            | Self::TaskReview => self.task_spelling(),
            _ => panic!("command spelling partition is closed"),
        }
    }
    const fn task_spelling(self) -> &'static str {
        match self {
            Self::TaskAdd => "task::add",
            Self::TaskOopsAdd => "task::oops-add",
            Self::TaskSweep => "task::sweep",
            Self::TaskEdit => "task::edit",
            Self::TaskDelete => "task::delete",
            Self::TaskClose => "task::close",
            Self::TaskConclude => "task::conclude",
            Self::TaskNote => "task::note",
            Self::TaskDefer => "task::defer",
            Self::TaskWake => "task::wake",
            Self::TaskFile => "task::file",
            Self::TaskPlace => "task::place",
            Self::TaskMedia => "task::media",
            Self::TaskDepends => "task::depends",
            Self::TaskTarget => "task::target",
            Self::TaskJoin => "task::join",
            Self::TaskInvite => "task::invite",
            Self::TaskDone => "task::done",
            Self::TaskReview => "task::review",
            _ => panic!("task command spelling partition is closed"),
        }
    }
    const fn room_spelling(self) -> &'static str {
        match self {
            Self::RoomScore => "room::score",
            Self::RoomPost => "room::post",
            Self::RoomConversationText => "room::conversation-text",
            Self::RoomLeave => "room::leave",
            Self::RoomContribute => "room::contribute",
            Self::RoomContributionRead => "room::contribution-read",
            _ => panic!("room command spelling partition is closed"),
        }
    }
    const fn preparation_spelling(self) -> &'static str {
        match self {
            Self::PreparationDecide => "preparation::decide",
            Self::OutputAdopt => "preparation::adopt",
            _ => panic!("preparation command spelling partition is closed"),
        }
    }

    const fn coordination_spelling(self) -> &'static str {
        match self {
            Self::ClaimWin => "claim::win",
            Self::ClaimRefresh => "claim::refresh",
            Self::ClaimFree => "claim::free",
            Self::ClaimLapse => "claim::lapse",
            Self::SteerSend => "steer::send",
            Self::SteerInbox => "steer::inbox",
            Self::SteerResend => "steer::resend",
            Self::SteerRecover => "steer::recover",
            Self::SteerCancel => "steer::cancel",
            Self::SteerLineControl => "steer::line-control",
            Self::QuoteRead => "quote::read",
            Self::TopicRead => "topic::read",
            Self::TopicEdit => "topic::edit",
            Self::ConsentRead => "consent::read",
            Self::ConsentPolicy => "consent::policy",
            Self::ConsentDecide => "consent::decide",
            _ => panic!("command spelling partition is closed"),
        }
    }

    const fn principal_spelling(self) -> &'static str {
        match self {
            Self::PrincipalRead => "principal::read",
            Self::PrincipalAdmit => "principal::admit",
            Self::PrincipalEdit => "principal::edit",
            Self::PrincipalGrant => "principal::grant",
            Self::PrincipalRevoke => "principal::revoke",
            Self::PrincipalDirectoryRead => "principal::directory-read",
            Self::PrincipalDeauthorize => "principal::deauthorize",
            _ => panic!("command spelling partition is closed"),
        }
    }

    const fn runtime_spelling(self) -> &'static str {
        match self {
            Self::TrustPend => "trust::pend",
            Self::TrustGrant => "trust::grant",
            Self::TrustRevoke => "trust::revoke",
            Self::TrustShow => "trust::show",
            Self::LaneRead => "lane::read",
            Self::LaneJobsRead => "lane::jobs-read",
            Self::LaneJobRead => "lane::job-read",
            Self::LaneJobStop => "lane::job-stop",
            Self::LaneSeat => "lane::seat",
            Self::LaneState => "lane::state",
            Self::LaneRenewal => "lane::renewal",
            Self::LaneRetune => "lane::retune",
            Self::LaneEffortRetune => "lane::effort-retune",
            Self::LaneFastRetune => "lane::fast-retune",
            Self::TeamConfigure => "team::configure",
            Self::TeamArrange => "team::arrange",
            Self::LaneRecall => "lane::recall",
            Self::RackRead => "rack::read",
            Self::GlobalSettingsRead => "settings::read",
            Self::GlobalSettingsSet => "settings::set",
            Self::VendorCatalog => "vendor::catalog",
            Self::ProofRecognize => "proof::recognize",
            Self::UpChange => "up::change",
            _ => panic!("command spelling partition is closed"),
        }
    }

    /// Whether this request reads one snapshot and commits no authority fact.
    #[must_use]
    pub const fn is_read(self) -> bool {
        matches!(
            self,
            Self::RoomScore
                | Self::OutboxRead
                | Self::BoardRead
                | Self::TaskDetail
                | Self::TaskRelations
                | Self::TaskTimeline
                | Self::BoardGraph
                | Self::BoardHandout
                | Self::DocLedger
                | Self::RackRead
                | Self::TrustShow
                | Self::GlobalSettingsRead
                | Self::VendorCatalog
                | Self::SteerInbox
                | Self::QuoteRead
                | Self::TopicRead
                | Self::ConsentRead
                | Self::ConsentPolicy
                | Self::LaneRead
                | Self::LaneJobsRead
                | Self::LaneJobRead
                | Self::RoomConversationText
                | Self::RoomContributionRead
                | Self::LaneRecall
                | Self::PrincipalRead
        )
    }
}

impl FromStr for CommandKind {
    type Err = CommandKindError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .find(|kind| kind.as_str() == text)
            .copied()
            .ok_or_else(|| CommandKindError(text.to_owned()))
    }
}

impl fmt::Display for CommandKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CommandKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CommandKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}
