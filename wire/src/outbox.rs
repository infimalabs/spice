//! Authored outbound messages admitted from an exact captured host URI write.

use serde::{Deserialize, Serialize};

use crate::body::{PeerAdmission, PeerLineAction};
use crate::rooms::TranscriptHunk;
use crate::{CanonicalUuid, Digest, Disposition, OccupantAddress, Seq, TaskHandle};

/// Maximum complete authored body admitted by the outbox.
pub const BODY_BYTES_MAX: usize = 256 * 1024;
/// Maximum independently judged packets admitted in one body.
pub const PACKETS_MAX: usize = 256;

/// Host-owned provenance of the captured write, never model-authored identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carrier {
    /// Exact occupant whose supervisor captured the call.
    pub occupant: CanonicalUuid,
    /// Complete record the supervisor preserved before dispatch.
    pub locus: crate::TapeLocus,
}

/// Submit the exact prose carried by one authenticated host URI call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submit {
    /// Captured call authority supplied by the host.
    pub carrier: Carrier,
    /// Complete, untrimmed UTF-8 authored body.
    pub text: String,
}

/// Inspect a submission without acknowledging or publishing anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Read {
    /// Stable identity returned by submission.
    pub submission: CanonicalUuid,
    /// Exact occupant frozen by the capturing host, never resolved from the latest lane.
    pub occupant: CanonicalUuid,
}

/// The addressing an invitation write's URL carried in place of an authored
/// header: the lane the invitation reached and the task whose room records
/// the message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    /// Lane the committed invitation reached.
    pub recipient: CanonicalUuid,
    /// Task the inviter holds, as the invitation resolved it.
    pub task: TaskHandle,
}

/// A complete body was admitted before any of its packet effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    /// Deterministic repository-local identity: the occupant and body of an
    /// outbox write, or the occupant and captured record of an invitation.
    pub submission: CanonicalUuid,
    /// Authenticated authoring lane.
    pub lane: CanonicalUuid,
    /// Exact author, retained across restart but not inherited by successors.
    pub occupant: CanonicalUuid,
    /// Digest of the complete untrimmed body.
    pub digest: Digest,
    /// Canonical captured bytes from which the body is reconstructed.
    pub hunk: TranscriptHunk,
    /// Number of independently judged packet positions.
    pub packets: u32,
    /// Present when the body is an invitation's message: the whole body is
    /// then one message to this addressing, and no header inside it is read.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub invitation: Option<Invitation>,
}

/// One packet's immutable judgment, committed with its successful native effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judged {
    /// Submission owning this packet.
    pub submission: CanonicalUuid,
    /// Zero-based stable packet position within the complete body.
    pub packet: u32,
    /// Native result, never recomputed after the judgment commits.
    pub outcome: PacketOutcome,
}

/// What became of the response text one acknowledgment header carried for the
/// peer lane that sent the directive. Neither says anyone read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BodyFate {
    /// The reply journaled to the sending lane carries the text.
    Sent,
    /// Nothing carries the text to anyone; the authored source alone holds it.
    Unsent,
}

/// Durable endpoint named by one admitted line body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BranchTarget {
    /// Internal lane at its newest proved occupant address.
    Lane(OccupantAddress),
    /// External principal at its immutable short code.
    Principal(crate::principal::PrincipalCode),
}

impl std::fmt::Display for BranchTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lane(address) => address.fmt(formatter),
            Self::Principal(code) => code.fmt(formatter),
        }
    }
}

/// One peer input a packet's substantive body became.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    /// Exact recipient endpoint.
    pub to: BranchTarget,
    /// Key of the peer input that carries the body.
    pub key: String,
    /// Why a lane body was admitted; `null` when no peer admission fact exists.
    #[serde(deserialize_with = "crate::optional::required")]
    pub admission: Option<PeerAdmission>,
}

impl Branch {
    /// The input this body became and its one admission outcome.
    #[must_use]
    pub fn admitted(&self) -> String {
        format!(
            "{} {}",
            self.key,
            admitted(self.admission, "this lane", &self.to.to_string())
        )
    }
}

