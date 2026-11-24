//! The single contract-bound command mapping before transport authentication.

use wire::TrustState;

use super::*;

impl MappedCommand {
    pub(super) fn selected(
        contract: &Contract,
        selected: SelectedCommand<'_>,
    ) -> ClientResult<Self> {
        match selected {
            SelectedCommand::Down => Self::down(contract),
            SelectedCommand::ConsentRead => Self::consent_read(contract),
            SelectedCommand::ConsentPolicy => Self::consent_policy(contract),
            SelectedCommand::ConsentDecide {
                request,
                answer,
                expected,
            } => Self::consent_decide(contract, request, answer, expected),
            SelectedCommand::ProofRecognize { manifest, head } => {
                Self::proof_recognize(contract, manifest, head)
            }
            SelectedCommand::LaneRead => Self::lane_read(contract),
            SelectedCommand::GlobalSettingsRead => Self::new(
                contract,
                &["settings"],
                CommandKind::GlobalSettingsRead,
                Vec::new(),
                &wire::intent::GlobalSettingsRead {},
            ),
            SelectedCommand::GlobalSettingsSet { body } => Self::new(
                contract,
                &["settings", "fast"],
                CommandKind::GlobalSettingsSet,
                Vec::new(),
                body,
            ),
            SelectedCommand::TeamConfigure { body } => Self::new(
                contract,
                contract.path,
                CommandKind::TeamConfigure,
                Vec::new(),
                body,
            ),
            SelectedCommand::LaneSeat { body } => Self::lane_seat(contract, body),
            SelectedCommand::LaneState { body } => Self::lane_state(contract, body),
            SelectedCommand::LaneRetune { body } => Self::lane_retune(contract, body),
            SelectedCommand::LaneEffortRetune { body } => Self::lane_effort_retune(contract, body),
            SelectedCommand::LaneFastRetune { body } => Self::lane_fast_retune(contract, body),
            SelectedCommand::LaneRenewal { body } => Self::lane_renewal(contract, body),
            SelectedCommand::LaneRecall { body } => Self::lane_recall(contract, body),
            SelectedCommand::QuoteRead { body } => Self::quote_read(contract, body),
            SelectedCommand::TopicRead { body } => Self::topic_read(contract, body),
            SelectedCommand::TopicEdit { body } => {
                let path = match &body.change {
                    wire::topic::TopicChange::Move { .. } => &["topic", "move"],
                    wire::topic::TopicChange::Describe { .. } => &["topic", "describe"],
                };
                Self::new(contract, path, CommandKind::TopicEdit, Vec::new(), body)
            }
            SelectedCommand::SteerCancel { directive } => Self::steer_cancel(contract, directive),
            SelectedCommand::SteerResend { directive } => Self::steer_resend(contract, directive),
            SelectedCommand::SteerRecover { directive } => Self::steer_recover(contract, directive),
            SelectedCommand::SteerSend { .. } | SelectedCommand::SteerInbox { .. } => {
                Err(protocol_fault(
                    "command::mapping-unsupported",
                    "steering preflight must select its authenticated mapping path",
                ))
            }
            SelectedCommand::TrustPend { digest, worktree } => Self::trust_rule(
                contract,
                digest,
                Some(worktree),
                TrustState::Pended,
                CommandKind::TrustPend,
            ),
            SelectedCommand::TrustGrant { digest } => Self::trust_rule(
                contract,
                digest,
                None,
                TrustState::Pended,
                CommandKind::TrustGrant,
            ),
            SelectedCommand::TrustRevoke { digest, standing } => {
                Self::trust_rule(contract, digest, None, standing, CommandKind::TrustRevoke)
            }
        }
    }

    fn down(contract: &Contract) -> ClientResult<Self> {
        Self::new(
            contract,
            &["down"],
            CommandKind::UpChange,
            vec![Witness::Transition {
                from: wire::ResidentState::Seated,
                to: wire::ResidentState::Dark,
            }],
            &wire::intent::UpChange {
                state: wire::ResidentState::Dark,
            },
        )
    }

