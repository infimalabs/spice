//! App publication and the resident's live surface loop.
mod capabilities;
mod control_dispatch;
mod input;
mod launch_completion;

use std::sync::Arc;

use control_dispatch::{
    ControlDeliveryTurn, Controls, control_delivery_turn, dispatch_control_delivery_burst,
};

use super::*;

struct StartedSurface<'a> {
    capabilities: serve::axum::Capabilities,
    discovery: serve::Discovery,
    control: serve::axum::Control,
    publication: SurfacePublication<'a>,
    shutdown: oneshot::Sender<()>,
    server_task: tokio::task::JoinHandle<Result<(), io::Error>>,
    message_publisher: serve::stream::MessagePublisher,
    progress: &'a mut dyn io::Write,
}

struct SurfacePublication<'a> {
    discovery: serve::Discovery,
    published: PublishedDiscovery,
    app_description: String,
    token: Option<String>,
    open: bool,
    endpoint: &'a mut dyn io::Write,
    daemon: Option<ChildStatus>,
}

struct MessageStartup {
    reader: message_surface::BacklogReader,
    diagnostic: Vec<u8>,
}

struct SurfaceRuntimes {
    timer_commits: bus::BroadcastSubscriber,
    timers: timers::Runtime,
    quota: quota::Runtime,
    quota_triggers: quota::Triggers,
    lifecycle: lifecycle::Runtime,
    taste_adjudication: Option<taste_surface::AdjudicationRuntime>,
    live_changes: lane_surface::LiveChanges,
}

impl SurfaceRuntimes {
    fn start(
        bus: &Bus,
        repository: &Repository,
        config: &SurfaceConfig,
        paths: &RuntimePaths,
    ) -> CommandResult<Self> {
        let environment = config.ambient.environment().map_err(|error| {
            environment_fault("resident::environment-invalid", &error.to_string())
        })?;
        let native_system =
            spice::git_shadow::native_system_config(repository.worktree_root(), &environment)
                .map_err(|error| {
                    environment_fault(
                        "resident::native-system-config-unavailable",
                        &error.to_string(),
                    )
                })?;
        let lifecycle = lifecycle::Runtime::with_native_system(&config.lane, native_system);
        let timer_commits = bus.reader().subscribe();
        let timers = timers::Runtime::start(config.steering.clone(), paths.supervisors.clone())?;
        let extension = quota::pin_extension(&paths.runtime).map_err(|error| {
            environment_fault("resident::quota-extension-unavailable", &error.to_string())
        })?;
        let mut quota = quota::Runtime::start(
            config.omp_binary.clone(),
            extension,
            repository.worktree_root().to_owned(),
            &config.ambient,
        );
        let quota_triggers = quota::Triggers::start(bus)?;
        quota.request();
        let taste_adjudication = if config.judge_enabled {
            Some(taste_surface::start_adjudication(&config.ambient)?)
        } else {
            None
        };
        let live_changes = lane_surface::LiveChanges::start(paths)?;
        Ok(Self {
            timer_commits,
            timers,
            quota,
            quota_triggers,
            lifecycle,
            taste_adjudication,
            live_changes,
        })
    }
}

/// The taste submitter and completion receiver when adjudication runs.
fn taste_parts(
    adjudication: Option<&mut taste_surface::AdjudicationRuntime>,
) -> (
    Option<&mut taste_surface::AdjudicationSubmitter>,
    Option<&mut taste_surface::AdjudicationCompletions>,
) {
    adjudication
        .map(taste_surface::AdjudicationRuntime::parts)
        .map_or((None, None), |(submitter, completions)| {
            (Some(submitter), Some(completions))
        })
}

impl SurfacePublication<'_> {
    fn publish(
        &mut self,
        paths: &RuntimePaths,
        residency: &ResidencyHead,
        progress: &mut dyn io::Write,
    ) -> CommandResult<()> {
        steering::publish_lane_discoveries(paths, &self.discovery, residency)?;
        self.discovery
            .publish(&self.published.path)
            .map_err(|error| environment_fault("serve::publish-failed", &error.to_string()))?;
        if let Some(daemon) = self.daemon.take() {
            daemon.published();
        }
        announce_started_surface(
            paths,
            &self.app_description,
            self.token.as_deref(),
            self.open,
            self.endpoint,
            progress,
        );
        Ok(())
    }
}

