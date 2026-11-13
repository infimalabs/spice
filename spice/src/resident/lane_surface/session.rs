//! Session-declaration proof, durable resume, and its refusals.

use listen::{Decoded, ResidueKind};

use super::*;

/// Why one occupant's session declaration was not admitted.
///
/// Reached by lane seating and by predecessor import, which carried a second
/// enum of its own until RECOVER-1kK7X4f2: the two mapped to the same seven
/// reasons through separate variants, so a reason could be reworded on one
/// path and left standing on the other.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::resident) enum SessionProofError {
    Missing(DeclarationWitness),
    Capture(String),
    CrossLane {
        expected: CanonicalUuid,
        observed: CanonicalUuid,
    },
    CrossOccupant {
        expected: CanonicalUuid,
        observed: CanonicalUuid,
    },
    WrongStream,
    Mismatched {
        expected: String,
        observed: String,
    },
    Duplicate {
        identity: String,
    },
}

impl SessionProofError {
    pub(in crate::resident) fn refusal(&self) -> Refusal {
        let (reason, expected, observed, detail) = match self {
            Self::Missing(witness) => (
                "lane::session-missing",
                Some("one current selected-stream session declaration".to_owned()),
                Some(witness.observation()),
                "retry with a vendor that emits its current initialization record",
            ),
            Self::Capture(detail) => (
                "lane::session-capture-failed",
                Some("one intact selected-stream capture".to_owned()),
                Some(detail.clone()),
                "repair the capture path and retry the lane launch",
            ),
            Self::CrossLane { expected, observed } => (
                "lane::session-cross-lane",
                Some(expected.to_string()),
                Some(observed.to_string()),
                "use only the declaration emitted by this lane's supervised occupant",
            ),
            Self::CrossOccupant { expected, observed } => (
                "lane::session-cross-occupant",
                Some(expected.to_string()),
                Some(observed.to_string()),
                "use only the declaration emitted by the current supervised occupant",
            ),
            Self::WrongStream => (
                "lane::session-wrong-stream",
                Some("the vendor-selected record stream".to_owned()),
                Some("another child stream".to_owned()),
                "declare the session on the selected JSONL stream",
            ),
            Self::Mismatched { expected, observed } => (
                "lane::session-mismatched",
                Some(expected.clone()),
                Some(observed.clone()),
                "resume the exact durable session identity",
            ),
            Self::Duplicate { identity } => (
                "lane::session-duplicate",
                Some("one declaration per occupant".to_owned()),
                Some(identity.clone()),
                "emit the initialization declaration exactly once",
            ),
        };
        Refusal::bare(
            wire::RefusalDiagnostic::new(
                reason.to_owned(),
                Some("session-declaration".to_owned()),
                expected,
                observed,
            )
            .with_detail(detail),
        )
    }
}

pub(in crate::resident) struct DeclarationState {
    pub(in crate::resident) reply: mpsc::SyncSender<Result<wire::LaneSession, SessionProofError>>,
    pub(in crate::resident) seen: Option<String>,
    pub(in crate::resident) witness: DeclarationWitness,
}

/// What the proof observer saw on the selected stream, when it saw no
/// declaration.
///
/// The refusal this feeds is a deadline elapsing, so it has nothing of its own
/// to report: without this it named only the record it wanted and never the
/// records it got, alone among the seven arms. A vendor whose bytes cannot be
/// read as records at all and a vendor whose records are read and never declare
/// are the same silence to the deadline and different faults to the operator.
///
/// The newest observation wins. An occupant that emitted a typed record and
/// then residue is reported by the residue, because the last thing it did is
/// the thing an operator is still looking at.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::resident) enum DeclarationWitness {
    /// No record reached the observer before the deadline.
    #[default]
    Silent,
    /// Records reached it, every one typed, none declaring a session.
    Typed,
    /// The newest record reached it as accounted residue, described here.
    Residue(String),
}