    fn consent_read(contract: &Contract) -> ClientResult<Self> {
        Self::new(
            contract,
            &["consent"],
            CommandKind::ConsentRead,
            Vec::new(),
            &wire::intent::ConsentRead {},
        )
    }

    fn consent_policy(contract: &Contract) -> ClientResult<Self> {
        Self::new(
            contract,
            &["consent", "policy"],
            CommandKind::ConsentPolicy,
            Vec::new(),
            &wire::intent::ConsentPolicy {},
        )
    }

    fn consent_decide(
        contract: &Contract,
        request: CanonicalUuid,
        answer: wire::Answer,
        expected: Vec<Witness>,
    ) -> ClientResult<Self> {
        Self::new(
            contract,
            contract.path,
            CommandKind::ConsentDecide,
            expected,
            &wire::intent::ConsentDecide { request, answer },
        )
    }

    fn proof_recognize(
        contract: &Contract,
        manifest: &proof::PublishedManifest,
        head: wire::Seq,
    ) -> ClientResult<Self> {
        Self::new(
            contract,
            &["release", "seal"],
            CommandKind::ProofRecognize,
            vec![Witness::Observation {
                key: manifest.digest.clone(),
                head,
            }],
            &wire::intent::Recognize {
                key: manifest.digest.clone(),
            },
        )
    }

    fn lane_read(contract: &Contract) -> ClientResult<Self> {
        Self::new(
            contract,
            &["lane"],
            CommandKind::LaneRead,
            Vec::new(),
            &wire::intent::LaneRead {},
        )
    }

    fn lane_seat(contract: &Contract, body: &wire::intent::LaneSeat) -> ClientResult<Self> {
        Self::new(
            contract,
            &["lane", "add"],
            CommandKind::LaneSeat,
            Vec::new(),
            body,
        )
    }

    fn lane_state(contract: &Contract, body: &wire::intent::LaneState) -> ClientResult<Self> {
        Self::new(
            contract,
            contract.path,
            CommandKind::LaneState,
            Vec::new(),
            body,
        )
    }

    fn lane_retune(contract: &Contract, body: &wire::intent::LaneRetune) -> ClientResult<Self> {
        Self::new(
            contract,
            &["lane", "model"],
            CommandKind::LaneRetune,
            Vec::new(),
            body,
        )
    }
    fn lane_effort_retune(
        contract: &Contract,
        body: &wire::intent::LaneEffortRetune,
    ) -> ClientResult<Self> {
        Self::new(
            contract,
            &["lane", "effort"],
            CommandKind::LaneEffortRetune,
            Vec::new(),
            body,
        )
    }
    fn lane_fast_retune(
        contract: &Contract,
        body: &wire::intent::LaneFastRetune,
    ) -> ClientResult<Self> {
        Self::new(
            contract,
            &["lane", "fast"],
            CommandKind::LaneFastRetune,
            Vec::new(),
            body,
        )
    }
    fn lane_renewal(contract: &Contract, body: &wire::intent::LaneRenewal) -> ClientResult<Self> {
        Self::new(
            contract,
            &["lane", "renew"],
            CommandKind::LaneRenewal,
            Vec::new(),
            body,
        )
    }

    fn lane_recall(contract: &Contract, body: &wire::intent::LaneRecall) -> ClientResult<Self> {
        Self::new(
            contract,
            &["recall"],
            CommandKind::LaneRecall,
            Vec::new(),
            body,
        )
    }

    fn quote_read(contract: &Contract, body: &wire::intent::QuoteRead) -> ClientResult<Self> {
        Self::new(
            contract,
            &["quote"],
            CommandKind::QuoteRead,
            Vec::new(),
            body,
        )
    }

    fn topic_read(contract: &Contract, body: &wire::topic::TopicRead) -> ClientResult<Self> {
        let path: &[&str] = match body {
            wire::topic::TopicRead::Preview { .. } => &["topic", "preview"],
            wire::topic::TopicRead::Show { .. } => &["topic", "show"],
        };
        Self::new(contract, path, CommandKind::TopicRead, Vec::new(), body)
    }