async fn start_surface<'a>(
    bus: &Bus,
    paths: &RuntimePaths,
    repository: &Repository,
    residency: &ResidencyHead,
    config: &SurfaceConfig,
    quota: serve::stream::QuotaFeed,
    launch: SurfaceLaunch<'a>,
) -> CommandResult<StartedSurface<'a>> {
    let epoch = residency.epoch;
    let operator = new_uuid();
    let capabilities = capabilities::build(bus, operator, residency)?;
    let SurfaceLaunch {
        app,
        version,
        binding,
        resident_lock,
        open,
        progress,
        endpoint,
        daemon,
    } = launch;
    let app_description = app.to_string();
    let AppBinding { listen, access } = binding;
    let AppAccess {
        policy,
        kind,
        token,
    } = access;
    let generation = serve::axum::Generation::new(epoch, capabilities.clone(), version, app);
    let quote_presenter =
        spice::quote::presenter(paths.tape.clone(), bus.reader(), quote_agent_prose);
    let (server, control) = serve::axum::Server::bind_streaming(
        serve::axum::Binding::new(listen, policy),
        generation,
        bus.reader(),
        config.serve.rows(),
        quote_presenter,
        quota,
        configuration_revision(repository.clone()),
    )
    .await
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            return Outcome::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "resident::listen-port-served".to_owned(),
                    Some("requested-listen-address".to_owned()),
                    Some("a port no other listener serves".to_owned()),
                    Some(listen.to_string()),
                )
                .with_detail(format!(
                    "{error}; choose another --port or stop the listener serving it"
                )),
            ));
        }
        environment_fault(
            "serve::bind-failed",
            &format!("listening on {listen} failed: {error}"),
        )
    })?;
    record_bound_launch(resident_lock, epoch, &server, kind)?;
    let discovery = server
        .discovery(operator)
        .map_err(|error| environment_fault("serve::discovery-invalid", &error.to_string()))?;
    let message_publisher = server.message_publisher().ok_or_else(|| {
        environment_fault(
            "serve::message-surface-unavailable",
            "the streaming app server has no transcript activity publisher",
        )
    })?;
    let (shutdown, stopped) = oneshot::channel();
    let server_task = tokio::spawn(server.up_until(async move {
        let _ = stopped.await;
    }));
    Ok(StartedSurface {
        capabilities,
        discovery: discovery.clone(),
        control,
        publication: SurfacePublication {
            discovery,
            published: PublishedDiscovery {
                path: paths.serve.clone(),
                lane_root: paths.lane_serve.clone(),
            },
            app_description,
            token,
            open,
            endpoint,
            daemon,
        },
        shutdown,
        server_task,
        message_publisher,
        progress,
    })
}

/// Keep one opaque configuration generation for all connections to this resident.
fn configuration_revision(repository: Repository) -> serve::stream::ConfigRevision {
    let previous = std::sync::Mutex::new(None::<(spice::config::CaptureGeneration, CanonicalUuid)>);
    serve::stream::ConfigRevision::new(move || {
        let fault = |detail: String| wire::Fault {
            class: wire::FaultClass::Environment,
            reason: "stream::configuration-unreadable".to_owned(),
            detail,
        };
        let mut previous = previous.lock().map_err(|error| fault(error.to_string()))?;
        let loaded = spice::config::capture_repository(&repository, &[])
            .map_err(|error| fault(error.to_string()))?;
        if let Some((generation, revision)) = previous.as_ref()
            && generation == loaded.generation()
        {
            return Ok(*revision);
        }
        let revision = new_uuid();
        *previous = Some((loaded.generation().clone(), revision));
        Ok(revision)
    })
}

fn announce_started_surface(
    paths: &RuntimePaths,
    app_description: &str,
    token: Option<&str>,
    open: bool,
    endpoint: &mut dyn io::Write,
    progress: &mut dyn io::Write,
) {
    let _ = writeln!(progress, "app source: {app_description}");
    announce_app(
        &paths.serve,
        token,
        open,
        || {},
        crate::command_client::system_browser,
        endpoint,
        progress,
    );
    let _ = endpoint.flush();
    let _ = progress.flush();
}

/// The lane and recovery runtimes commands run against, sharing one reporter.
fn runtimes<'a>(
    capabilities: &'a serve::axum::Capabilities,
    discovery: &'a serve::Discovery,
    reporter: &std::sync::Arc<dyn lane::capture::CaptureReporter>,
    reads: &'a read_materializations::CurrentReads,
    resources: &'a crate::board_command::resources::ResourceClaims,
) -> command_surface::Runtimes<'a> {
    command_surface::Runtimes::new(
        lane_surface::Runtime::new(capabilities, discovery, reporter.clone(), reads),
        lane_recovery::Runtime::new(reporter.clone()),
        resources,
    )
}