impl DeclarationWitness {
    /// Read one captured record as the observation a refusal would report.
    fn of(decoded: &Decoded) -> Self {
        let Some(residue) = decoded.residue() else {
            return Self::Typed;
        };
        // Named exhaustively so a decoder that learns a new residue kind has to
        // decide how an operator hears it rather than inheriting a stale phrase.
        Self::Residue(match &residue.kind {
            ResidueKind::Absent { field } => format!("one record carrying no {field}"),
            ResidueKind::Unknown { discriminator } => {
                format!("one record of unknown type {discriminator}")
            }
            ResidueKind::Malformed { field, expected } => {
                format!("one record whose {field} is not {expected}")
            }
            ResidueKind::Truncated => "one incomplete record".to_owned(),
            ResidueKind::VersionDrift { discriminator } => {
                format!("one record of type {discriminator} this decoder does not own")
            }
        })
    }

    /// The operator-facing observation this witness stands for.
    fn observation(&self) -> String {
        match self {
            Self::Silent => "no record on the selected stream".to_owned(),
            Self::Typed => "records on the selected stream, none declaring a session".to_owned(),
            Self::Residue(detail) => detail.clone(),
        }
    }
}

/// The one observer that admits a session declaration.
///
/// Seating and predecessor import both install this, and its seven refusals
/// are proven against it in `lane_recovery_tests.rs`, which is where import
/// held the copy this replaced. The proofs stayed there rather than moving
/// beside the type, because the silent reporter they drive is a fixture that
/// file already needs for its own launch tests.
pub(in crate::resident) struct DeclarationReporter {
    pub(in crate::resident) inner: Arc<dyn CaptureReporter>,
    pub(in crate::resident) lane: CanonicalUuid,
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) worktree: String,
    pub(in crate::resident) vendor: wire::Name,
    pub(in crate::resident) model: String,
    pub(in crate::resident) effort: String,
    pub(in crate::resident) fast_mode: bool,
    pub(in crate::resident) git: String,
    pub(in crate::resident) rtk: wire::body::RtkHealth,
    pub(in crate::resident) epoch: CanonicalUuid,
    pub(in crate::resident) stream: vendor::RecordStream,
    pub(in crate::resident) expected: Option<String>,
    pub(in crate::resident) state: Arc<Mutex<DeclarationState>>,
}

/// The one proof a launch hands its awaiter.
///
/// The receiver and the observer state travel together because the deadline
/// arm of [`await_session`] needs both: nothing was sent, so what the occupant
/// emitted can only be read back from the state the reporter wrote it into.
/// Bundled rather than passed as a second argument so no caller can await one
/// launch's declarations against another's observations.
pub(in crate::resident) struct SessionProof {
    pub(in crate::resident) session: mpsc::Receiver<Result<wire::LaneSession, SessionProofError>>,
    pub(in crate::resident) state: Arc<Mutex<DeclarationState>>,
    pub(in crate::resident) horizon: Duration,
}

/// Select fresh launch for never-started lanes and proven resume otherwise.
pub(in crate::resident) fn launch_resume(
    paths: &RuntimePaths,
    row: &wire::LaneResidency,
) -> CommandResult<vendor::Resume> {
    if row.session.is_none() && row.lineage.is_empty() {
        Ok(vendor::Resume::Fresh)
    } else {
        resume_session(paths, row)
    }
}

impl DeclarationReporter {
    fn reject(&self, state: &mut DeclarationState, error: SessionProofError) -> CaptureReportError {
        let _sent = state.reply.try_send(Err(error.clone()));
        CaptureReportError::new(format!("session declaration refused: {error:?}"))
    }
}