/// The one admission outcome of a substantive body sent `from` one lane `to`
/// another. Delivery without a peer-admission fact names only its destination.
#[must_use]
pub fn admitted(admission: Option<PeerAdmission>, from: &str, to: &str) -> String {
    match admission {
        Some(PeerAdmission::Active) => format!("delivered to active recipient {to}"),
        Some(PeerAdmission::Cohort) => format!("admitted to the exact committed start of {to}"),
        Some(PeerAdmission::SameRound) => format!("admitted same-round to {to}"),
        Some(PeerAdmission::FirstReturn) => format!("admitted as the exact first return to {to}"),
        Some(PeerAdmission::PairToken(spend)) => format!(
            "admitted to {to} by outside-round pair token ({from} to {to}: {} left)",
            spend.remaining
        ),
        None => format!("admitted to {to}"),
    }
}

/// What became of the substantive text one keyed control carried for the
/// other lane of its line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineBody {
    /// One peer input carries it.
    Admitted(Branch),
    /// It was refused before any peer input existed; the authored source
    /// alone holds it.
    Refused {
        /// Native stable reason.
        reason: String,
    },
}

/// What one keyed control did on its peer line, apart from the text it carried.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineEffect {
    /// Key of the input that opened the line.
    pub root: String,
    /// The transition the control recorded; absent when the line had already
    /// closed, so the key retired and the line did not move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<PeerLineAction>,
    /// What became of the substantive text the control carried; absent when
    /// it carried none. The text of a seen reaction is not a body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<LineBody>,
}

impl LineEffect {
    /// The control's effect on `key`, then the fate of its body.
    #[must_use]
    pub fn effect(&self, key: &str) -> String {
        let (control, carried, missing, owed) = match self.action {
            None => (
                "acknowledged; line already closed",
                "body",
                "no body delivered",
                "",
            ),
            Some(PeerLineAction::Reaction) => ("seen", "", "", ""),
            Some(PeerLineAction::Continue) => {
                ("acknowledged", "continuation", "continuation unsent", "")
            }
            Some(PeerLineAction::Finish | PeerLineAction::AutomaticFinish) => (
                "closed",
                "final body",
                "no final body delivered",
                ", still owed a seen after the close",
            ),
            Some(PeerLineAction::Decline) => (
                "declined",
                "reason",
                "no reason delivered",
                ", still owed a seen after the close",
            ),
            Some(PeerLineAction::Unanswered) => {
                ("ended unanswered", "body", "no body delivered", "")
            }
        };
        let control = format!("{key} on line {}: {control}", self.root);
        match &self.body {
            None => control,
            Some(LineBody::Admitted(branch)) => {
                format!("{control}; {carried} {}{owed}", branch.admitted())
            }
            Some(LineBody::Refused { reason })
                if self.action.is_none() && reason == "steer::line-closed" =>
            {
                format!(
                    "{control}; {missing} (refused before peer input: {reason}). A reply on this \
                     closed line reaches nobody. Build on this message in a new linked line with \
                     `SYN #{key}: BODY`"
                )
            }
            Some(LineBody::Refused { reason }) => {
                format!("{control}; {missing} (refused before peer input: {reason})")
            }
        }
    }
}