pub(super) async fn run_surface(
    bus: &Bus,
    repository: &Repository,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    config: &SurfaceConfig,
    seats: &mut Seats,
    launch: SurfaceLaunch<'_>,
) -> CommandResult<()> {
    let design_completions = design_awareness::CompletionCursor::start(bus)?;
    let residency = current_residency(bus, epoch)?;
    let mut backgrounds = SurfaceRuntimes::start(bus, repository, config, paths)?;
    let StartedSurface {
        capabilities,
        discovery,
        control,
        mut publication,
        shutdown,
        mut server_task,
        message_publisher,
        progress,
    } = start_surface(
        bus,
        paths,
        repository,
        residency.body(),
        config,
        backgrounds.quota.feed(),
        launch,
    )
    .await?;
    let channels = capture::channels();
    let door = capture::SupervisorReports::bind(paths)?;
    let resources = restore_resources(bus, repository, config)?;
    let joining = crate::board_command::Joining {
        host: crate::command_client::Host::new(repository, &config.ambient),
        resources: &resources,
    };
    recover_startup_state(bus, paths, config, (epoch, joining), seats, progress)?;
    let mut message_startup = launch_message_startup(bus, paths, epoch, &message_publisher)?;
    let reads = read_materializations::CurrentReads::start(bus, paths, &config.consent)
        .map_err(|error| store_fault("resident::read-materialization-failed", &error))?;
    let command_surface = command_surface::Surface::new(
        bus,
        repository,
        paths,
        epoch,
        runtimes(
            &capabilities,
            &discovery,
            &channels.reporter,
            &reads,
            &resources,
        ),
        config,
        &reads,
    );
    let mut wakes = Wakes {
        timers: backgrounds.timers,
        lifecycle: backgrounds.lifecycle,
        launches: super::launch_runtime::Runtime::new(),
        door,
        reconciliation: lifecycle::Reconciliation::default(),
    };
    let mut controls = Controls {
        control,
        inbox_deliveries: InboxDeliveries::new(),
        live_changes: backgrounds.live_changes,
        quota: backgrounds.quota,
    };
    let (taste_submitter, taste_completions) = taste_parts(backgrounds.taste_adjudication.as_mut());
    publish_ready_surface(
        &command_surface,
        seats,
        &mut wakes,
        progress,
        &mut publication,
    )
    .await?;
    let Some(message_startup) = await_message_startup(
        &mut message_startup,
        &command_surface,
        seats,
        &mut wakes,
        &mut controls,
        &mut server_task,
        progress,
    )
    .await?
    else {
        return stop_surface(publication, shutdown, server_task).await;
    };
    let _ = progress.write_all(&message_startup.diagnostic);
    let message_state = Arc::new(std::sync::Mutex::new(message_startup.reader.into_state()));
    command_surface.attach_profiles(Arc::clone(&message_state));
    LiveSurface {
        bus,
        epoch,
        seats,
        command_surface: &command_surface,
        server_task,
        message_publisher,
        message_state,
        design_completions,
        taste_submitter,
        taste_completions,
        records: channels.records,
        failures: channels.failures,
        exits: channels.exits,
        diagnostic: progress,
        quota_triggers: backgrounds.quota_triggers,
        timer_commits: backgrounds.timer_commits,
        wakes,
        controls,
    }
    .drive(publication, shutdown)
    .await
}