impl CaptureReporter for DeclarationReporter {
    fn record(&self, record: CaptureRecord) -> Result<(), CaptureReportError> {
        let Some(declaration) = record.declaration.as_ref() else {
            // Only the selected stream is witnessed, for the same reason a
            // declaration off it is refused below. An occupant that writes
            // nothing but diagnostics to the other stream has said nothing on
            // the one this deadline waits on, and calling that "records, none
            // declaring" would replace one wrong observation with another.
            if record.stream == self.stream {
                self.state
                    .lock()
                    .map_err(|_| CaptureReportError::new("session proof lock is poisoned"))?
                    .witness = DeclarationWitness::of(&record.decoded);
            }
            return self.inner.record(record);
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| CaptureReportError::new("session proof lock is poisoned"))?;
        // The offending declaration is the one named, as in every other arm
        // here and in the second declaration `await_session` refuses: an
        // operator told to emit the record once needs the one that was too
        // many, not the one already admitted. A resumed vendor session can
        // legitimately re-announce the identity it already proved -- that
        // repeat carries no new authority and is not the operator error this
        // guards against, so only a second declaration naming a *different*
        // identity is the duplicate this refuses.
        if let Some(seen) = state.seen.clone() {
            let identity = declaration.session.as_str().to_owned();
            if identity != seen {
                return Err(self.reject(&mut state, SessionProofError::Duplicate { identity }));
            }
            return self.inner.record(record);
        }
        if record.lane != self.lane {
            return Err(self.reject(
                &mut state,
                SessionProofError::CrossLane {
                    expected: self.lane,
                    observed: record.lane,
                },
            ));
        }
        if record.occupant != self.occupant {
            return Err(self.reject(
                &mut state,
                SessionProofError::CrossOccupant {
                    expected: self.occupant,
                    observed: record.occupant,
                },
            ));
        }
        if record.stream != self.stream {
            return Err(self.reject(&mut state, SessionProofError::WrongStream));
        }
        let identity = declaration.session.as_str().to_owned();
        if let Some(expected) = &self.expected
            && expected != &identity
        {
            return Err(self.reject(
                &mut state,
                SessionProofError::Mismatched {
                    expected: expected.clone(),
                    observed: identity,
                },
            ));
        }
        let session = wire::LaneSession {
            lane: self.lane,
            occupant: self.occupant,
            worktree: self.worktree.clone(),
            vendor: self.vendor.clone(),
            launch: wire::body::LaneLaunchIdentity::new(self.model.clone(), self.effort.clone())
                .with_fast_mode(self.fast_mode)
                .with_git(self.git.clone())
                .with_rtk(self.rtk.clone()),
            epoch: self.epoch,
            stream: record_stream(self.stream).to_owned(),
            identity: identity.clone(),
            locus: wire::TapeLocus {
                member: declaration.locus.member,
                offset: declaration.locus.offset,
                length: declaration.locus.length,
            },
        };
        self.inner.record(record)?;
        state.seen = Some(identity);
        state.reply.try_send(Ok(session)).map_err(|error| {
            CaptureReportError::new(format!(
                "session proof receiver refused declaration: {error}"
            ))
        })
    }

    fn stage(&self) -> &'static str {
        self.inner.stage()
    }

    fn failed(&self, failure: CaptureFailure) {
        if let Ok(state) = self.state.lock() {
            let _sent = state
                .reply
                .try_send(Err(SessionProofError::Capture(failure.failure.to_string())));
        }
        self.inner.failed(failure);
    }

    fn exited(&self, exit: CaptureExit) {
        self.inner.exited(exit);
    }
}

pub(in crate::resident) fn session_fact(
    session: &wire::LaneSession,
    source: CanonicalUuid,
) -> wire::body::LaneSessionDeclared {
    wire::body::LaneSessionDeclared {
        lane: session.lane,
        occupant: session.occupant,
        worktree: session.worktree.clone(),
        vendor: session.vendor.clone(),
        launch: session.launch.clone(),
        epoch: session.epoch,
        stream: session.stream.clone(),
        source,
        identity: session.identity.clone(),
        member: session.locus.member,
        offset: session.locus.offset,
        length: session.locus.length,
    }
}