/// Native outcomes of one independently processed authored packet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PacketOutcome {
    /// An acknowledgment or reasoned refusal retired this directive.
    Acknowledged {
        /// Exact directive key.
        key: String,
        /// Recorded terminal disposition.
        disposition: Disposition,
        /// Fate of the header's response text toward the peer lane that sent
        /// the directive; absent when the header carried none or the directive
        /// has no peer sender.
        #[serde(deserialize_with = "crate::optional::required")]
        body: Option<BodyFate>,
        /// The key's effect on its peer line and the fate of the body it
        /// carried; absent when the directive is not a line input.
        #[serde(deserialize_with = "crate::optional::required")]
        line: Option<LineEffect>,
    },
    /// A later header in this same body supplies this key's decisive answer.
    Superseded {
        /// Exact directive key.
        key: String,
    },
    /// The packet's native directed and room effects were recorded.
    Messaged {
        /// Lanes whose seated recipients received the packet.
        lanes: Vec<CanonicalUuid>,
        /// Historical addresses whose vacant lanes queue the packet.
        queued: Vec<OccupantAddress>,
        /// Those `queued` addresses whose vacant lane the route's room
        /// relationship resumes to hear the packet.
        resuming: Vec<OccupantAddress>,
        /// Explicit redirections to proved successors.
        redirected: Vec<String>,
        /// Addresses that received nothing, with native reasons.
        unreached: Vec<String>,
        /// Routes refused because their pair had no start left.
        spent: Vec<String>,
        /// Rooms recording the exact authored hunk.
        rooms: Vec<TaskHandle>,
        /// Body lines delivered.
        #[serde(deserialize_with = "crate::optional::required")]
        lines: Option<usize>,
        /// The peer input each admitted route's body became, in route order.
        branches: Vec<Branch>,
    },
    /// This packet recorded no successful effect.
    Refused {
        /// Native stable reason.
        reason: String,
        /// Bounded factual detail.
        detail: String,
    },
}

impl PacketOutcome {
    /// One plain statement of what this packet did: each keyed control's
    /// effect before the fate of the text it carried, and each room record
    /// and peer branch of a message on its own.
    #[must_use]
    pub fn effect(&self) -> String {
        match self {
            Self::Acknowledged {
                key,
                line: Some(line),
                ..
            } => line.effect(key),
            Self::Acknowledged {
                key,
                disposition,
                body,
                line: None,
            } => {
                let control = match disposition {
                    Disposition::Acked => format!("{key}: acknowledged"),
                    other => format!("{key}: {}", other.as_str()),
                };
                match body {
                    None => control,
                    Some(BodyFate::Sent) => format!("{control}; its text went back to the sender"),
                    Some(BodyFate::Unsent) => format!("{control}; its text was not sent"),
                }
            }
            Self::Superseded { key } => {
                format!("{key}: superseded by a later header in the same text")
            }
            Self::Messaged {
                lanes,
                queued,
                unreached,
                spent,
                rooms,
                branches,
                ..
            } => {
                let mut parts = rooms
                    .iter()
                    .map(|room| format!("recorded on {room}"))
                    .collect::<Vec<_>>();
                let passive = !parts.is_empty()
                    && branches.is_empty()
                    && lanes.is_empty()
                    && queued.is_empty()
                    && unreached.is_empty()
                    && spent.is_empty();
                if passive {
                    parts.push("closed".to_owned());
                }
                if branches.is_empty() {
                    parts.extend(lanes.iter().map(|lane| format!("admitted to lane {lane}")));
                    parts.extend(queued.iter().map(|to| format!("admitted to {to}")));
                } else {
                    parts.extend(branches.iter().map(Branch::admitted));
                }
                parts.extend(
                    unreached
                        .iter()
                        .chain(spent)
                        .map(|refused| format!("refused before peer input: {refused}")),
                );
                parts.join("; ")
            }
            Self::Refused { reason, detail } => format!("refused ({reason}): {detail}"),
        }
    }
}

/// One packet judgment and the journal position proving it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketReceipt {
    /// Stable packet position.
    pub packet: u32,
    /// Durable judgment position.
    pub position: Seq,
    /// Original native outcome.
    pub outcome: PacketOutcome,
}