fn recover_startup_state(
    bus: &Bus,
    paths: &RuntimePaths,
    config: &SurfaceConfig,
    authority: (CanonicalUuid, crate::board_command::Joining<'_>),
    seats: &mut Seats,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    lane_surface::reconcile_seats(bus, paths, &config.steering, authority, seats, diagnostic)?;
    recover_transcripts(bus, paths, authority, &config.steering, diagnostic)
}

/// The one resource ledger every join, including a recovered invitation ACK,
/// decides against.
fn restore_resources(
    bus: &Bus,
    repository: &Repository,
    config: &SurfaceConfig,
) -> CommandResult<crate::board_command::resources::ResourceClaims> {
    let resources = crate::board_command::resources::ResourceClaims::new(sill::LockTopology::new(
        repository.worktree_root(),
        &config.lock,
    ));
    resources
        .restore(bus, Some(repository))
        .map_err(typed_outcome)?;
    Ok(resources)
}

async fn publish_ready_surface(
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    wakes: &mut Wakes,
    diagnostic: &mut dyn io::Write,
    publication: &mut SurfacePublication<'_>,
) -> CommandResult<()> {
    wakes.reconcile(command_surface, seats, diagnostic)?;
    command_surface.synchronize_reads_before_publish()?;
    let residency = current_residency(command_surface.bus(), command_surface.epoch())?;
    tokio::task::yield_now().await;
    publication.publish(command_surface.paths, residency.body(), diagnostic)
}

fn launch_message_startup(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    publisher: &serve::stream::MessagePublisher,
) -> CommandResult<oneshot::Receiver<CommandResult<MessageStartup>>> {
    let residency = current_residency(bus, epoch)?;
    start_message_startup(paths, residency.body(), publisher, bus.reader())
}

fn start_message_startup(
    paths: &RuntimePaths,
    residency: &wire::ResidencyHead,
    publisher: &serve::stream::MessagePublisher,
    bus: bus::BusReader,
) -> CommandResult<oneshot::Receiver<CommandResult<MessageStartup>>> {
    let paths = paths.clone();
    let residency = residency.clone();
    let publisher = publisher.clone();
    let (finished, receiver) = oneshot::channel();
    std::thread::Builder::new()
        .name("spice-message-startup".to_owned())
        .spawn(move || {
            let mut diagnostic = Vec::new();
            let result = {
                let mut reader = message_surface::BacklogReader::indexed();
                message_surface::restore_agent_messages_indexed(
                    &bus,
                    &paths,
                    &residency,
                    &publisher,
                    &mut reader,
                    &mut diagnostic,
                )
                .map(|()| MessageStartup { reader, diagnostic })
            };
            let _ = finished.send(result);
        })
        .map_err(|error| {
            environment_fault("serve::message-startup-unavailable", &error.to_string())
        })?;
    Ok(receiver)
}

async fn await_message_startup(
    startup: &mut oneshot::Receiver<CommandResult<MessageStartup>>,
    command_surface: &command_surface::Surface<'_>,
    seats: &mut Seats,
    wakes: &mut Wakes,
    controls: &mut Controls,
    server_task: &mut tokio::task::JoinHandle<Result<(), io::Error>>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<Option<MessageStartup>> {
    loop {
        tokio::select! {
            result = &mut *startup => {
                let ready = result.map_err(|_| environment_fault(
                    "serve::message-startup-closed",
                    "the message startup worker ended without its terminal result",
                ))?;
                return ready.map(Some);
            }
            (turn, request) = control_delivery_turn(
                &mut controls.control,
                &controls.inbox_deliveries,
                wakes.reconciliation.control_enabled(),
            ) => match turn {
                ControlDeliveryTurn::Control => {
                    wakes.settle_for_control(command_surface, seats, diagnostic)?;
                    if dispatch_control_delivery_burst(
                        command_surface,
                        seats,
                        wakes,
                        controls,
                        request,
                        diagnostic,
                    ).await? {
                        return Ok(None);
                    }
                }
                ControlDeliveryTurn::Delivery => {
                    deliver_owed_inbox(command_surface, &mut controls.inbox_deliveries).await?;
                }
            },
            completed = wakes.launches.recv() => launch_completion::accept(
                command_surface, seats, wakes, diagnostic, completed,
            ).await?,
            ending = &mut *server_task => return Err(server_outcome(ending)),
        }
    }
}

/// Announce the app this start serves, and dial it when asked to.
///
/// The endpoint is read back out of the discovery file the resident has just
/// published rather than formatted from the address it bound, so the line an
/// operator is told to open has already passed the ownership, loopback-origin,
/// and generation admission that a client applies to the same file. That is
/// the whole reason there is no separate opener command: one reader admits the
/// endpoint, and `up` is simply its first caller.
///
/// Authenticated access decorates that admitted endpoint only after admission.
/// The token stays in the URL fragment for announcement and browser launch, so
/// it neither changes the discovery origin nor crosses in the browser's HTTP
/// origin.
///
/// A resident whose own discovery fails that admission says so and keeps
/// serving. The surface is up either way, and refusing to run because the
/// operator cannot be handed a URL would trade a working appliance for a
/// cosmetic one.
///
/// Only an endpoint reaches the endpoint stream; a failure to produce one is a
/// diagnostic and goes where the other diagnostics went. A shell that reads the
/// endpoint stream to dial it cannot tell a URL from a sentence explaining why
/// there is not one, so it sees an empty capture instead.
pub(in crate::resident) fn announce_app(
    discovery_path: &Path,
    auth_token: Option<&str>,
    open: bool,
    before_current_check: impl FnOnce(),
    browser: impl FnOnce(&str) -> Result<(), String>,
    announcement: &mut dyn io::Write,
    diagnostic: &mut dyn io::Write,
) {
    let result = crate::command_client::open_app_result_with(
        discovery_path,
        before_current_check,
        |endpoint| {
            if open {
                browser(&app_opening(endpoint, auth_token))
            } else {
                Ok(())
            }
        },
    );
    let _ = match result {
        Ok(opened) => writeln!(
            announcement,
            "app endpoint: {}",
            app_opening(&opened.endpoint, auth_token)
        ),
        Err(fault) => writeln!(diagnostic, "app unavailable: {}", fault.reason),
    };
}

fn app_opening(endpoint: &str, auth_token: Option<&str>) -> String {
    auth_token.map_or_else(
        || endpoint.to_owned(),
        |token| format!("{endpoint}#{token}"),
    )
}

/// The live loop's state: one receiver for each closed event source and the
/// resident-owned runtimes their turns settle.
struct LiveSurface<'s, 'c> {
    bus: &'s Bus,
    epoch: CanonicalUuid,
    seats: &'s mut Seats,
    command_surface: &'s command_surface::Surface<'c>,
    server_task: tokio::task::JoinHandle<Result<(), io::Error>>,
    message_publisher: serve::stream::MessagePublisher,
    message_state: Arc<std::sync::Mutex<message_surface::MessageState>>,
    design_completions: design_awareness::CompletionCursor,
    taste_submitter: Option<&'s mut taste_surface::AdjudicationSubmitter>,
    taste_completions: Option<&'s mut taste_surface::AdjudicationCompletions>,
    records: tokio::sync::mpsc::Receiver<lane::capture::CaptureRecord>,
    failures: tokio::sync::mpsc::UnboundedReceiver<lane::capture::CaptureFailure>,
    exits: tokio::sync::mpsc::UnboundedReceiver<lane::capture::CaptureExit>,
    diagnostic: &'s mut dyn io::Write,
    quota_triggers: quota::Triggers,
    timer_commits: bus::BroadcastSubscriber,
    wakes: Wakes,
    controls: Controls,
}

impl LiveSurface<'_, '_> {
    /// Serve turns until a control takes the resident dark, then stop the surface.
    async fn drive(
        mut self,
        publication: SurfacePublication<'_>,
        shutdown: oneshot::Sender<()>,
    ) -> CommandResult<()> {
        let mut input_lead = input::Lead::Supervisor;
        while !self.turn(&mut input_lead).await? {
            reap::present_closed_transcripts(
                self.seats,
                self.command_surface,
                &self.message_publisher,
                self.taste_submitter.as_deref_mut(),
                self.message_state.as_ref(),
                self.diagnostic,
            )?;
        }
        self.wakes.timers.synchronize(self.bus)?;
        stop_surface(publication, shutdown, self.server_task).await
    }

    /// Settle one ready input; true once the resident reached dark.
    async fn turn(&mut self, input_lead: &mut input::Lead) -> CommandResult<bool> {
        let (bus, epoch, command_surface) = (self.bus, self.epoch, self.command_surface);
        let narration_deadline = message_surface::lock_state(self.message_state.as_ref())?
            .narration
            .next_deadline();
        tokio::select! {
            // Reconciliation always probes the supervisor door; rotate the
            // three inputs that can remain continuously ready.
            input = input::next(
                input_lead,
                self.wakes.door.recv(),
                control_delivery_turn(
                    &mut self.controls.control,
                    &self.controls.inbox_deliveries,
                    self.wakes.reconciliation.control_enabled(),
                ),
                self.records.recv(),
            ) => match input {
                input::Input::Supervisor(report) => {
                    let reports = self.wakes.door.batch(report);
                    self.accept_supervisor_reports(reports)?;
                }
                input::Input::Control((turn, request)) => {
                    return self.control_turn(turn, request).await;
                }
                input::Input::Record(record) => self.accept_record_turn(record)?,
            },
            () = narration::wait(narration_deadline) => accept_narration_turn(
                bus, epoch, self.seats, command_surface, self.message_state.as_ref(), self.diagnostic,
            )?,
            completed = receive_judgment(&mut self.taste_completions) => accept_judgment(
                bus, epoch, command_surface, self.taste_submitter.as_deref_mut(), completed?,
            )?,
            failure = self.failures.recv() => accept_failure(
                bus, epoch, self.seats, command_surface, &mut self.wakes.lifecycle, self.diagnostic,
                failure,
            )?,
            exit = self.exits.recv() => accept_exit(
                bus, epoch, self.seats, command_surface, &mut self.wakes.lifecycle, self.diagnostic,
                exit,
            )?,
            timer = self.wakes.timers.recv() => self.wakes.accept_timer(
                command_surface, self.seats, self.diagnostic, timer,
            )?,
            () = self.quota_triggers.wait_periodic() => {
                request_quota(&mut self.controls.quota, &mut self.quota_triggers);
            }
            completed = self.controls.quota.recv() => self.controls.quota.accept(completed),
            Some(change) = self.controls.live_changes.recv() => change.record(bus, epoch),
            committed = self.timer_commits.recv() => self.accept_commit_turn(committed)?,
            completed = self.wakes.launches.recv() => launch_completion::accept(
                command_surface, self.seats, &mut self.wakes, self.diagnostic, completed,
            ).await?,
            ending = &mut self.server_task => return Err(server_outcome(ending)),
        }
        Ok(false)
    }

    /// Serve one control burst or owed inbox delivery; true once it reached dark.
    async fn control_turn(
        &mut self,
        turn: ControlDeliveryTurn,
        request: Option<serve::axum::ControlRequest>,
    ) -> CommandResult<bool> {
        if let ControlDeliveryTurn::Delivery = turn {
            deliver_owed_inbox(self.command_surface, &mut self.controls.inbox_deliveries).await?;
            return Ok(false);
        }
        dispatch_control_delivery_burst(
            self.command_surface,
            self.seats,
            &mut self.wakes,
            &mut self.controls,
            request,
            self.diagnostic,
        )
        .await
    }

    /// Accept one drained batch of supervisor notices in arrival order.
    fn accept_supervisor_reports(
        &mut self,
        reports: Vec<io::Result<super::supervisor::Notice>>,
    ) -> CommandResult<()> {
        let batch = begin_message_batch(&self.message_publisher)?;
        for report in reports {
            self.accept_supervisor_report(&batch, report)?;
        }
        commit_message_batch(batch)
    }

    fn accept_record_turn(
        &mut self,
        record: Option<lane::capture::CaptureRecord>,
    ) -> CommandResult<()> {
        let batch = begin_message_batch(&self.message_publisher)?;
        self.accept_record(&batch, record)?;
        commit_message_batch(batch)
    }

    fn accept_supervisor_report(
        &mut self,
        batch: &serve::stream::MessageBatch,
        report: io::Result<super::supervisor::Notice>,
    ) -> CommandResult<()> {
        let (bus, epoch, command_surface) = (self.bus, self.epoch, self.command_surface);
        let notice = match report {
            Ok(notice) => notice,
            Err(error) => {
                let _ = writeln!(self.diagnostic, "lane supervisor notice refused: {error}");
                return Ok(());
            }
        };
        if let super::supervisor::Notice::Idle { .. } | super::supervisor::Notice::Active { .. } =
            notice
        {
            return self
                .wakes
                .reconcile(self.command_surface, self.seats, self.diagnostic);
        }
        if let super::supervisor::Notice::Exited { lane, occupant } = notice {
            return accept_exit(
                bus,
                epoch,
                self.seats,
                command_surface,
                &mut self.wakes.lifecycle,
                self.diagnostic,
                Some(lane::capture::CaptureExit { lane, occupant }),
            );
        }
        if let super::supervisor::Notice::Failed {
            lane,
            occupant,
            failure,
        } = notice
        {
            return super::reap::account_failure(
                bus,
                epoch,
                self.seats,
                &mut self.wakes.lifecycle,
                lane::capture::CaptureExit { lane, occupant },
                &failure,
                self.diagnostic,
            );
        }
        if let super::supervisor::Notice::FastRefused {
            lane,
            occupant,
            refusal,
        } = notice
        {
            lane_surface::record_fast_refusal(bus, epoch, lane, occupant, &refusal);
            return Ok(());
        }
        let residency = current_residency(bus, epoch)?;
        let Some(record) = accept_ask(command_surface, residency.body(), notice, self.diagnostic)?
        else {
            return Ok(());
        };
        self.accept_record(batch, Some(record))
    }

    fn accept_record(
        &mut self,
        batch: &serve::stream::MessageBatch,
        record: Option<lane::capture::CaptureRecord>,
    ) -> CommandResult<()> {
        let record = record.ok_or_else(|| {
            environment_fault(
                "lane::capture-records-closed",
                "lane capture record receiver closed",
            )
        })?;
        let command_surface = self.command_surface;
        let surfaces = CaptureSurfaces::new(
            command_surface.paths,
            &command_surface.config.taste,
            &command_surface.config.steering,
            batch,
            command_surface.joining(),
        )
        .narration(command_surface.config.lane.narration());
        let views =
            message_surface::CaptureViews::new(self.message_state.as_ref(), self.diagnostic);
        let recognition = self.taste_submitter.as_deref_mut().map_or(
            message_surface::TasteRecognition::Direct,
            message_surface::TasteRecognition::Deferred,
        );
        accept_capture_record(
            self.bus,
            self.epoch,
            self.seats,
            &surfaces,
            recognition,
            &record,
            views,
        )
    }

    /// Let one journal commit settle the reconciliation and the sweep it may run.
    ///
    /// Every presented record commits a fact, so this arm fires throughout a
    /// lane's ending; the sweep asks the door before it judges any seat.
    fn accept_commit_turn(
        &mut self,
        committed: Result<JournalCommit, tokio::sync::broadcast::error::RecvError>,
    ) -> CommandResult<()> {
        let (bus, command_surface) = (self.bus, self.command_surface);
        let paths = command_surface.paths;
        let message_state = self.message_state.as_ref();
        let commit =
            self.wakes
                .accept_commit(command_surface, self.seats, self.diagnostic, committed)?;
        message_surface::app_diagnostic(
            message_surface::outbox::advance(
                &bus.reader(),
                paths,
                &self.message_publisher,
                message_state,
            ),
            self.diagnostic,
        )?;
        if self.quota_triggers.accept_commits(bus)? {
            self.controls.quota.request();
        }
        settle_spoken_acknowledgements(
            bus,
            &self.message_publisher,
            message_state,
            self.diagnostic,
        )?;
        let changed = match commit {
            Some(commit) => {
                let changes = room_catch_up::changes(bus, commit)?;
                room_catch_up::reconcile_commit(bus, paths, self.seats, &changes, self.diagnostic);
                steering::tell_line_outcomes(bus, paths, self.seats, commit, self.diagnostic);
                Some(changes.tasks)
            }
            None => None,
        };
        message_surface::advance_profile_warming(
            &bus.reader(),
            paths,
            message_state,
            self.diagnostic,
        )?;
        design_input::reconcile(
            bus,
            paths,
            self.seats,
            &mut self.design_completions,
            message_state,
            changed.as_ref(),
            self.diagnostic,
        )
    }
}

/// Pay one scheduled delivery turn, then let readiness produced during that
/// synchronous notification reach the current-thread executor before choosing again.
pub(in crate::resident) async fn deliver_owed_inbox(
    command_surface: &command_surface::Surface<'_>,
    inbox_deliveries: &mut InboxDeliveries,
) -> CommandResult<()> {
    deliver_owed_inbox_with(inbox_deliveries, |deliveries| {
        deliver_next_inbox(command_surface, deliveries)
    })
    .await
}

async fn deliver_owed_inbox_with(
    inbox_deliveries: &mut InboxDeliveries,
    deliver: impl FnOnce(&mut InboxDeliveries) -> CommandResult<()>,
) -> CommandResult<()> {
    if !inbox_deliveries.is_empty() {
        deliver(inbox_deliveries)?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

/// Decide a lane's consent request, or hand every other notice on as the
/// record it carries.
fn accept_ask(
    command_surface: &command_surface::Surface<'_>,
    residency: &wire::ResidencyHead,
    notice: super::supervisor::Notice,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<Option<lane::capture::CaptureRecord>> {
    let super::supervisor::Notice::Asked {
        lane,
        occupant,
        request,
        operation,
    } = notice
    else {
        return capture::materialize(command_surface.paths, residency, notice, diagnostic);
    };
    let ask = consent_surface::LaneAsk {
        lane,
        occupant,
        request,
        operation,
    };
    let asked = command_surface.ask_consent(residency, ask, diagnostic);
    super::deferral::defer_store_fault(asked, diagnostic, "lane consent").map(|()| None)
}

/// Journal the narration steps that came due for lanes still held by the
/// occupant they describe.
fn accept_narration_turn(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &Seats,
    command_surface: &command_surface::Surface<'_>,
    message_state: &std::sync::Mutex<message_surface::MessageState>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let steps = message_surface::lock_state(message_state)?.narration.due(
        std::time::Instant::now(),
        |lane, occupant| {
            seats
                .activity(lane)
                .is_some_and(|activity| activity.occupant() == occupant)
        },
    );
    narration::record(bus, command_surface.paths, epoch, steps, diagnostic)
}

/// Settle one finished taste adjudication with its submitter and the journal.
fn accept_judgment(
    bus: &Bus,
    epoch: CanonicalUuid,
    command_surface: &command_surface::Surface<'_>,
    taste_submitter: Option<&mut taste_surface::AdjudicationSubmitter>,
    completed: taste_surface::CompletedJudgment,
) -> CommandResult<()> {
    taste_submitter
        .expect("a completion receiver always has its submitter")
        .finished(&completed);
    taste_surface::complete_judgment(
        bus,
        command_surface.paths,
        epoch,
        &command_surface.config.taste,
        &command_surface.config.steering,
        completed,
    )
}

async fn receive_judgment(
    completions: &mut Option<&mut taste_surface::AdjudicationCompletions>,
) -> CommandResult<taste_surface::CompletedJudgment> {
    match completions.as_deref_mut() {
        Some(completions) => completions.recv().await,
        None => std::future::pending().await,
    }
}

fn accept_timer_fired(
    bus: &Bus,
    command_surface: &command_surface::Surface<'_>,
    lifecycle: &mut lifecycle::Runtime,
    fired: timers::Fired,
) -> CommandResult<()> {
    match fired {
        timers::Fired::LaneResume(lane) => command_surface.expire_lifecycle_hold(lifecycle, lane),
        timers::Fired::Steering => {
            let residency = bus
                .residency_head()
                .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
            if let Some(residency) = residency {
                steering::notify_inboxes(command_surface.paths, residency.body());
            }
            Ok(())
        }
        timers::Fired::ClaimLapse(Some(lapsed)) => {
            let residency = bus
                .residency_head()
                .map_err(|error| store_fault("claim::residency-unreadable", &error.to_string()))?;
            if let Some((lane, notice)) = residency.and_then(|head| lapsed.notice(head.body())) {
                crate::resident::supervisor::command::claim_lapse(
                    command_surface.paths,
                    lane,
                    notice,
                );
            }
            Ok(())
        }
        timers::Fired::Stale
        | timers::Fired::DeferredTaskWoken(_)
        | timers::Fired::ClaimLapse(None)
        | timers::Fired::ClaimRenew
        | timers::Fired::TaskWake(_) => Ok(()),
    }
}

async fn stop_surface(
    publication: SurfacePublication<'_>,
    shutdown: oneshot::Sender<()>,
    server_task: tokio::task::JoinHandle<Result<(), io::Error>>,
) -> CommandResult<()> {
    drop(publication);
    let _ = shutdown.send(());
    match server_task.await {
        Ok(Ok(())) => Ok(()),
        ending => Err(server_outcome(ending)),
    }
}

fn begin_message_batch(
    publisher: &serve::stream::MessagePublisher,
) -> CommandResult<serve::stream::MessageBatch> {
    publisher
        .batch()
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))
}

fn commit_message_batch(batch: serve::stream::MessageBatch) -> CommandResult<()> {
    batch
        .commit()
        .map(drop)
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))
}

fn accept_failure(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    command_surface: &command_surface::Surface<'_>,
    lifecycle: &mut lifecycle::Runtime,
    diagnostic: &mut dyn io::Write,
    failure: Option<lane::capture::CaptureFailure>,
) -> CommandResult<()> {
    let failure = failure.ok_or_else(|| {
        environment_fault(
            "lane::capture-failures-closed",
            "lane capture failure receiver closed",
        )
    })?;
    let accounted = (|| {
        if !lane_surface::renewal_requested_exit(bus, epoch, failure.lane, failure.occupant)? {
            command_surface.observe_lifecycle_death(
                lifecycle,
                seats,
                failure.lane,
                failure.occupant,
                diagnostic,
            )?;
        }
        lane_surface::capture_failed(bus, epoch, seats, &failure)
    })();
    super::deferral::defer_store_fault(accounted, diagnostic, "capture failure accounting")
}

fn accept_exit(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    command_surface: &command_surface::Surface<'_>,
    lifecycle: &mut lifecycle::Runtime,
    diagnostic: &mut dyn io::Write,
    exit: Option<lane::capture::CaptureExit>,
) -> CommandResult<()> {
    let exit = exit.ok_or_else(|| {
        environment_fault(
            "lane::capture-exits-closed",
            "lane process exit receiver closed",
        )
    })?;
    let accounted = super::reap::account_exit(
        bus,
        command_surface.paths,
        (
            &command_surface.config.steering,
            epoch,
            command_surface.joining(),
        ),
        seats,
        lifecycle,
        exit,
        diagnostic,
    );
    super::deferral::defer_store_fault(accounted, diagnostic, "exit accounting")
}

fn request_quota(quota: &mut quota::Runtime, triggers: &mut quota::Triggers) {
    triggers.accept_periodic();
    quota.request();
}

/// Follow every commit with the cards that spoke an acknowledgement ahead of
/// the journal, so any committed retirement re-presents its card from the
/// journal disposition. The app being unavailable is a
/// diagnostic, as it is for live publication, not a resident fault.
fn settle_spoken_acknowledgements(
    bus: &Bus,
    message_publisher: &serve::stream::MessagePublisher,
    message_state: &std::sync::Mutex<message_surface::MessageState>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let settled = (|| {
        let batch = begin_message_batch(message_publisher)?;
        message_surface::settle_pending_presentations(bus, &batch, message_state)?;
        commit_message_batch(batch)
    })();
    message_surface::app_diagnostic(settled, diagnostic)
}

/// The resident runtimes a wake reconciliation advances: the timers it
/// re-arms, the lifecycle it resolves, the launches it starts, the supervisor
/// door its seat sweep asks first, and the reconciliation demand it settles.
struct Wakes {
    timers: timers::Runtime,
    lifecycle: lifecycle::Runtime,
    launches: super::launch_runtime::Runtime,
    door: capture::SupervisorReports,
    reconciliation: lifecycle::Reconciliation,
}

impl Wakes {
    /// Consume one fired timer, then reconcile the lifecycle it may have moved.
    fn accept_timer(
        &mut self,
        command_surface: &command_surface::Surface<'_>,
        seats: &mut Seats,
        diagnostic: &mut dyn io::Write,
        timer: Option<bus::timer::TimerEvent>,
    ) -> CommandResult<()> {
        let timer = timer.ok_or_else(|| {
            environment_fault(
                "bus::timer-events-closed",
                "the production timer event stream closed while the resident was live",
            )
        })?;
        let bus = command_surface.bus();
        let fired = self
            .timers
            .consume(bus, timer, Some(command_surface.repository()))?;
        accept_timer_fired(bus, command_surface, &mut self.lifecycle, fired)?;
        self.settle(command_surface, seats, diagnostic)
    }

    /// Reconcile for one journal commit the timers do not already cover, then
    /// settle the demand it answers; a lagged stream reconciles and settles
    /// unconditionally.
    fn accept_commit(
        &mut self,
        command_surface: &command_surface::Surface<'_>,
        seats: &mut Seats,
        diagnostic: &mut dyn io::Write,
        committed: Result<JournalCommit, tokio::sync::broadcast::error::RecvError>,
    ) -> CommandResult<Option<JournalCommit>> {
        let commit = match committed {
            Ok(commit) => Some(commit),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Err(environment_fault(
                    "bus::lifecycle-commits-closed",
                    "the durable commit stream closed while the resident was live",
                ));
            }
        };
        command_surface.refresh_reads()?;
        if commit.is_none_or(|commit| !self.timers.covers(commit)) {
            super::deferral::defer_store_fault(
                self.reconcile(command_surface, seats, diagnostic),
                diagnostic,
                "lifecycle reconciliation",
            )?;
        }
        match commit {
            Some(commit) => self.reconciliation.settle_commit(commit),
            None => self.reconciliation.settle(),
        }
        Ok(commit)
    }

    /// Settle the reconciliation demand a control turn is waiting on.
    fn settle_for_control(
        &mut self,
        command_surface: &command_surface::Surface<'_>,
        seats: &mut Seats,
        diagnostic: &mut dyn io::Write,
    ) -> CommandResult<()> {
        if self.reconciliation.control_settles() {
            self.settle(command_surface, seats, diagnostic)?;
        }
        Ok(())
    }

    /// Reconcile wakes and settle the pending demand.
    fn settle(
        &mut self,
        command_surface: &command_surface::Surface<'_>,
        seats: &mut Seats,
        diagnostic: &mut dyn io::Write,
    ) -> CommandResult<()> {
        // A deferred sweep leaves the demand pending, so the next control turn
        // drives it again instead of a timer having to notice.
        let swept = self.reconcile(command_surface, seats, diagnostic);
        if swept.is_ok() {
            self.reconciliation.settle();
        }
        super::deferral::defer_store_fault(swept, diagnostic, "lifecycle reconciliation")
    }

    fn reconcile(
        &mut self,
        command_surface: &command_surface::Surface<'_>,
        seats: &mut Seats,
        diagnostic: &mut dyn io::Write,
    ) -> CommandResult<()> {
        let bus = command_surface.bus();
        self.timers.synchronize(bus)?;
        super::reap::reap_ended_seats(
            command_surface,
            seats,
            &mut self.lifecycle,
            &mut self.door,
            diagnostic,
        )?;
        let (paths, epoch) = (command_surface.paths, command_surface.epoch());
        lane_surface::reconcile_seats(
            bus,
            paths,
            &command_surface.config.steering,
            (epoch, command_surface.joining()),
            seats,
            diagnostic,
        )?;
        let wake = command_surface.reconcile_lifecycle(
            &mut self.lifecycle,
            seats,
            &mut self.launches,
            diagnostic,
        )?;
        self.timers.synchronize_wake(wake)
    }
}

pub(in crate::resident) fn deliver_next_inbox(
    command_surface: &command_surface::Surface<'_>,
    inbox_deliveries: &mut InboxDeliveries,
) -> CommandResult<()> {
    let residency = command_surface.inbox_residency()?;
    let Some(delivery) = inbox_deliveries.next(&residency) else {
        return Ok(());
    };
    command_surface.notify_inbox(delivery.lane());
    inbox_deliveries.complete(delivery);
    Ok(())
}