    fn steer_cancel(contract: &Contract, directive: CanonicalUuid) -> ClientResult<Self> {
        Self::new(
            contract,
            &["steer", "cancel"],
            CommandKind::SteerCancel,
            vec![Witness::Directive {
                steer: directive,
                disposition: Disposition::Live,
            }],
            &wire::intent::DirectiveRef { directive },
        )
    }

    fn steer_recover(contract: &Contract, directive: CanonicalUuid) -> ClientResult<Self> {
        let request = wire::intent::DirectiveRef { directive };
        Self::new(
            contract,
            &["steer", "recover"],
            CommandKind::SteerRecover,
            vec![request.recovery_witness()],
            &request,
        )
    }

    fn steer_resend(contract: &Contract, directive: CanonicalUuid) -> ClientResult<Self> {
        Self::new(
            contract,
            &["steer", "resend"],
            CommandKind::SteerResend,
            vec![Witness::Directive {
                steer: directive,
                disposition: Disposition::Live,
            }],
            &wire::intent::DirectiveRef { directive },
        )
    }

    fn trust_rule(
        contract: &Contract,
        digest: &wire::Digest,
        worktree: Option<&str>,
        standing: TrustState,
        kind: CommandKind,
    ) -> ClientResult<Self> {
        let path = match kind {
            CommandKind::TrustPend => &["trust", "pend"][..],
            CommandKind::TrustGrant => &["trust", "grant"][..],
            CommandKind::TrustRevoke => &["trust", "revoke"][..],
            _ => unreachable!("trust mapping receives only pend, grant, or revoke"),
        };
        Self::new(
            contract,
            path,
            kind,
            vec![Witness::Trust {
                digest: digest.clone(),
                standing,
            }],
            &wire::intent::TrustRef {
                digest: digest.clone(),
                worktree: worktree.map(str::to_owned),
            },
        )
    }

    pub(super) fn new<B: Serialize>(
        contract: &Contract,
        mapped_path: &[&str],
        kind: CommandKind,
        expected: Vec<Witness>,
        body: &B,
    ) -> ClientResult<Self> {
        if contract.path != mapped_path
            || !contract.guards.contains(&Guard::Resident)
            || matches!(contract.principal, Principal::Local)
            || kind.is_read() != matches!(contract.journal, crate::cli::JournalAccess::Read)
        {
            return Err(protocol_fault(
                "command::mapping-unsupported",
                "selected CLI contract has no resident command mapping",
            ));
        }
        let encoded = wire::encode(body).map_err(|_| {
            protocol_fault(
                "command::body-unencodable",
                "selected CLI body cannot be represented by the current wire",
            )
        })?;
        kind.binding().body.decode(&encoded).map_err(|_| {
            protocol_fault(
                "command::body-incomplete",
                "selected CLI body does not match the command registry binding",
            )
        })?;
        let body = wire::decode(&encoded).map_err(|_| {
            protocol_fault(
                "command::body-unrepresentable",
                "validated command body did not retain its wire representation",
            )
        })?;
        let mapped = Self {
            id: CanonicalUuid::new(uuid::Uuid::new_v4()),
            kind,
            expected,
            body,
        };
        let preflight = mapped.envelope(CanonicalUuid::new(uuid::Uuid::new_v4()));
        preflight
            .witnessed()
            .map_err(|error| protocol_fault("command::witness-law", error.to_string()))?;
        Ok(mapped)
    }

    pub(super) fn envelope(&self, epoch: CanonicalUuid) -> Command<serde_json::Value> {
        Command {
            v: wire::WireVersion::CURRENT,
            id: self.id,
            epoch,
            kind: self.kind,
            expected: self.expected.clone(),
            body: self.body.clone(),
        }
    }

    /// Whether this envelope, refused by an ending resident, has a successor
    /// to serve it.
    ///
    /// Admission closes the moment a dark command is enqueued, so a `spice
    /// down` refused with not-admitting lost a race with another and the work
    /// it asked for is already under way. A successor is not what it asked to
    /// take dark, and "run it again once a resident is seated" is the opposite
    /// of what the operator wanted, so that refusal stands exactly as the
    /// resident gave it.
    pub(super) fn outlasts_restart(&self) -> bool {
        self.kind != CommandKind::UpChange
    }
}