pub(in crate::resident) fn session_source(
    paths: &RuntimePaths,
    session: &wire::LaneSession,
) -> CommandResult<CanonicalUuid> {
    let root = paths.lane_tape(session.lane).join(session.vendor.as_str());
    let source = tape::Tape::open_existing(&root)
        .and_then(|tape| tape.source_id())
        .map_err(|error| {
            store_fault(
                "lane::session-source-unavailable",
                &format!("{}: {error}", root.display()),
            )
        })?;
    source.as_str().parse().map_err(|error| {
        store_fault(
            "lane::session-source-invalid",
            &format!("{}: {error}", source.as_str()),
        )
    })
}

pub(in crate::resident) fn record_stream(stream: vendor::RecordStream) -> &'static str {
    match stream {
        vendor::RecordStream::Stdout => "stdout",
        vendor::RecordStream::Stderr => "stderr",
    }
}

pub(in crate::resident) fn await_session(
    proof: SessionProof,
) -> Result<wire::LaneSession, SessionProofError> {
    let SessionProof {
        session,
        state,
        horizon,
    } = proof;
    match session.recv_timeout(horizon) {
        Ok(Ok(declared)) => match session.recv_timeout(SESSION_SETTLE) {
            Ok(Err(error)) => Err(error),
            Ok(Ok(duplicate)) => Err(SessionProofError::Duplicate {
                identity: duplicate.identity,
            }),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(declared),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(declared),
        },
        Ok(Err(error)) => Err(error),
        Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
            // A poisoned lock is itself a silence about the occupant: the
            // observer that would have written the witness is the thread that
            // panicked holding it.
            Err(SessionProofError::Missing(
                state
                    .lock()
                    .map(|state| state.witness.clone())
                    .unwrap_or_default(),
            ))
        }
    }
}

/// Resume one lane onto the session its durable head declares.
///
/// Reached by the seat and recall surfaces and by predecessor import, which
/// held a second copy of this chain until RECOVER-1kK7Bxbt: one durable
/// condition now refuses under one reason whichever surface asks.
pub(in crate::resident) fn resume_session(
    paths: &RuntimePaths,
    row: &wire::LaneResidency,
) -> CommandResult<vendor::Resume> {
    let session = validate_session_head(row)?;
    let vendor = session_vendor(&row.vendor)?;
    let identity = prove_session_tape(paths, row, session, vendor)?;
    Ok(vendor::Resume::Session(identity))
}

/// Admit only a session head bound to this exact lane, worktree, vendor,
/// latest occupant, and selected stream.
pub(super) fn validate_session_head(
    row: &wire::LaneResidency,
) -> CommandResult<&wire::LaneSession> {
    let session = row.session.as_ref().ok_or_else(|| {
        session_head_refusal(
            "lane::session-authority-missing",
            Some("a session declared by this lane's occupant".to_owned()),
            Some("occupied lane that has not declared a session".to_owned()),
            "start this lane fresh; it has no declared session to resume",
        )
    })?;
    if session.lane != row.lane {
        return Err(session_head_refusal(
            "lane::session-cross-lane",
            Some(row.lane.to_string()),
            Some(session.lane.to_string()),
            "restore the session head bound to this exact lane",
        ));
    }
    if session.worktree != row.worktree {
        return Err(session_head_refusal(
            "lane::session-wrong-worktree",
            Some(row.worktree.clone()),
            Some(session.worktree.clone()),
            "restore the session head bound to this exact worktree",
        ));
    }
    if session.vendor != row.vendor {
        return Err(session_head_refusal(
            "lane::session-cross-vendor",
            Some(row.vendor.to_string()),
            Some(session.vendor.to_string()),
            "restore the session head admitted by this lane's vendor",
        ));
    }
    let expected_occupant = row.lineage.last().copied().ok_or_else(|| {
        session_head_refusal(
            "lane::session-lineage-missing",
            Some("the occupant that declared the session".to_owned()),
            None,
            "repair the durable lane lineage before resuming",
        )
    })?;
    if session.occupant != expected_occupant {
        return Err(session_head_refusal(
            "lane::session-cross-occupant",
            Some(expected_occupant.to_string()),
            Some(session.occupant.to_string()),
            "restore the declaration made by the lane's latest occupant",
        ));
    }
    if session.stream != "stdout" {
        return Err(session_head_refusal(
            "lane::session-wrong-stream",
            Some("stdout".to_owned()),
            Some(session.stream.clone()),
            "restore the locus from the vendor-selected JSONL stream",
        ));
    }
    Ok(session)
}

