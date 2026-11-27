//! The resident's single exhaustive command dispatcher.
//! Every admitted command reaches exactly one domain handler from this table.

use std::io;
use std::sync::{Arc, Mutex, OnceLock};

use bus::Bus;
use lane::occupant::Seats;
use spice::placeholder::PlaceholderReason;
use spice::repository::Repository;
use wire::intent::{CommandAnswer, Intent};
use wire::{CanonicalUuid, CommandKind, Outcome, Refusal, Reply, ReplyPosition};

use super::lane_surface::RetuneSelection;
use super::{
    CommandDispatch, CommandEffects, RuntimePaths, SurfaceConfig, consent_surface,
    current_residency, dispatch_proof_recognize, dispatch_up_change, lane_recovery, lane_surface,
    lifecycle, message_surface, policy_surface, principal_surface, rack_surface,
    read_materializations::CurrentReads, settings_surface, steering, trust_report, trust_show,
    vendor_surface,
};

/// Invite a partner, notify the lane the invitation reached, then settle the
/// message its own write admitted from the inviter's tape. A message that
/// fails its proof refuses the invitation; a refused invitation admits
/// nothing.
pub(super) fn invite(
    bus: &Bus,
    paths: &RuntimePaths,
    (epoch, joining): (CanonicalUuid, crate::board_command::Joining<'_>),
    config: &steer::Config,
    profiles: crate::board_command::OccupantProfiles<'_>,
    (command, intent): (&wire::AdmittedCommand, &wire::intent::TaskInvite),
) -> (Outcome<CommandAnswer>, CommandEffects) {
    let mut effects = CommandEffects::default();
    let message = match steering::outbox::prove_invitation(bus, paths, epoch, command, intent) {
        Ok(message) => message,
        Err(outcome) => return (outcome, effects),
    };
    let (outcome, invited) = crate::board_command::task_invite(
        bus,
        command,
        (intent, message.as_ref()),
        joining,
        profiles,
    );
    if let Outcome::Reply(Reply {
        position,
        body: CommandAnswer::TaskInvite(answer),
    }) = &outcome
    {
        if let (ReplyPosition::Committed(_), Some(lane)) = (position, invited) {
            effects.notify_inbox(lane);
        }
        if let Some(submission) = answer.message {
            // The invitation stands whatever happens here. An interrupted
            // message stays accepted and unjudged: its receipt names it
            // unresolved, and a replay or startup recovery resumes it.
            let _ = steering::outbox::deliver_invitation(
                bus,
                paths,
                (epoch, joining),
                config,
                submission,
                &mut effects,
            );
        }
    }
    (outcome, effects)
}

pub(super) struct Surface<'a> {
    bus: &'a Bus,
    repository: &'a Repository,
    pub(super) paths: &'a RuntimePaths,
    epoch: CanonicalUuid,
    lane: lane_surface::Runtime<'a>,
    recovery: lane_recovery::Runtime,
    pub(super) config: &'a SurfaceConfig,
    reads: &'a CurrentReads,
    topic_tree: Arc<crate::board_command::topic_tree::TopicTree>,
    resources: &'a crate::board_command::resources::ResourceClaims,
    /// Retained session profiles a nudge scores, once message startup
    /// restores them.
    profiles: OnceLock<Arc<Mutex<message_surface::MessageState>>>,
}

pub(super) struct Runtimes<'a> {
    lane: lane_surface::Runtime<'a>,
    recovery: lane_recovery::Runtime,
    resources: &'a crate::board_command::resources::ResourceClaims,
}

impl<'a> Runtimes<'a> {
    pub(super) fn new(
        lane: lane_surface::Runtime<'a>,
        recovery: lane_recovery::Runtime,
        resources: &'a crate::board_command::resources::ResourceClaims,
    ) -> Self {
        Self {
            lane,
            recovery,
            resources,
        }
    }
}

impl<'a> Surface<'a> {
    pub(super) fn new(
        bus: &'a Bus,
        repository: &'a Repository,
        paths: &'a RuntimePaths,
        epoch: CanonicalUuid,
        runtimes: Runtimes<'a>,
        config: &'a SurfaceConfig,
        reads: &'a CurrentReads,
    ) -> Self {
        let Runtimes {
            lane,
            recovery,
            resources,
        } = runtimes;
        Self {
            bus,
            repository,
            paths,
            epoch,
            lane,
            recovery,
            config,
            reads,
            topic_tree: crate::board_command::topic_tree::TopicTree::shared(paths.data.clone()),
            resources,
            profiles: OnceLock::new(),
        }
    }

