//! Peer attribution on a pending directive: who sent it, which earlier
//! occupant of the receiving lane it named, and where it stands on its
//! conversation line.

use serde::{Deserialize, Serialize};

use crate::body::{PeerAdmission, PeerBodyKind, PeerLineAction};
use crate::ident::{CanonicalUuid, Instant};

/// The exact occupant that authored a SYN peer message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerSender {
    /// Sending occupant, as the message's creating fact records it.
    pub occupant: CanonicalUuid,
    /// That occupant's address, derived from its birth on the sending lane.
    ///
    /// Absent when the journal no longer proves exactly one such birth; an
    /// absent address is reported as missing, never replaced by another.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub address: Option<crate::OccupantAddress>,
}

/// The earlier occupant of a lane that a peer message named, beside the birth
/// of the occupant that heard it instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAddressed {
    /// The named occupant's address: its birth on the receiving lane.
    pub address: crate::OccupantAddress,
    /// Commit instant of the named occupant's birth.
    pub born: Instant,
    /// Commit instant of the receiving occupant's birth on the same lane.
    pub successor_born: Instant,
}

/// What its holder's keyed control did with one line input.
///
/// A control that only saw an input never answered it: an answer is the body
/// that control carried, admitted as an input of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LineAnswer {
    /// A control covered it and delivered no body: a nod, a bare terminal
    /// control, or a control whose body was refused.
    Seen {},
    /// The control that covered it delivered the input `key`.
    Answered {
        /// Key of the admitted body that answered it.
        key: String,
        /// What that body's header did on the line.
        body: PeerBodyKind,
    },
}

impl std::fmt::Display for LineAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Seen {} => formatter.write_str("seen, no body returned"),
            Self::Answered { key, body } => {
                write!(formatter, "answered by {key} ({})", body.as_str())
            }
        }
    }
}

/// Where one peer input stands on its conversation line, named by the keys
/// its two lanes act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineStanding {
    /// Key of the input that opened the line. An opening input names itself.
    pub root: String,
    /// Key of the opening input of the line this one was linked from.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parent: Option<String>,
    /// What the header that carried this body did on the line.
    pub body: PeerBodyKind,
    /// Keys of the exact inputs that header covered, in its order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered: Vec<String>,
    /// Why this body became input, when its admitting fact recorded the cause.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub admission: Option<PeerAdmission>,
    /// What its holder's control did with it; absent while no control has
    /// covered it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub answer: Option<LineAnswer>,
    /// The one terminal action that closed the line, when it is closed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub closed: Option<PeerLineAction>,
}

impl LineStanding {
    /// What this input is on its line, in the words every surface shares.
    #[must_use]
    pub fn standing(&self) -> String {
        let root = &self.root;
        let mut standing = match self.body {
            PeerBodyKind::Open => format!("opens line {root}"),
            PeerBodyKind::Continue => format!("continues line {root}"),
            PeerBodyKind::Final => format!("final body of line {root}"),
            PeerBodyKind::Decline => format!("reason line {root} was declined"),
        };
        if let Some(parent) = &self.parent {
            standing.push_str(&format!(", linked from line {parent}"));
        }
        if !self.covered.is_empty() {
            standing.push_str(&format!(", answering {}", self.covered.join(" ")));
        }
        if let Some(admission) = self.admission {
            standing.push_str(&format!("; {}", arrival(admission)));
        }
        if let Some(answer) = &self.answer {
            standing.push_str(&format!("; {answer}"));
        }
        if let Some(closed) = self.closed {
            standing.push_str(&format!("; line {}", terminal(closed)));
        }
        standing
    }

    /// This standing without what later line effects change: the identity a
    /// retained record of the input may keep.
    #[must_use]
    pub fn identity(self) -> Self {
        Self {
            answer: None,
            closed: None,
            ..self
        }
    }
}

/// How one admitted body reached its recipient lane, from either side.
fn arrival(admission: PeerAdmission) -> String {
    match admission {
        PeerAdmission::Active => "delivered to its active recipient".to_owned(),
        PeerAdmission::Cohort => "admitted to its recipient's exact committed start".to_owned(),
        PeerAdmission::SameRound => "admitted same-round".to_owned(),
        PeerAdmission::FirstReturn => "admitted as the line's exact first return".to_owned(),
        PeerAdmission::PairToken(spend) => format!(
            "admitted by outside-round pair token (sender to recipient: {} left)",
            spend.remaining
        ),
    }
}

