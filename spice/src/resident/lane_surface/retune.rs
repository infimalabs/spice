//! Durable seat retuning and confirmed live OMP application.
//!
//! The seat remains launch authority across recovery, relaunch, and
//! succession. A running OMP occupant may additionally change immediately,
//! but only through its negotiated command stream and only after the exact
//! correlated response succeeds. Unsupported, ambiguous, refused, or
//! interrupted live changes leave the running identity untouched while the
//! durable seat selection still advances.

use super::super::settings_surface::global_fast_mode;
use super::super::supervisor::command::{LiveRetune, LiveRetuneResult};
use super::*;

/// One seat selection a retune command commits.
#[derive(Clone, Copy)]
pub(in crate::resident) enum Selection<'a> {
    Model(&'a LaneRetune),
    Effort(&'a LaneEffortRetune),
    Fast(&'a LaneFastRetune),
}

/// Vendors whose running occupant is OMP's acknowledged command stream.
fn live_omp(vendor: &str) -> bool {
    matches!(vendor, "omp" | "scripted")
}

/// The seat fact one selection journals, carrying the value it replaced.
enum SeatFact {
    Model(wire::body::LaneRetuned),
    Effort(wire::body::LaneEffortRetuned),
    Fast(wire::body::LaneFastRetuned),
}

impl Selection<'_> {
    fn lane(self) -> CanonicalUuid {
        match self {
            Self::Model(request) => request.lane,
            Self::Effort(request) => request.lane,
            Self::Fast(request) => request.lane,
        }
    }

    fn topology_revision(self) -> u64 {
        match self {
            Self::Model(request) => request.topology_revision,
            Self::Effort(request) => request.topology_revision,
            Self::Fast(request) => request.topology_revision,
        }
    }

    fn validate(self) -> Option<Outcome<CommandAnswer>> {
        match self {
            Self::Model(request) if model_malformed(request.model.as_ref()) => Some(lane_refusal(
                "lane::model-invalid",
                Some("one nonblank model spelling without surrounding whitespace".to_owned()),
                request.model.clone(),
                "trim the model spelling or clear it to follow the vendor configuration",
            )),
            Self::Effort(request)
                if request
                    .effort
                    .as_deref()
                    .is_some_and(|effort| effort.parse::<vendor::Effort>().is_err()) =>
            {
                Some(lane_refusal(
                    "lane::effort-invalid",
                    Some(closed_efforts()),
                    request.effort.clone(),
                    "choose low, medium, high, xhigh, or max; clear it to follow vendor configuration",
                ))
            }
            _ => None,
        }
    }

    fn replace_seat(self, row: &mut wire::LaneResidency) -> SeatFact {
        match self {
            Self::Model(request) => {
                row.forget_fast_refusal();
                SeatFact::Model(wire::body::LaneRetuned {
                    lane: request.lane,
                    from: std::mem::replace(&mut row.model, request.model.clone()),
                    model: request.model.clone(),
                })
            }
            Self::Effort(request) => SeatFact::Effort(wire::body::LaneEffortRetuned {
                lane: request.lane,
                from: std::mem::replace(&mut row.effort, request.effort.clone()),
                effort: request.effort.clone(),
            }),
            Self::Fast(request) => {
                if request.fast_mode {
                    row.forget_fast_refusal();
                }
                SeatFact::Fast(wire::body::LaneFastRetuned {
                    lane: request.lane,
                    from: std::mem::replace(&mut row.fast_mode, request.fast_mode),
                    fast_mode: request.fast_mode,
                })
            }
        }
    }

    /// The live model or effort change this selection asks of the running
    /// occupant, if any. Clears keep the running value; fast mode is
    /// resolved across its levels by [`fast_flips`] instead. A model spelling
    /// is resolved by the exact running supervisor before the durable seat
    /// changes, so absent and ambiguous names refuse without changing either
    /// identity. Losing that supervisor in the meantime leaves only the
    /// durable next-launch selection.
    fn live_request(
        self,
        row: &wire::LaneResidency,
        paths: &RuntimePaths,
    ) -> Result<Option<LiveRetune>, Outcome<CommandAnswer>> {
        let Some(occupant) = row.occupant else {
            return Ok(None);
        };
        if !live_omp(row.vendor.as_str()) {
            return Ok(None);
        }
        match self {
            Self::Effort(request) => Ok(request
                .effort
                .clone()
                .map(|effort| LiveRetune::Effort { effort })),
            Self::Model(request) => {
                let Some(spelling) = request.model.clone() else {
                    return Ok(None);
                };
                match super::super::supervisor::command::resolve_model(
                    paths,
                    row.lane,
                    occupant,
                    spelling.clone(),
                ) {
                    Ok(super::super::supervisor::command::ModelResolution::Resolved {
                        provider,
                        model,
                    }) => Ok(Some(LiveRetune::Model { provider, model })),
                    Ok(super::super::supervisor::command::ModelResolution::Refused { detail }) => {
                        Err(lane_refusal(
                            "lane::model-unresolved",
                            Some("one model the running occupant serves".to_owned()),
                            Some(spelling),
                            &detail,
                        ))
                    }
                    Err(_) => Ok(None),
                }
            }
            Self::Fast(_) => Ok(None),
        }
    }
}

/// One live fast-mode change a running OMP member owes after a write.
pub(super) struct FastFlip {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    live: LiveRetune,
}

/// The live fast-mode changes owed by one residency or global settings write.
///
/// `before_global` resolves `before`; `after` resolves with `after_global`.
/// A member owes one only when the write flips its effective fast mode,
/// resolved additively across lane, team and global flags, and it
/// runs an OMP launch with recorded identity. That identity can lag a change
/// still awaiting confirmation, so it is not compared.
pub(super) fn fast_flips(
    before: &wire::ResidencyHead,
    after: &wire::ResidencyHead,
    before_global: bool,
    after_global: bool,
) -> Vec<FastFlip> {
    after
        .lanes
        .iter()
        .filter_map(|row| {
            let enabled = after.fast_mode(row, after_global);
            let prior = before.lanes.iter().find(|prior| prior.lane == row.lane)?;
            if before.fast_mode(prior, before_global) == enabled || !live_omp(row.vendor.as_str()) {
                return None;
            }
            let occupant = row.occupant?;
            Some(FastFlip {
                lane: row.lane,
                occupant,
                live: LiveRetune::Fast { enabled },
            })
        })
        .collect()
}

/// Owe each running member whose effective fast mode a global setting change
/// to `after` flips its live change. `before` is the setting it replaced.
/// The setting is already durable.
pub(in crate::resident) fn owe_global_fast_flips(
    effects: &mut CommandEffects,
    command: &wire::AdmittedCommand,
    residency: &wire::ResidencyHead,
    before: bool,
    after: bool,
) {
    owe_fast_flips(
        effects,
        command,
        fast_flips(residency, residency, before, after),
    );
}

/// Owe each running member its fast-mode change. The write that owed them
/// is already durable.
pub(super) fn owe_fast_flips(
    effects: &mut CommandEffects,
    command: &wire::AdmittedCommand,
    flips: Vec<FastFlip>,
) {
    for flip in flips {
        effects.owe_live(LiveChange::new(
            command,
            flip.lane,
            flip.occupant,
            flip.live,
        ));
    }
}

/// One live change a running OMP occupant owes after its durable write.
///
/// Asking the occupant waits on the provider, so it runs outside the
/// resident loop; only a confirmed change returns to be journaled.
pub(in crate::resident) struct LiveChange {
    actor: wire::Actor,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    live: LiveRetune,
    /// The lane's effective fast mode a model change states for its new
    /// model: OMP's fast-mode command reaches only the current model's tier
    /// family, so the new family otherwise keeps whatever tier it launched
    /// with or a resumed session recorded.
    restate_fast: Option<bool>,
    /// The fast mode that statement left running and, when it did not apply
    /// cleanly, the words saying why.
    restated: Option<(LiveRetune, Option<String>)>,
}

impl LiveChange {
    fn new(
        command: &wire::AdmittedCommand,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        live: LiveRetune,
    ) -> Self {
        Self {
            actor: command.actor().clone(),
            lane,
            occupant,
            live,
            restate_fast: None,
            restated: None,
        }
    }

    /// Ask the occupant's supervisor for this change and wait for the
    /// provider's correlated answer; true only when the provider applied it.
    /// A model change then states the lane's fast mode for the new model.
    pub(in crate::resident) fn confirm(&mut self, paths: &RuntimePaths) -> bool {
        let ask = |live: LiveRetune| {
            super::super::supervisor::command::retune(paths, self.lane, self.occupant, live)
        };
        if !matches!(ask(self.live.clone()), Ok(LiveRetuneResult::Applied)) {
            return false;
        }
        if let Some(enabled) = self.restate_fast {
            self.restated = Some(restated(enabled, ask(LiveRetune::Fast { enabled })));
        }
        true
    }

    /// Journal this confirmed change against the current head.
    pub(in crate::resident) fn record(&self, bus: &Bus, epoch: CanonicalUuid) {
        let change = (&self.live, None);
        match &self.restated {
            Some((fast, refusal)) => record_runtime(
                bus,
                epoch,
                &self.actor,
                (self.lane, self.occupant),
                &[change, (fast, refusal.as_deref())],
            ),
            None => record_runtime(
                bus,
                epoch,
                &self.actor,
                (self.lane, self.occupant),
                &[change],
            ),
        }
    }
}

/// The fast mode a model change's restatement left running and, unless it
/// applied cleanly, why. OMP's no-tier refusal leaves the turn running without
/// fast mode in OMP's words. Any other failure leaves it unconfirmed, which
/// the lane row says rather than claiming a fast mode the provider never
/// confirmed.
fn restated(
    enabled: bool,
    answer: std::io::Result<LiveRetuneResult>,
) -> (LiveRetune, Option<String>) {
    let unconfirmed = |detail: &dyn std::fmt::Display| {
        format!("fast mode unconfirmed after the model change: {detail}")
    };
    let refusal = match answer {
        Ok(LiveRetuneResult::Applied) => return (LiveRetune::Fast { enabled }, None),
        Ok(LiveRetuneResult::Refused { detail }) if vendor::omp::fast_mode_unavailable(&detail) => {
            detail
        }
        Ok(LiveRetuneResult::Refused { detail }) => unconfirmed(&detail),
        Err(error) => unconfirmed(&error),
    };
    (LiveRetune::Fast { enabled: false }, Some(refusal))
}

/// Journal that a running occupant's vendor refused fast mode in these words
/// and the occupant turned it off to continue without it.
pub(in crate::resident) fn record_fast_refusal(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    refusal: &str,
) {
    record_runtime(
        bus,
        epoch,
        &wire::Actor::Spice,
        (lane, occupant),
        &[(&LiveRetune::Fast { enabled: false }, Some(refusal))],
    );
}

/// Asks running occupants for owed live changes on one worker outside the
/// resident loop, one at a time in the order their writes committed, so a
/// later change to the same occupant is never applied before an earlier one.
/// Confirmed changes return through [`LiveChanges::recv`] for the loop to
/// journal, which keeps the loop the only residency writer.
pub(in crate::resident) struct LiveChanges {
    owed: Option<mpsc::Sender<LiveChange>>,
    confirmed: tokio::sync::mpsc::UnboundedReceiver<LiveChange>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl LiveChanges {
    /// Start the worker that confirms each queued live change.
    pub(in crate::resident) fn start(paths: &RuntimePaths) -> CommandResult<Self> {
        let (owed, asks) = mpsc::channel::<LiveChange>();
        let (confirm, confirmed) = tokio::sync::mpsc::unbounded_channel();
        let paths = paths.clone();
        let worker = std::thread::Builder::new()
            .name("spice-live-retune".to_owned())
            .spawn(move || {
                for mut change in asks {
                    if confirm.is_closed() {
                        return;
                    }
                    if change.confirm(&paths) && confirm.send(change).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| {
                super::super::release::environment_fault(
                    "lane::live-change-start-failed",
                    &error.to_string(),
                )
            })?;
        Ok(Self {
            owed: Some(owed),
            confirmed,
            worker: Some(worker),
        })
    }

    /// Queue the changes one dispatched command owes.
    pub(in crate::resident) fn owe(&self, changes: Vec<LiveChange>) {
        let Some(owed) = &self.owed else {
            return;
        };
        for change in changes {
            let _ = owed.send(change);
        }
    }

    /// The next change its provider confirmed.
    pub(in crate::resident) async fn recv(&mut self) -> Option<LiveChange> {
        self.confirmed.recv().await
    }
}

impl Drop for LiveChanges {
    fn drop(&mut self) {
        drop(self.owed.take());
        self.confirmed.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Apply one confirmed live change to the running launch identity, saying
/// whether it changed anything. A fast-mode change keeps the vendor's refusal
/// when that turned it off and clears any earlier one, and a new refusal is
/// news even when fast mode was already off; a model change clears it, since
/// the refusal was the old model's.
fn apply_runtime(row: &mut wire::LaneResidency, live: &LiveRetune, refusal: Option<&str>) -> bool {
    let launch = &mut row
        .session
        .as_mut()
        .expect("a live request requires a recorded session")
        .launch;
    match live {
        LiveRetune::Model { provider, model } => {
            let model = format!("{provider}/{model}");
            if launch.model == model {
                return false;
            }
            launch.model = model;
            launch.fast_refusal = None;
        }
        LiveRetune::Effort { effort } => {
            if launch.effort == *effort {
                return false;
            }
            launch.effort.clone_from(effort);
        }
        LiveRetune::Fast { enabled } => {
            if launch.fast_mode == *enabled
                && (refusal.is_none() || launch.fast_refusal.as_deref() == refusal)
            {
                return false;
            }
            launch.fast_mode = *enabled;
            launch.fast_refusal = refusal.map(str::to_owned);
        }
    }
    true
}

/// Apply confirmed live changes in order, saying whether any changed the
/// running launch identity.
fn apply_runtime_changes(
    row: &mut wire::LaneResidency,
    changes: &[(&LiveRetune, Option<&str>)],
) -> bool {
    changes.iter().fold(false, |changed, (live, refusal)| {
        apply_runtime(row, live, *refusal) | changed
    })
}

/// Install `session` as this seat's running session. A refusal outlives the
/// occupant that met it: a launch of the same model without fast mode
/// inherits it, so later turns neither meet nor announce it again.
pub(in crate::resident) fn seat_session(
    row: &mut wire::LaneResidency,
    mut session: wire::LaneSession,
) {
    if let Some(prior) = row.session.take()
        && !session.launch.fast_mode
        && prior.launch.model == session.launch.model
    {
        session.launch.fast_refusal = prior.launch.fast_refusal;
    }
    row.session = Some(session);
}

/// One runtime fact carrying every change in `changes`; a fast-mode change
/// carries its refusal.
fn runtime_fact(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    changes: &[(&LiveRetune, Option<&str>)],
) -> wire::body::LaneRuntimeRetuned {
    let mut fact = wire::body::LaneRuntimeRetuned {
        lane,
        occupant,
        model: None,
        effort: None,
        fast_mode: None,
        fast_refusal: None,
    };
    for (live, refusal) in changes {
        match live {
            LiveRetune::Model { provider, model } => {
                fact.model = Some(format!("{provider}/{model}"));
            }
            LiveRetune::Effort { effort } => fact.effort = Some(effort.clone()),
            LiveRetune::Fast { enabled } => {
                fact.fast_mode = Some(*enabled);
                fact.fast_refusal = refusal.map(str::to_owned);
            }
        }
    }
    fact
}

/// The fast mode a selection's write owes running occupants: a fast-mode
/// write owes each flip it makes, and a model change owes the lane's
/// effective fast mode for the model it brings, since OMP states fast mode
/// for the current model's tier family alone.
fn owed_fast(
    selection: Selection<'_>,
    before: &wire::ResidencyHead,
    after: &wire::ResidencyHead,
    global: Option<bool>,
) -> (Vec<FastFlip>, Option<bool>) {
    match (selection, global) {
        (Selection::Fast(_), Some(global)) => (fast_flips(before, after, global, global), None),
        (Selection::Model(_), Some(global)) => (
            Vec::new(),
            after
                .lanes
                .iter()
                .find(|row| row.lane == selection.lane())
                .map(|row| after.fast_mode(row, global)),
        ),
        _ => (Vec::new(), None),
    }
}

/// Commit one seated lane's model, effort, or fast-mode selection and owe the
/// live OMP change the running occupant's served catalog resolves it to.
pub(in crate::resident) fn retune(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &Seats,
    command: &wire::AdmittedCommand,
    selection: Selection<'_>,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    if let Some(refusal) = selection.validate() {
        return refusal;
    }
    let observed = match current_residency(bus, epoch) {
        Ok(head) => head,
        Err(outcome) => return typed_outcome(outcome),
    };
    let topology_revision =
        match next_topology_revision(observed.body(), selection.topology_revision()) {
            Ok(revision) => revision,
            Err(outcome) => return outcome,
        };
    let global = if matches!(selection, Selection::Effort(_)) {
        None
    } else {
        match global_fast_mode(bus) {
            Ok(global) => Some(global),
            Err(outcome) => return typed_outcome(outcome),
        }
    };
    let mut next = observed.body().clone();
    next.topology_revision = topology_revision;
    let Some(row) = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == selection.lane())
    else {
        return missing_lane(selection.lane());
    };
    let live = match selection.live_request(row, paths) {
        Ok(live) => live.zip(row.occupant),
        Err(outcome) => return outcome,
    };
    let seat_fact = selection.replace_seat(row);
    let (flips, restate_fast) = owed_fast(selection, observed.body(), &next, global);
    let answer = match code_snapshot(bus)
        .and_then(|codes| answer(&next, None, seats, paths, codes.facts()))
    {
        Ok(answer) => answer,
        Err(outcome) => return typed_outcome(outcome),
    };

    let mut write = match write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    ) {
        Ok(write) => write,
        Err(outcome) => return typed_outcome(outcome),
    };
    guard_residency(&mut write, &observed);
    let cause = FactCause::Existing(observed.position());
    let seat_fact = match seat_fact {
        SeatFact::Model(body) => write.fact(body, cause),
        SeatFact::Effort(body) => write.fact(body, cause),
        SeatFact::Fast(body) => write.fact(body, cause),
    };
    let seat_fact = match seat_fact {
        Ok(fact) => fact,
        Err(error) => return store_fault("lane::fact-invalid", &error.to_string()),
    };
    write.residency_head(seat_fact, move |_| next);
    let result = match selection {
        Selection::Model(request) => {
            command_result(&mut write, command, request.clone(), epoch, answer.clone())
        }
        Selection::Effort(request) => {
            command_result(&mut write, command, request.clone(), epoch, answer.clone())
        }
        Selection::Fast(request) => {
            command_result(&mut write, command, request.clone(), epoch, answer.clone())
        }
    };
    if let Err(outcome) = result {
        return typed_outcome(outcome);
    }
    let seated = match commit(bus, write) {
        Ok(committed) => committed,
        Err(outcome) => return typed_outcome(outcome),
    };
    let first = seq(seated.first());
    let last = seq(seated.last());
    // The seat is durable before the occupant is touched, so no later refusal
    // can leave a running change that the journal never heard about. The reply
    // is exactly this transaction's receipt, so a retried command replays it
    // unchanged; a confirmed runtime change is a later residency fact that
    // lane reads observe.
    if let Some((live, occupant)) = live {
        let mut change = LiveChange::new(command, selection.lane(), occupant, live);
        change.restate_fast = restate_fast;
        effects.owe_live(change);
    }
    owe_fast_flips(effects, command, flips);
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange { first, last }),
        body: CommandAnswer::Lane(Box::new(answer)),
    })
}