    pub(super) fn repository(&self) -> &'a Repository {
        self.repository
    }

    /// What an invitation join decides against: this resident's repository
    /// and its one resource ledger.
    pub(super) fn joining(&self) -> crate::board_command::Joining<'a> {
        crate::board_command::Joining {
            host: crate::command_client::Host {
                repository: self.repository,
                ambient: &self.config.ambient,
            },
            resources: self.resources,
        }
    }

    /// Give nudges the retained lane profiles message startup restored.
    pub(super) fn attach_profiles(&self, profiles: Arc<Mutex<message_surface::MessageState>>) {
        let _ = self.profiles.set(profiles);
    }

    /// Record a seated occupant's consent request and answer what policy
    /// answers.
    pub(super) fn ask_consent(
        &self,
        residency: &wire::ResidencyHead,
        ask: consent_surface::LaneAsk,
        diagnostic: &mut dyn io::Write,
    ) -> super::CommandResult<()> {
        consent_surface::ask(
            self.bus,
            self.reads,
            self.paths,
            &self.config.consent,
            residency,
            ask,
            diagnostic,
        )
    }

    pub(super) fn refresh_reads(&self) -> super::CommandResult<()> {
        self.topic_tree.notify();
        self.reads
            .refresh()
            .map_err(|error| super::store_fault("resident::read-materializer-unavailable", &error))
    }

    /// Start the sole topic writer without waiting for any replay.
    pub(super) fn synchronize_reads_before_publish(&self) -> super::CommandResult<()> {
        let reader = self.bus.reader();
        self.topic_tree
            .start(move || reader.task_snapshot().ok())
            .map_err(|error| {
                super::environment_fault("resident::topic-regrow-unavailable", &error.to_string())
            })?;
        self.reads
            .synchronize_before_publish()
            .map_err(|error| super::store_fault("resident::read-materialization-failed", &error))
    }

    pub(super) fn replay(
        &self,
        seats: &Seats,
        command: &wire::AdmittedCommand,
    ) -> Option<Outcome<CommandAnswer>> {
        if command.kind() == CommandKind::OutboxSubmit {
            // The body index, not a transient command envelope, owns packet recovery.
            return None;
        }
        let outcome = replay_command(self.bus, command)?;
        let Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Lane(_),
        }) = outcome
        else {
            return Some(outcome);
        };
        let current = lane_surface::read(self.bus, self.paths, self.epoch, seats, command);
        let Outcome::Reply(Reply {
            body: CommandAnswer::Lane(answer),
            ..
        }) = current
        else {
            return Some(current);
        };
        Some(Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Lane(answer),
        }))
    }

    pub(super) fn dispatch_fresh(
        &self,
        seats: &mut Seats,
        command: &wire::AdmittedCommand,
    ) -> CommandDispatch {
        match command.body() {
            Intent::TopicRead(wire::topic::TopicRead::Preview { title, pitch }) => {
                return CommandDispatch::ordinary(crate::board_command::topic_tree::preview(
                    self.bus,
                    self.repository,
                    command.actor(),
                    &self.topic_tree,
                    title,
                    pitch,
                ));
            }
            Intent::TopicRead(wire::topic::TopicRead::Show { topic, after }) => {
                return CommandDispatch::ordinary(super::topic_surface::read(
                    self.bus, topic, *after,
                ));
            }
            Intent::TopicEdit(request) => {
                return CommandDispatch::ordinary(super::topic_surface::edit(
                    self.bus, command, request,
                ));
            }
            _ => {}
        }
        if let Some(outcome) = crate::board_command::dispatch_with_resources(
            self.bus,
            command,
            Some(crate::command_client::Host {
                repository: self.repository,
                ambient: &self.config.ambient,
            }),
            &self.config.allocation,
            self.profiles.get().map(AsRef::as_ref),
            Some(self.resources),
        ) {
            // The board command has already committed. Reconciliation only
            // restores advisory claim possessions and cannot replace its reply.
            let _ = self.resources.restore(self.bus, Some(self.repository));
            return CommandDispatch::ordinary(outcome);
        }
        if let Some(dispatched) = self.dispatch_lane(seats, command) {
            return dispatched;
        }
        if let Some(dispatched) = self.dispatch_controls(command) {
            return dispatched;
        }
        CommandDispatch::ordinary(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                PlaceholderReason::HandlerNotLanded.to_reason(),
                None,
                Some(command.kind().to_string()),
                None,
            ),
        )))
    }

    fn dispatch_lane(
        &self,
        seats: &mut Seats,
        command: &wire::AdmittedCommand,
    ) -> Option<CommandDispatch> {
        let ordinary = |outcome| Some(CommandDispatch::ordinary(outcome));
        match (command.kind(), command.body()) {
            (CommandKind::UpChange, Intent::UpChange(change)) => ordinary(dispatch_up_change(
                self.bus, self.paths, self.epoch, seats, command, change,
            )),
            (CommandKind::ProofRecognize, Intent::Recognize(recognize)) => ordinary(
                dispatch_proof_recognize(self.bus, self.paths, self.epoch, command, recognize),
            ),
            (CommandKind::LaneRead, Intent::LaneRead(_)) => ordinary(lane_surface::read(
                self.bus, self.paths, self.epoch, seats, command,
            )),
            (CommandKind::LaneJobsRead, Intent::LaneJobs(_)) => ordinary(
                lane_surface::job_catalog(self.bus, self.paths, self.epoch, command),
            ),
            (CommandKind::LaneJobRead | CommandKind::LaneJobStop, Intent::LaneJob(target)) => {
                ordinary(lane_surface::job_control(
                    self.bus, self.paths, self.epoch, command, target,
                ))
            }
            (CommandKind::LaneSeat, Intent::LaneSeat(seat)) => ordinary(lane_surface::seat(
                self.bus, self.paths, self.epoch, seats, &self.lane, command, seat,
            )),
            (CommandKind::LaneState, Intent::LaneState(state)) => {
                Some(self.dispatch_lane_state(seats, command, state))
            }
            (CommandKind::LaneRenewal, Intent::LaneRenewal(renewal)) => ordinary(
                lane_surface::renew(self.bus, self.paths, self.epoch, seats, command, renewal),
            ),
            (CommandKind::LaneRecall, Intent::LaneRecall(recall)) => ordinary(
                lane_surface::recall(self.bus, self.paths, self.epoch, seats, command, recall),
            ),
            _ => self.dispatch_lane_effects(seats, command),
        }
    }

    fn dispatch_lane_state(
        &self,
        seats: &mut Seats,
        command: &wire::AdmittedCommand,
        state: &wire::intent::LaneState,
    ) -> CommandDispatch {
        if state.worktree.is_some() {
            return match lane_recovery::begin_import(
                self.bus,
                self.paths,
                self.epoch,
                seats,
                &self.recovery,
                state,
            ) {
                Ok(launch) => CommandDispatch::import(launch),
                Err(outcome) => CommandDispatch::ordinary(outcome),
            };
        }
        if state.action == wire::intent::LaneAction::Wake {
            return match lane_surface::begin_wake(
                self.bus, self.paths, self.epoch, seats, &self.lane, state,
            ) {
                Ok(launch) => CommandDispatch::wake(launch),
                Err(outcome) => CommandDispatch::ordinary(outcome),
            };
        }
        CommandDispatch::ordinary(lane_surface::state(
            self.bus, self.paths, self.epoch, seats, &self.lane, command, state,
        ))
    }

    fn dispatch_lane_effects(
        &self,
        seats: &mut Seats,
        command: &wire::AdmittedCommand,
    ) -> Option<CommandDispatch> {
        let mut effects = CommandEffects::default();
        let outcome = match (command.kind(), command.body()) {
            (CommandKind::LaneRetune, Intent::LaneRetune(retune)) => {
                self.retune(seats, command, RetuneSelection::Model(retune), &mut effects)
            }
            (CommandKind::LaneEffortRetune, Intent::LaneEffortRetune(retune)) => self.retune(
                seats,
                command,
                RetuneSelection::Effort(retune),
                &mut effects,
            ),
            (CommandKind::LaneFastRetune, Intent::LaneFastRetune(retune)) => {
                self.retune(seats, command, RetuneSelection::Fast(retune), &mut effects)
            }
            (CommandKind::TeamArrange, Intent::TeamArrange(arrange)) => {
                lane_surface::arrange_teams(
                    self.bus,
                    self.paths,
                    self.epoch,
                    seats,
                    command,
                    arrange,
                    &mut effects,
                )
            }
            (CommandKind::TeamConfigure, Intent::TeamConfigure(configure)) => {
                lane_surface::configure(
                    self.bus,
                    self.paths,
                    self.epoch,
                    seats,
                    command,
                    configure,
                    &mut effects,
                )
            }
            _ => return None,
        };
        Some(CommandDispatch::with_effects(outcome, effects))
    }

    fn retune(
        &self,
        seats: &Seats,
        command: &wire::AdmittedCommand,
        selection: lane_surface::RetuneSelection<'_>,
        effects: &mut CommandEffects,
    ) -> Outcome<CommandAnswer> {
        let (bus, paths, epoch) = (self.bus, self.paths, self.epoch);
        lane_surface::retune(bus, paths, epoch, seats, command, selection, effects)
    }

    fn dispatch_invite(
        &self,
        command: &wire::AdmittedCommand,
        intent: &wire::intent::TaskInvite,
    ) -> CommandDispatch {
        let (outcome, effects) = invite(
            self.bus,
            self.paths,
            (self.epoch, self.joining()),
            &self.config.steering,
            self.profiles.get().map(AsRef::as_ref),
            (command, intent),
        );
        CommandDispatch::with_effects(outcome, effects)
    }

    fn dispatch_controls(&self, command: &wire::AdmittedCommand) -> Option<CommandDispatch> {
        match command.body() {
            Intent::OutboxSubmit(request) => {
                let mut effects = CommandEffects::default();
                let outcome = steering::outbox::submit(
                    self.bus,
                    self.paths,
                    (self.epoch, self.joining()),
                    command,
                    request,
                    &self.config.steering,
                    &mut effects,
                );
                return Some(CommandDispatch::with_effects(outcome, effects));
            }
            Intent::OutboxRead(request) => {
                return Some(CommandDispatch::ordinary(steering::outbox::inspect(
                    self.bus, self.paths, self.epoch, command, request,
                )));
            }
            _ => {}
        }
        self.dispatch_room_reads(command)
            .map(CommandDispatch::ordinary)
            .or_else(|| self.dispatch_room_writes(command))
            .or_else(|| self.dispatch_steering(command))
            .or_else(|| self.dispatch_settings(command))
            .or_else(|| self.dispatch_policy(command).map(CommandDispatch::ordinary))
    }

    /// Global runtime settings writes with their lane effects.
    fn dispatch_settings(&self, command: &wire::AdmittedCommand) -> Option<CommandDispatch> {
        match (command.kind(), command.body()) {
            (CommandKind::GlobalSettingsSet, Intent::GlobalSettingsSet(settings)) => {
                let mut effects = CommandEffects::default();
                let outcome =
                    settings_surface::set(self.bus, self.epoch, command, settings, &mut effects);
                Some(CommandDispatch::with_effects(outcome, effects))
            }
            _ => None,
        }
    }

    /// Room reads, which commit nothing and notify no lane.
    fn dispatch_room_reads(
        &self,
        command: &wire::AdmittedCommand,
    ) -> Option<Outcome<CommandAnswer>> {
        Some(match (command.kind(), command.body()) {
            (CommandKind::RoomScore, Intent::RoomScore(request)) => {
                let Some(profiles) = self.profiles.get() else {
                    return Some(Outcome::Refusal(Refusal::bare(
                        wire::RefusalDiagnostic::new(
                            "room::score-profile-store-unavailable".to_owned(),
                            None,
                            None,
                            None,
                        )
                        .with_detail(
                            "resident session profiles have not finished startup".to_owned(),
                        ),
                    )));
                };
                super::design_awareness::score(self.bus, profiles.as_ref(), request)
            }
            (CommandKind::RoomConversationText, Intent::RoomConversationText(request)) => {
                crate::room_command::conversation_text(self.bus, &self.paths.tape, request)
            }
            (CommandKind::RoomContributionRead, Intent::RoomContributionRead(request)) => {
                crate::room_command::contribution_read(
                    self.bus,
                    crate::room_command::SourceOwners {
                        tape: &self.paths.tape,
                        artifacts: &self.paths.artifacts(),
                        git: &self.paths.common,
                    },
                    request,
                )
            }
            _ => return None,
        })
    }

    /// Room writes and deferrals, which notify each lane they address: a
    /// deferral that stops a held open Design notifies its holder.
    fn dispatch_room_writes(&self, command: &wire::AdmittedCommand) -> Option<CommandDispatch> {
        let (outcome, notify): (Outcome<CommandAnswer>, Vec<wire::CanonicalUuid>) =
            match (command.kind(), command.body()) {
                (CommandKind::TaskDefer, Intent::TaskDefer(defer)) => {
                    let (outcome, stopped) = crate::board_command::task_defer(
                        self.bus,
                        command,
                        defer,
                        Some(crate::command_client::Host {
                            repository: self.repository,
                            ambient: &self.config.ambient,
                        }),
                        &self.config.steering,
                    );
                    (outcome, stopped.into_iter().collect())
                }
                (CommandKind::TaskInvite, Intent::TaskInvite(invite)) => {
                    return Some(self.dispatch_invite(command, invite));
                }
                (CommandKind::RoomLeave, Intent::RoomLeave(leave)) => (
                    crate::board_command::room_leave(
                        self.bus,
                        command,
                        leave,
                        Some(self.repository),
                    ),
                    Vec::new(),
                ),
                (CommandKind::RoomContribute, Intent::RoomContribute(contribute)) => (
                    crate::board_command::room_contribute(
                        self.bus,
                        command,
                        contribute,
                        Some(crate::room_command::SourceOwners {
                            tape: &self.paths.tape,
                            artifacts: &self.paths.artifacts(),
                            git: &self.paths.common,
                        }),
                        Some(self.repository),
                    ),
                    Vec::new(),
                ),
                _ => return None,
            };
        let mut effects = CommandEffects::default();
        if matches!(
            &outcome,
            Outcome::Reply(Reply {
                position: ReplyPosition::Committed(_),
                ..
            })
        ) {
            for lane in notify {
                effects.notify_inbox(lane);
            }
        }
        Some(CommandDispatch::with_effects(outcome, effects))
    }

    fn dispatch_steering(&self, command: &wire::AdmittedCommand) -> Option<CommandDispatch> {
        let mut effects = CommandEffects::default();
        let outcome = match (command.kind(), command.body()) {
            (CommandKind::SteerInbox, Intent::SteerInbox(inbox)) => Some(steering::inbox(
                self.bus, self.paths, self.epoch, command, inbox,
            )),
            (CommandKind::QuoteRead, Intent::QuoteRead(request)) => {
                Some(steering::quote(self.bus, self.paths, request))
            }
            (CommandKind::SteerSend, Intent::SteerSend(send)) => Some(steering::send(
                self.bus,
                self.epoch,
                &self.config.steering,
                self.profiles.get().map(AsRef::as_ref),
                command,
                send,
                &mut effects,
            )),
            (CommandKind::SteerLineControl, Intent::SteerLineControl(control)) => {
                Some(steering::line_control(
                    self.bus,
                    self.epoch,
                    &self.config.steering,
                    command,
                    control,
                    &mut effects,
                ))
            }
            (CommandKind::LineArm, Intent::LineArm(request)) => {
                Some(steering::line_arm(self.bus, self.epoch, command, request))
            }
            (CommandKind::SteerCancel, Intent::DirectiveRef(cancel)) => Some(steering::cancel(
                self.bus,
                self.epoch,
                command,
                cancel,
                &mut effects,
            )),
            (CommandKind::SteerResend, Intent::DirectiveRef(resend)) => Some(steering::resend(
                self.bus,
                self.paths,
                self.epoch,
                &self.config.steering,
                command,
                resend,
                &mut effects,
            )),
            (CommandKind::SteerRecover, Intent::DirectiveRef(recover)) => Some(steering::recover(
                self.bus,
                self.epoch,
                &self.config.steering,
                command,
                recover,
                &mut effects,
            )),
            _ => None,
        }?;
        Some(CommandDispatch::with_effects(outcome, effects))
    }

    fn dispatch_policy(&self, command: &wire::AdmittedCommand) -> Option<Outcome<CommandAnswer>> {
        match (command.kind(), command.body()) {
            (CommandKind::RackRead, Intent::RackRead(request)) => {
                Some(rack_surface::read(self.bus, request))
            }
            (CommandKind::GlobalSettingsRead, Intent::GlobalSettingsRead(_)) => {
                Some(settings_surface::read(self.bus))
            }
            (CommandKind::VendorCatalog, Intent::VendorCatalog(_)) => {
                Some(vendor_surface::catalog(self.bus, self.repository))
            }
            (CommandKind::ConsentRead, Intent::ConsentRead(_)) => {
                Some(policy_surface::read(self.reads, &self.config.consent))
            }
            (CommandKind::ConsentPolicy, Intent::ConsentPolicy(_)) => {
                Some(policy_surface::policy(self.reads, &self.config.consent))
            }
            (CommandKind::ConsentDecide, Intent::ConsentDecide(decide)) => {
                Some(policy_surface::decide(
                    self.bus,
                    self.reads,
                    self.paths,
                    self.epoch,
                    &self.config.consent,
                    command,
                    decide,
                ))
            }
            (CommandKind::TrustPend, Intent::TrustRef(reference)) => {
                Some(policy_surface::trust_pend(
                    self.bus,
                    self.repository,
                    self.epoch,
                    command,
                    reference,
                ))
            }
            (CommandKind::TrustGrant | CommandKind::TrustRevoke, Intent::TrustRef(reference)) => {
                Some(policy_surface::trust_rule(
                    self.bus,
                    self.repository,
                    self.epoch,
                    command,
                    reference,
                ))
            }
            (CommandKind::TrustReport, Intent::TrustReportRead(request)) => {
                Some(trust_report::read(self.bus, request))
            }
            (CommandKind::TrustShow, Intent::TrustRef(reference)) => {
                Some(trust_show::read(self.bus, reference))
            }
            (CommandKind::PrincipalDirectoryRead, Intent::PrincipalDirectoryRead(_)) => {
                Some(principal_surface::directory(self.bus, command.actor()))
            }
            (CommandKind::PrincipalRead, Intent::PrincipalRead(_)) => {
                Some(principal_surface::read(self.bus))
            }
            (CommandKind::PrincipalAdmit, Intent::PrincipalAdmit(admit)) => {
                Some(self.republishing_roles(principal_surface::admit(
                    self.bus,
                    self.repository,
                    self.epoch,
                    command,
                    admit,
                )))
            }
            (CommandKind::PrincipalEdit, Intent::PrincipalEdit(edit)) => {
                Some(principal_surface::edit(self.bus, self.epoch, command, edit))
            }
            (
                CommandKind::PrincipalGrant | CommandKind::PrincipalRevoke,
                Intent::PrincipalRoleRef(reference),
            ) => Some(self.republishing_roles(principal_surface::role(
                self.bus, self.epoch, command, reference,
            ))),
            (CommandKind::PrincipalDeauthorize, Intent::PrincipalDeauthorize(request)) => {
                Some(self.republishing_roles(principal_surface::deauthorize(
                    self.bus, self.epoch, command, request,
                )))
            }
            _ => None,
        }
    }

    /// Republish the principal heads once a principal change lands, so the
    /// changed principal's next command is decided by its new roles and an
    /// issued or retired capability binds or ends at once.
    fn republishing_roles(&self, outcome: Outcome<CommandAnswer>) -> Outcome<CommandAnswer> {
        if matches!(outcome, Outcome::Reply(_))
            && let Err(fault) = self.lane.publish_roles(self.bus)
        {
            return super::typed_outcome(fault);
        }
        outcome
    }

    pub(super) fn inbox_residency(
        &self,
    ) -> super::CommandResult<journal::JournalHead<wire::ResidencyHead>> {
        current_residency(self.bus, self.epoch)
    }

    pub(super) fn complete_command_launch(
        &self,
        seats: &mut Seats,
        command: &wire::AdmittedCommand,
        completion: super::CommandLaunchCompletion,
        launched: super::CommandResult<super::supervisor::Launch>,
    ) -> Outcome<CommandAnswer> {
        match completion {
            super::CommandLaunchCompletion::Wake(pending) => lane_surface::finish_wake(
                self.bus, self.paths, self.epoch, seats, command, *pending, launched,
            ),
            super::CommandLaunchCompletion::Import(pending) => lane_recovery::finish_import(
                self.bus, self.paths, self.epoch, seats, command, *pending, launched,
            ),
        }
    }

    pub(super) fn complete_automatic_launch(
        &self,
        lifecycle: &mut lifecycle::Runtime,
        seats: &mut Seats,
        diagnostic: &mut dyn std::io::Write,
        pending: lifecycle::PendingLaunch,
        launched: super::CommandResult<super::supervisor::Launch>,
    ) -> super::CommandResult<Option<CanonicalUuid>> {
        lifecycle.complete_launch(&self.resident(), seats, diagnostic, pending, launched)
    }

    pub(super) fn notify_inbox(&self, lane: CanonicalUuid) {
        steering::notify_inbox(self.paths, lane);
    }

    pub(super) const fn epoch(&self) -> CanonicalUuid {
        self.epoch
    }

    pub(super) const fn bus(&self) -> &'a Bus {
        self.bus
    }

    fn resident(&self) -> lifecycle::Resident<'_> {
        lifecycle::Resident {
            bus: self.bus,
            repository: self.repository,
            paths: self.paths,
            epoch: self.epoch,
            lane_runtime: &self.lane,
            config: self.config,
            profiles: self.profiles.get().map(AsRef::as_ref),
        }
    }

    pub(super) fn reconcile_lifecycle(
        &self,
        lifecycle: &mut lifecycle::Runtime,
        seats: &mut Seats,
        launches: &mut super::launch_runtime::Runtime,
        diagnostic: &mut dyn std::io::Write,
    ) -> super::CommandResult<Option<lifecycle::WakeDeadline>> {
        lifecycle.reconcile(&self.resident(), seats, launches, diagnostic)
    }

    pub(super) fn observe_lifecycle_death(
        &self,
        lifecycle: &mut lifecycle::Runtime,
        seats: &Seats,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        diagnostic: &mut dyn std::io::Write,
    ) -> super::CommandResult<()> {
        let seated = seats
            .started(lane)
            .filter(|activity| activity.occupant() == occupant);
        lifecycle.observe_death(self.bus, self.epoch, seated, lane, false, diagnostic)
    }

    pub(super) fn expire_lifecycle_hold(
        &self,
        lifecycle: &mut lifecycle::Runtime,
        lane: CanonicalUuid,
    ) -> super::CommandResult<()> {
        lifecycle.expire_hold(self.bus, self.epoch, lane)
    }

    pub(super) fn prepare_explicit_lifecycle(
        &self,
        lifecycle: &mut lifecycle::Runtime,
        command: &wire::AdmittedCommand,
    ) -> super::CommandResult<bool> {
        let mut released = lifecycle.observe_topology(command);
        if let Intent::LaneState(state) = command.body()
            && command.holds(wire::principal::PrincipalRole::Operator)
            && state.action == wire::intent::LaneAction::Wake
        {
            if lifecycle.launch_pending(state.lane) {
                return Err(Outcome::Refusal(Refusal::bare(
                    wire::RefusalDiagnostic::new(
                        "lifecycle::launch-pending",
                        Some("lane".to_owned()),
                        Some("no launch proof pending for this lane".to_owned()),
                        Some(state.lane.to_string()),
                    )
                    .with_detail(
                        "wait for the current wake to settle before starting another occupant",
                    ),
                )));
            }
            if state.worktree.is_none() {
                lifecycle.operator_wake(self.bus, self.epoch, state.lane)?;
                released = true;
            }
        }
        if matches!(command.body(), Intent::LaneRenewal(_)) {
            lifecycle.release_all();
            released = true;
        }
        Ok(released)
    }
}

pub(in crate::resident) fn replay_command(
    bus: &Bus,
    command: &wire::AdmittedCommand,
) -> Option<Outcome<CommandAnswer>> {
    let replayed = command.clone();
    match bus.read(move |read| read.command_replay(&replayed)) {
        Ok(Some(journal::CommandReplay::Exact(outcome))) => Some(outcome),
        Ok(Some(journal::CommandReplay::Conflict)) => Some(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "command::identity-reused",
                Some("id".to_owned()),
                Some("an unused identity or the exact original request".to_owned()),
                Some(command.id().to_string()),
            ),
        ))),
        Ok(None) => None,
        Err(error) => Some(super::store_fault(
            "command::receipt-unreadable",
            &error.to_string(),
        )),
    }
}