/// Select the declaration dialect owned by this lane's vendor.
fn session_vendor(vendor: &wire::Name) -> CommandResult<&'static spice::extension::Vendor> {
    let Some(declared) = super::super::ingest_vendor(vendor) else {
        let expected = spice::extension::VENDORS
            .iter()
            .map(|entry| entry.vendor.name())
            .collect::<Vec<_>>()
            .join(" or ");
        return Err(session_head_refusal(
            "lane::session-vendor-unsupported",
            Some(expected),
            Some(vendor.as_str().to_owned()),
            "resume a lane recorded under a vendor this build declares",
        ));
    };
    Ok(declared)
}

/// Prove the durable tape still holds the exact declaration the head names.
fn prove_session_tape(
    paths: &RuntimePaths,
    row: &wire::LaneResidency,
    session: &wire::LaneSession,
    vendor: &spice::extension::Vendor,
) -> CommandResult<vendor::SessionId> {
    let tape = tape::Tape::open_existing(paths.lane_tape(row.lane).join(row.vendor.as_str()))
        .map_err(|error| {
            session_head_refusal(
                "lane::session-tape-moved",
                Some("the exact durable lane/vendor tape".to_owned()),
                Some(error.to_string()),
                "restore the preserved tape before resuming this lane",
            )
        })?;
    let locus = listen::Locus::new(
        session.locus.member,
        session.locus.offset,
        session.locus.length,
    );
    let bytes = tape.read(locus).map_err(|error| {
        session_head_refusal(
            "lane::session-tape-moved",
            Some(locus.to_string()),
            Some(error.to_string()),
            "restore the exact declaration bytes before resuming this lane",
        )
    })?;
    // The one error a decode can raise is a locus whose length disagrees with
    // the bytes handed to it, and the read above answers this exact locus or
    // refuses, so a disagreement here is a broken tape contract rather than a
    // moved tape. A record the dialect cannot read is not an error at all: it
    // decodes to residue, declares nothing, and is refused below with every
    // other locus that no longer holds the declaration the head names.
    let decoded = vendor
        .decode(&bytes, locus)
        .expect("a tape read answers its exact locus length");
    let declaration = vendor::SessionDeclaration::admit(&decoded).map_err(|error| {
        session_head_refusal(
            "lane::session-identity-invalid",
            Some("one nonempty vendor session identity".to_owned()),
            Some(error.to_string()),
            "repair the durable session authority before resuming",
        )
    })?;
    let declared = declaration.as_ref().map(|row| row.session.as_str());
    if declared != Some(session.identity.as_str()) {
        return Err(session_head_refusal(
            "lane::session-tape-moved",
            Some(session.identity.clone()),
            declared.map(str::to_owned),
            "restore the declaration record named by the durable locus",
        ));
    }
    let identity = vendor::SessionId::new(session.identity.clone()).map_err(|error| {
        session_head_refusal(
            "lane::session-identity-invalid",
            Some("one nonempty vendor session identity".to_owned()),
            Some(error.to_string()),
            "repair the durable session authority before resuming",
        )
    })?;
    Ok(identity)
}

/// Refuse one resume whose durable session head no longer holds.
fn session_head_refusal(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("lane-session-head".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    ))
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