/// Commits a runtime fact may race before it gives up; each attempt re-reads
/// the head, so only a lane whose residency changes this often goes unrecorded.
const RUNTIME_RECORD_ATTEMPTS: usize = 3;

/// Journal confirmed live changes to one occupant as one runtime fact against
/// the current head, as long as the same occupant still runs; a successor
/// launches from the seat instead.
fn record_runtime(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: &wire::Actor,
    (lane, occupant): (CanonicalUuid, CanonicalUuid),
    changes: &[(&LiveRetune, Option<&str>)],
) {
    for _ in 0..RUNTIME_RECORD_ATTEMPTS {
        let Ok(observed) = current_residency(bus, epoch) else {
            return;
        };
        let mut next = observed.body().clone();
        let Some(row) = next.lanes.iter_mut().find(|row| row.lane == lane) else {
            return;
        };
        let launched = row.session.is_some();
        if row.occupant != Some(occupant) || !launched {
            return;
        }
        if !apply_runtime_changes(row, changes) {
            return;
        }
        let Ok(mut write) = write(actor.clone(), epoch, Producer::ResidentRecognition) else {
            return;
        };
        guard_residency(&mut write, &observed);
        let Ok(fact) = write.fact(
            runtime_fact(lane, occupant, changes),
            FactCause::Existing(observed.position()),
        ) else {
            return;
        };
        write.residency_head(fact, move |_| next);
        if commit(bus, write).is_ok() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