/// Why processing stopped after admission; accepted work was not rolled back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Interruption {
    /// Native admission of the next effect was refused.
    Refusal(crate::Refusal),
    /// A defect left the remaining packet judgments unresolved.
    Fault(crate::Fault),
}
/// Truthful inspection of one accepted authored message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Stable submission identity.
    pub submission: CanonicalUuid,
    /// Journal position admitting the body.
    pub accepted: Seq,
    /// Whether this write reused a submission; absent on side-effect-free
    /// inspection, which reports only the accepted record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused: Option<bool>,
    /// Authoring lane.
    pub lane: CanonicalUuid,
    /// Exact authoring occupant.
    pub occupant: CanonicalUuid,
    /// Exact tape-backed body on inspection; omitted from bounded write receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Side-effect-free URI for this receipt.
    pub inspect: String,
    /// Authenticated quote link for the authored body.
    pub quote: String,
    /// Original decided outcomes, in packet order.
    pub decided: Vec<PacketReceipt>,
    /// Packet positions still requiring a native judgment.
    pub unresolved: Vec<u32>,
    /// How to send text a decided outcome reports `unsent`; derived from the
    /// decided outcomes, never stored, and absent when none is unsent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// One plain statement per decided outcome, in packet order; derived
    /// from the decided outcomes and never stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<String>,
    /// A processing interruption, never a claim that accepted work rolled back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption: Option<Interruption>,
}

#[cfg(test)]
mod effect_tests {
    use super::*;
    use crate::body::PairSpend;

    fn address(birth: u64) -> OccupantAddress {
        OccupantAddress::from(Seq::new(birth).expect("fixture birth"))
    }

    fn messaged(rooms: &[&str], branches: Vec<Branch>, unreached: &[&str]) -> PacketOutcome {
        PacketOutcome::Messaged {
            lanes: Vec::new(),
            queued: branches
                .iter()
                .filter_map(|branch| match branch.to {
                    BranchTarget::Lane(address) => Some(address),
                    BranchTarget::Principal(_) => None,
                })
                .collect(),
            resuming: Vec::new(),
            redirected: Vec::new(),
            unreached: unreached.iter().map(|&reason| reason.to_owned()).collect(),
            spent: Vec::new(),
            rooms: rooms
                .iter()
                .map(|room| room.parse().expect("fixture room"))
                .collect(),
            lines: Some(1),
            branches,
        }
    }

    /// A task-only message is one passive record that closes itself; naming
    /// a peer as well reports the room, each admitted branch with its own
    /// cause, and each refused route, none of which reads as another.
    #[test]
    fn a_message_states_its_room_record_and_each_peer_branch_apart() {
        let room = "SPICE-dYL0cCWN9";
        assert_eq!(
            messaged(&[room], Vec::new(), &[]).effect(),
            format!("recorded on {room}; closed")
        );
        let (near, far) = (address(7), address(9));
        let frontier = "2026-11-11T06:00:00.000Z".parse().expect("fixture instant");
        let branch = |to, key: &str, admission| Branch {
            to: BranchTarget::Lane(to),
            key: key.to_owned(),
            admission: Some(admission),
        };
        let mixed = messaged(
            &[room],
            vec![
                branch(near, "dYMa", PeerAdmission::SameRound),
                branch(
                    far,
                    "dYMb",
                    PeerAdmission::PairToken(PairSpend {
                        remaining: 2,
                        frontier,
                    }),
                ),
            ],
            &["9zzz is vacant"],
        )
        .effect();
        assert_eq!(
            mixed,
            format!(
                "recorded on {room}; dYMa admitted same-round to {near}; dYMb admitted to {far} \
                 by outside-round pair token (this lane to {far}: 2 left); refused before peer \
                 input: 9zzz is vacant"
            )
        );
    }

    /// A decline whose reason is refused stays a recorded decline and never
    /// reads as a delivery.
    #[test]
    fn a_refused_reason_leaves_its_decline_recorded() {
        let declined = PacketOutcome::Acknowledged {
            key: "dYMc".to_owned(),
            disposition: Disposition::Declined,
            body: Some(BodyFate::Unsent),
            line: Some(LineEffect {
                root: "dYMa".to_owned(),
                action: Some(PeerLineAction::Decline),
                body: Some(LineBody::Refused {
                    reason: "steer::line-recipient-vacant".to_owned(),
                }),
            }),
        }
        .effect();
        assert_eq!(
            declined,
            "dYMc on line dYMa: declined; no reason delivered (refused before peer input: \
             steer::line-recipient-vacant)"
        );
    }
}