/// How one terminal action left its line.
const fn terminal(action: PeerLineAction) -> &'static str {
    match action {
        PeerLineAction::Finish => "closed by FIN",
        PeerLineAction::Decline => "declined by NAK",
        PeerLineAction::AutomaticFinish => "closed at a clean checkpoint",
        PeerLineAction::Unanswered => {
            "ended unanswered: an input lapsed, was dead-lettered or was cancelled"
        }
        PeerLineAction::Reaction | PeerLineAction::Continue => "open",
    }
}

/// One open peer line a lane takes part in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerLine {
    /// Key of the input that opened the line.
    pub root: String,
    /// Key of the opening input of the line this one was linked from.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parent: Option<String>,
    /// Address that reaches the other lane of the line.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub counterpart: Option<crate::OccupantAddress>,
    /// Every substantive input admitted on the line, oldest first.
    pub inputs: Vec<PeerLineInput>,
    /// Whether the opening input's one exact first return was spent, by a
    /// nod or by a substantive answer.
    pub returned: bool,
    /// Whether every input is answered and both lanes have since crossed a
    /// clean checkpoint, so the line closes itself at its next reconciliation.
    /// A line any lane still owes an answer on is never eligible.
    pub checkpoint: bool,
}

impl PeerLine {
    /// Whether this lane still holds an input it has not answered.
    #[must_use]
    pub fn owed_here(&self) -> bool {
        self.inputs
            .iter()
            .any(|input| input.held && input.pending())
    }

    /// Whether the other lane still holds an input it has not answered.
    #[must_use]
    pub fn owed_there(&self) -> bool {
        self.inputs
            .iter()
            .any(|input| !input.held && input.pending())
    }

    /// Who the line waits on, in the words every surface shares.
    ///
    /// Only a pending input makes a lane the waiting party; how long an input
    /// has been quiet says nothing here.
    #[must_use]
    pub fn waiting(&self) -> &'static str {
        match (self.owed_here(), self.owed_there(), self.checkpoint) {
            (true, true, _) => "waits on both lanes",
            (true, false, _) => "waits on this lane",
            (false, true, _) => "waits on the other lane",
            (false, false, true) => "settled; closes at its next reconciliation",
            (false, false, false) => {
                "no input pending; open until a FIN, a NAK, or both lanes' clean checkpoints"
            }
        }
    }
}

impl std::fmt::Display for PeerLine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "line {}", self.root)?;
        if let Some(parent) = &self.parent {
            write!(formatter, " linked from line {parent}")?;
        }
        if let Some(counterpart) = self.counterpart {
            write!(formatter, " with {counterpart}")?;
        }
        write!(
            formatter,
            ": {}; first return {}",
            self.waiting(),
            if self.returned { "spent" } else { "unspent" }
        )?;
        for input in &self.inputs {
            write!(
                formatter,
                "\n  {} {} held {}, ",
                input.key,
                input.body.as_str(),
                if input.held { "here" } else { "there" },
            )?;
            match (&input.answer, input.disposition) {
                (Some(answer), _) => write!(formatter, "{answer}")?,
                (None, disposition) => formatter.write_str(unanswered(disposition))?,
            }
            if let Some(admission) = input.admission {
                write!(formatter, "; {}", arrival(admission))?;
            }
        }
        Ok(())
    }
}

/// One substantive input admitted on a peer line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerLineInput {
    /// Key its holding lane answers it by.
    pub key: String,
    /// Whether the lane this view describes holds the input; otherwise the
    /// other lane does.
    pub held: bool,
    /// What the header that carried this body did on the line.
    pub body: PeerBodyKind,
    /// What its holder's control did with it; absent while no control has
    /// covered it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub answer: Option<LineAnswer>,
    /// The retirement state its steering head sits in. An input no control
    /// covered that is no longer live names its terminal failure here.
    pub disposition: crate::Disposition,
    /// Why this body became input, when its admitting fact recorded the cause.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub admission: Option<PeerAdmission>,
}

impl PeerLineInput {
    /// Whether its holder still holds it unanswered.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.disposition == crate::Disposition::Live
    }
}

/// How an input no line control covered stands, by its retirement state.
///
/// Expiry is the residency's steering lifetime, a resource bound: it retires
/// the input and says nothing about its content, which the journal retains.
const fn unanswered(disposition: crate::Disposition) -> &'static str {
    match disposition {
        crate::Disposition::Live => "pending",
        crate::Disposition::Lapsed => {
            "lapsed unanswered when the residency's steering lifetime expired it; body retained"
        }
        crate::Disposition::DeadLettered => {
            "parked undelivered because its lane could not start; body retained for the operator"
        }
        crate::Disposition::Canceled => "canceled by its sender or the operator; body retained",
        crate::Disposition::Acked => "acknowledged outside its line",
        crate::Disposition::Declined => "declined outside its line",
    }
}
