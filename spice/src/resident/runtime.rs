use super::*;

#[cfg(test)]
mod canvas_selection_tests;
mod identity;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimePaths {
    pub(super) worktree: PathBuf,
    pub(super) common: PathBuf,
    pub(super) runtime: PathBuf,
    pub(super) live: PathBuf,
    pub(super) data: PathBuf,
    pub(super) journal: PathBuf,
    pub(super) resident: PathBuf,
    pub(super) serve: PathBuf,
    pub(super) lane_serve: PathBuf,
    pub(super) degraded: PathBuf,
    pub(crate) tape: PathBuf,
    pub(super) supervisors: PathBuf,
    pub(super) steering_media: PathBuf,
    pub(super) stage: PathBuf,
}

struct ResidentClientPaths {
    resident: PathBuf,
    serve: PathBuf,
}

impl ResidentClientPaths {
    pub(super) fn from_repository(repository: &Repository) -> CommandResult<Self> {
        let serve = repository
            .admit_resident_client()
            .map_err(|error| crate::state_failure(&error).outcome())?;
        let resident = serve.with_file_name("resident");
        Ok(Self { resident, serve })
    }
}

pub(super) struct SurfaceConfig {
    serve: serve::Config,
    pub(super) steering: steer::Config,
    pub(super) taste: taste::Bag,
    pub(super) judge_enabled: bool,
    pub(super) consent: consent::Policy,
    pub(super) allocation: board::NextConfig,
    pub(super) lane: lane::Config,
    pub(super) lock: sill::LockConfig,
    pub(super) omp_binary: String,
    /// The process environment `main` captured, for every child and Git
    /// operation this resident runs.
    pub(super) ambient: Ambient,
}

struct SurfaceLaunch<'a> {
    app: serve::canvas::CanvasAssets,
    version: wire::Version,
    binding: AppBinding,
    resident_lock: &'a ResidentLock,
    /// Whether the operator asked this start to also open the app.
    ///
    /// Discovery is announced either way, so this is not part of the binding:
    /// it decides who dials the published address, not which address gets
    /// published or what admits a caller to it.
    open: bool,
    progress: &'a mut dyn io::Write,
    endpoint: &'a mut dyn io::Write,
    daemon: Option<ChildStatus>,
}

// Runtime policy and trust identity derive from this one captured generation.
pub(super) struct StartupSurface {
    pub(super) config: SurfaceConfig,
    pub(super) trust: crate::trust_command::Surface,
}

#[cfg(test)]
pub(super) fn load_startup_surface(
    repository: &Repository,
) -> Result<StartupSurface, spice::config::LoadError> {
    load_startup_surface_with(repository, &spice::ambient::capture(), || {})
}

/// Create the runtime directory, then capture the startup surface from it.
///
/// The local configuration document lives beneath the runtime directory, and
/// a capture records how far a missing document's path existed. A capture
/// taken before the directory existed, such as the invocation's own, would
/// name a generation that creating it then moves, so every guarded startup
/// write would refuse `trust::surface-moved`.
pub(super) fn prepare_startup(
    repository: &Repository,
    paths: &RuntimePaths,
    ambient: &Ambient,
) -> Result<StartupSurface, Outcome<()>> {
    fs::create_dir_all(&paths.runtime)
        .map_err(|error| environment_fault("resident::runtime-unavailable", &error.to_string()))?;
    let loaded = spice::config::capture_repository(repository, &[])
        .map_err(|error| configuration_refusal(&error))?;
    Ok(startup_surface_from_loaded(&loaded, ambient.clone()))
}

#[cfg(test)]
pub(super) fn load_startup_surface_with<F>(
    repository: &Repository,
    ambient: &Ambient,
    after_capture: F,
) -> Result<StartupSurface, spice::config::LoadError>
where
    F: FnOnce(),
{
    let loaded = spice::config::capture_repository(repository, &[])?;
    after_capture();
    Ok(startup_surface_from_loaded(&loaded, ambient.clone()))
}

fn startup_surface_from_loaded(loaded: &spice::config::Loaded, ambient: Ambient) -> StartupSurface {
    StartupSurface {
        config: SurfaceConfig {
            serve: *loaded.resolved().root().serve(),
            steering: loaded.resolved().root().steer().clone(),
            taste: loaded.resolved().root().taste().bag().clone(),
            judge_enabled: loaded.resolved().root().judge().enabled().unwrap_or(false),
            consent: loaded.resolved().root().consent().policy().clone(),
            allocation: *loaded.resolved().root().board().next(),
            lane: loaded.resolved().root().lane().clone(),
            lock: loaded.resolved().root().lock().clone(),
            omp_binary: loaded
                .resolved()
                .root()
                .vendor()
                .omp()
                .binary()
                .as_str()
                .to_owned(),
            ambient,
        },
        trust: crate::trust_command::Surface::from_loaded(loaded),
    }
}

pub(super) fn require_current_surface(
    repository: &Repository,
    surface: &crate::trust_command::Surface,
) -> Result<(), GuardError> {
    match surface.is_current(repository) {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(GuardError::refused("trust::surface-moved")),
    }
}

impl RuntimePaths {
    pub(super) fn lane_tape(&self, lane: CanonicalUuid) -> PathBuf {
        spice::repository::lane_tape(&self.tape, lane)
    }

    pub(super) fn lane_occupants(&self, lane: CanonicalUuid) -> PathBuf {
        spice::repository::lane_occupants(&self.supervisors, lane)
    }

    #[cfg(test)]
    pub(super) fn discover(start: &Path) -> CommandResult<Self> {
        let repository = Repository::discover(start).map_err(|error| {
            environment_fault(spice::repository::ResolveError::REASON, &error.to_string())
        })?;
        Self::from_repository(&repository)
    }

    pub(crate) fn from_repository(repository: &Repository) -> CommandResult<Self> {
        let state = repository
            .admit_resident_state()
            .map_err(|error| crate::state_failure(&error).outcome())?;
        let journal = state.journal().to_path_buf();
        let data = journal
            .parent()
            .expect("the journal has one canonical parent")
            .to_path_buf();
        Ok(Self {
            worktree: repository.worktree_root().to_path_buf(),
            common: repository.common_dir().to_path_buf(),
            runtime: state.runtime().to_path_buf(),
            live: state.live().to_path_buf(),
            data,
            journal,
            resident: state.resident().to_path_buf(),
            serve: state.serve().to_path_buf(),
            lane_serve: state.lane_serve().to_path_buf(),
            degraded: state.degraded().to_path_buf(),
            tape: state.lanes().to_path_buf(),
            supervisors: state.lanes().to_path_buf(),
            steering_media: state.lanes().to_path_buf(),
            stage: repository.worktree_root().join("dist/stage"),
        })
    }

    /// The task artifact store under this resident's runtime root.
    pub(super) fn artifacts(&self) -> artifact::Store {
        let (data, lock) = spice::repository::StatePaths::task_artifacts_under(&self.runtime);
        artifact::Store::new(data, lock)
    }

    /// Browser speech contract for one admitted worktree, read from its
    /// current configuration.
    pub(super) fn speech_for_worktree(
        &self,
        worktree: &str,
    ) -> Result<serve::SpeechConfig, String> {
        let repository = Repository::discover(worktree).map_err(|error| error.to_string())?;
        if repository.common_dir() != self.common
            || repository.worktree_root() != Path::new(worktree)
        {
            return Err(
                "speech worktree no longer names one current repository worktree root".to_owned(),
            );
        }
        spice::config::load_repository(&repository, &[])
            .map(|resolved| resolved.root().say().clone())
            .map_err(|error| error.to_string())
    }

    pub(super) fn clear_stale_surfaces(&self) -> CommandResult<()> {
        remove_if_present(&self.serve)
            .map_err(|error| environment_fault("serve::stale-discovery", &error.to_string()))?;
        remove_tree_if_present(&self.lane_serve)
            .map_err(|error| environment_fault("serve::stale-lane-discovery", &error.to_string()))
    }
}

#[derive(Debug)]
pub(super) struct ResidentLock {
    possession: Possession,
}

impl ResidentLock {
    pub(super) fn acquire(paths: &RuntimePaths) -> CommandResult<Self> {
        Self::acquire_with(paths, || {})
    }

    pub(super) fn acquire_with(
        paths: &RuntimePaths,
        after_contention: impl FnMut(),
    ) -> CommandResult<Self> {
        fs::create_dir_all(&paths.live).map_err(|error| {
            environment_fault("resident::runtime-unavailable", &error.to_string())
        })?;
        let name = LockName::new("resident").expect("resident is a non-empty lock name");
        let lock = LockFile::new(name, &paths.resident);
        match lock
            .try_acquire_with(after_contention)
            .map_err(|error| environment_fault("resident::lock-unavailable", &error.to_string()))?
        {
            Acquisition::Acquired(possession) => {
                possession
                    .rewrite_record(holder_record(std::process::id(), None).as_bytes())
                    .map_err(|error| {
                        environment_fault("resident::holder-unwritable", &error.to_string())
                    })?;
                Ok(Self { possession })
            }
            Acquisition::Contended => {
                // Holder rewrites never publish an empty intermediate state,
                // so a contender arriving during the pending-to-epoch change
                // can still name the holder instead of taking this fallback.
                let observed = fs::read_to_string(&paths.resident)
                    .ok()
                    .map(|holder| holder.trim().to_owned())
                    .filter(|holder| !holder.is_empty())
                    .unwrap_or_else(|| "holder metadata unavailable".to_owned());
                Err(Outcome::Refusal(Refusal::bare(
                    wire::RefusalDiagnostic::new(
                        "resident::already-up".to_owned(),
                        Some("resident-lock".to_owned()),
                        Some("unheld".to_owned()),
                        Some(observed),
                    ),
                )))
            }
        }
    }

    pub(super) fn set_epoch(&self, epoch: CanonicalUuid) -> io::Result<()> {
        self.possession
            .rewrite_record(holder_record(std::process::id(), Some(epoch)).as_bytes())
    }

    pub(super) fn set_launch(
        &self,
        epoch: CanonicalUuid,
        launch: ResidentLaunch,
    ) -> io::Result<()> {
        self.possession
            .rewrite_record(launched_holder_record(std::process::id(), epoch, launch).as_bytes())
    }
}

fn record_bound_launch(
    resident_lock: &ResidentLock,
    epoch: CanonicalUuid,
    server: &serve::axum::Server,
    access: ResidentAccess,
) -> CommandResult<()> {
    let bound = server.local_addr().map_err(|error| {
        environment_fault("serve::bound-address-unavailable", &error.to_string())
    })?;
    resident_lock
        .set_launch(epoch, ResidentLaunch::new(bound, access))
        .map_err(|error| environment_fault("resident::holder-unwritable", &error.to_string()))
}

impl Drop for ResidentLock {
    fn drop(&mut self) {
        let _ = self.possession.file().set_len(0);
        let _ = self.possession.file().sync_data();
    }
}

#[derive(Debug)]
struct PublishedDiscovery {
    path: PathBuf,
    lane_root: PathBuf,
}

impl Drop for PublishedDiscovery {
    fn drop(&mut self) {
        let _ = remove_if_present(&self.path);
        let _ = remove_tree_if_present(&self.lane_root);
    }
}

/// The address the surface listens on when nothing names one.
///
/// Loopback, because reaching this surface from another machine is a decision
/// only the operator can make with `--host`. The kernel picks the port, so a
/// resident started in another repository, or a demo, never contends for a port
/// another resident already answers on. An operator who wants a URL that
/// survives restarts names it with `--port`; discovery and the `up` answer carry
/// whatever was bound.
pub(crate) const EPHEMERAL_LOOPBACK: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

pub(crate) struct AppBinding {
    listen: SocketAddr,
    access: AppAccess,
}

pub(crate) struct AppAccess {
    policy: serve::axum::Access,
    kind: ResidentAccess,
    pub(crate) token: Option<String>,
}

enum AppSelection {
    Installed,
    Staged,
}

impl AppSelection {
    fn resolve(self, repository: &Repository) -> CommandResult<serve::canvas::CanvasAssets> {
        self.resolve_with(repository.worktree_root(), spice::version::canvas())
    }

    fn resolve_with(
        self,
        worktree: &Path,
        linked: Option<&Result<serve::canvas::EmbeddedArchive, rack::Error>>,
    ) -> CommandResult<serve::canvas::CanvasAssets> {
        match self {
            Self::Installed => {
                let archive = match linked {
                    None => {
                        return Err(environment_fault(
                            "serve::canvas-archive-absent",
                            "this installed executable links no canvas archive; reinstall it with `spice dev xtask -- install`",
                        ));
                    }
                    Some(Err(error)) => {
                        return Err(environment_fault(
                            "serve::canvas-archive-invalid",
                            &error.to_string(),
                        ));
                    }
                    Some(Ok(archive)) => archive.clone(),
                };
                Ok(serve::canvas::CanvasAssets::EmbeddedArchive(archive))
            }
            Self::Staged => serve::AppSource::resolve(worktree)
                .map(serve::canvas::CanvasAssets::DevelopmentDirectory)
                .map_err(|error| {
                    environment_fault("serve::app-source-invalid", &error.to_string())
                }),
        }
    }
}

impl AppBinding {
    pub(crate) fn local(listen: SocketAddr) -> Self {
        Self {
            listen,
            access: AppAccess {
                policy: serve::axum::Access::local(),
                kind: ResidentAccess::Local,
                token: None,
            },
        }
    }
}

pub(crate) fn up(
    leaf: &clap::ArgMatches,
    child: Option<ChildLaunch>,
    test_app_stage: bool,
    progress: &mut dyn io::Write,
    endpoint: &mut dyn io::Write,
    ambient: &Ambient,
) -> Outcome<()> {
    let listen = match requested_listen(leaf) {
        Ok(listen) => listen,
        Err(outcome) => return outcome,
    };
    let handoff_token = child.as_ref().and_then(|child| child.token.clone());
    let access = match requested_access(leaf, listen, handoff_token) {
        Ok(access) => access,
        Err(outcome) => return outcome,
    };
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("resident::cwd-unavailable", &error.to_string()),
    };
    let app = if test_app_stage {
        AppSelection::Staged
    } else {
        AppSelection::Installed
    };
    up_from_with(
        &current,
        UpRequest {
            app,
            binding: AppBinding { listen, access },
            open: leaf.get_flag("open"),
            ambient,
        },
        progress,
        endpoint,
        child.map(|child| child.status),
    )
}

/// The address `--host` and `--port` asked for, each defaulted on its own.
///
/// Both are parsed strictly: what the operator named is what gets bound, so a
/// host that is not an address of this machine has to fail here by name rather
/// than resolve to something they did not ask for.
pub(crate) fn requested_listen(leaf: &clap::ArgMatches) -> CommandResult<SocketAddr> {
    let host = match leaf.get_one::<String>("host") {
        Some(host) => host.parse::<IpAddr>().map_err(|_| {
            listen_refusal(
                "resident::host-invalid",
                "one IP address to listen on",
                host,
            )
        })?,
        None => EPHEMERAL_LOOPBACK.ip(),
    };
    let port = match leaf.get_one::<String>("port") {
        Some(port) => port.parse::<u16>().map_err(|_| {
            listen_refusal("resident::port-invalid", "one port from 0 to 65535", port)
        })?,
        None => EPHEMERAL_LOOPBACK.port(),
    };
    Ok(SocketAddr::new(host, port))
}

/// The access a resident serves, from argv and, for a detached child, the
/// token its launcher handed over privately; both at once is a launcher bug.
pub(crate) fn requested_access(
    leaf: &clap::ArgMatches,
    listen: SocketAddr,
    handoff_token: Option<String>,
) -> CommandResult<AppAccess> {
    let argument_token = leaf.get_one::<String>("auth-token").cloned();
    if argument_token.is_some() && handoff_token.is_some() {
        return Err(environment_fault(
            "resident::daemon-handoff-ambiguous",
            "the detached child received authentication in both argv and its private handoff",
        ));
    }
    let token = handoff_token.or(argument_token);
    if let Some(token) = token.as_deref()
        && !auth_token_is_valid(token)
    {
        return Err(access_refusal(
            "resident::auth-token-invalid",
            None,
            &format!(
                "use {AUTH_TOKEN_MIN_BYTES} to {AUTH_TOKEN_MAX_BYTES} URL-safe \
                 characters: letters, digits, hyphen, period, underscore, or tilde"
            ),
        ));
    }
    let allow_insecure = leaf.get_flag("allow-insecure-bind");
    if !listen.ip().is_loopback() && !allow_insecure {
        return Err(access_refusal(
            "resident::insecure-bind-consent-required",
            Some(&listen.to_string()),
            "add --allow-insecure-bind only when this network exposure is intended",
        ));
    }
    let kind = if token.is_some() {
        ResidentAccess::Authenticated
    } else if !listen.ip().is_loopback() && allow_insecure {
        ResidentAccess::Insecure
    } else {
        ResidentAccess::Local
    };
    let policy = match (&kind, &token) {
        (ResidentAccess::Authenticated, Some(token)) => {
            serve::axum::Access::authenticated(token.clone())
        }
        (ResidentAccess::Insecure, None) => serve::axum::Access::insecure(),
        (ResidentAccess::Local, None) => serve::axum::Access::local(),
        _ => unreachable!("access kind and token derive together"),
    };
    Ok(AppAccess {
        policy,
        kind,
        token,
    })
}

fn access_refusal(reason: &str, observed: Option<&str>, detail: &str) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("requested-app-access".to_owned()),
            Some("loopback, or an explicitly consented non-loopback bind".to_owned()),
            observed.map(str::to_owned),
        )
        .with_detail(detail),
    ))
}

fn listen_refusal(reason: &str, expected: &str, observed: &str) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        reason.to_owned(),
        Some("requested-listen-address".to_owned()),
        Some(expected.to_owned()),
        Some(observed.to_owned()),
    )))
}

/// Run the explicit demo/test surface against its worktree-built app stage.
pub(crate) fn up_from(current: &Path, binding: AppBinding, ambient: &Ambient) -> Outcome<()> {
    up_from_with(
        current,
        UpRequest {
            app: AppSelection::Staged,
            binding,
            open: false,
            ambient,
        },
        &mut io::sink(),
        &mut io::sink(),
        None,
    )
}

/// What one resident start serves, before any repository authority is read.
struct UpRequest<'a> {
    app: AppSelection,
    binding: AppBinding,
    /// Whether the operator asked this start to also open the app.
    open: bool,
    ambient: &'a Ambient,
}

fn up_from_with(
    current: &Path,
    request: UpRequest<'_>,
    progress: &mut dyn io::Write,
    endpoint: &mut dyn io::Write,
    daemon: Option<ChildStatus>,
) -> Outcome<()> {
    let UpRequest {
        app,
        binding,
        open,
        ambient,
    } = request;
    let repository = match Repository::discover(current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, &error.to_string());
        }
    };
    let app = match app.resolve(&repository) {
        Ok(app) => app,
        Err(outcome) => return outcome,
    };
    let paths = match RuntimePaths::from_repository(&repository) {
        Ok(paths) => paths,
        Err(outcome) => return outcome,
    };
    let startup_surface = match prepare_startup(&repository, &paths, ambient) {
        Ok(surface) => surface,
        Err(outcome) => return outcome,
    };
    if let Err(error) = fs::create_dir_all(&paths.data) {
        return environment_fault("resident::data-unavailable", &error.to_string());
    }
    let resident_lock = match ResidentLock::acquire(&paths) {
        Ok(lock) => lock,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = paths.clear_stale_surfaces() {
        return outcome;
    }

    let mut journal = match journal::Journal::open(&paths.journal) {
        Ok(journal) => journal,
        Err(error) => return journal_open_failure(error),
    };
    let migration = journal.migration();
    if let Err(outcome) = steering::admit_index(&mut journal) {
        return outcome;
    }
    let version = match identity::app_version_from_journal(&app, &journal) {
        Ok(version) => version,
        Err(outcome) => return outcome,
    };
    let epoch = new_uuid();
    if let Err(error) = resident_lock.set_epoch(epoch) {
        return environment_fault("resident::holder-unwritable", &error.to_string());
    }
    let bus = match Bus::start(journal) {
        Ok(bus) => bus,
        Err(error) => return store_fault("bus::start-failed", &error.to_string()),
    };
    let startup = match commit_startup(&bus, &repository, &startup_surface.trust, epoch, migration)
        .and_then(|startup| admit_linked_rack(&bus, epoch, &app).map(|()| startup))
    {
        Ok(startup) => startup,
        Err(outcome) => return outcome,
    };

    let mut seats = Seats::new();
    // Every resident diagnostic reaches the log through this one stream, so
    // the duplicate rollup is installed once, here, rather than at each of
    // the callers that supply the stream. The wrapper is settled below,
    // before the surface's answer is returned.
    let mut progress = log::Rollup::new(progress);
    let surface = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run_surface(
            &bus,
            &repository,
            &paths,
            epoch,
            &startup_surface.config,
            &mut seats,
            SurfaceLaunch {
                app,
                version,
                binding,
                resident_lock: &resident_lock,
                open,
                progress: &mut progress,
                endpoint,
                daemon,
            },
        )),
        Err(error) => Err(environment_fault(
            "resident::runtime-failed",
            &error.to_string(),
        )),
    };
    drop(progress);
    let answer = settle_surface(&bus, &paths, epoch, startup, &mut seats, surface);
    let shutdown = bus.shutdown();
    drop(resident_lock);
    if let Err(error) = shutdown {
        return store_fault("bus::shutdown-failed", &error.to_string());
    }
    answer
}

fn journal_open_failure(error: journal::OpenError) -> Outcome<()> {
    match error {
        journal::OpenError::Migration(migration) => Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "journal::schema-refused".to_owned(),
                None,
                Some(format!("Rust journal schema {}", journal::SCHEMA_VERSION,)),
                Some(migration.to_string()),
            )
            .with_detail(concat!(
                "finish or replace the unfinalized or incompatible Rust authority ",
                "before starting the resident"
            )),
        )),
        error => store_fault("journal::open-failed", &error.to_string()),
    }
}

fn configuration_refusal(error: &spice::config::LoadError) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        "resident::configuration-refused".to_owned(),
        Some("effective-configuration".to_owned()),
        Some("one admitted effective repository configuration".to_owned()),
        Some(error.diagnostic()),
    )))
}

pub(super) fn settle_surface(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    startup: StartupCommit,
    seats: &mut Seats,
    surface: CommandResult<()>,
) -> Outcome<()> {
    match surface {
        Ok(()) => committed_reply(startup.first, startup.last),
        Err(original) => match drive_shutdown(bus, paths, epoch, seats, Actor::Spice) {
            Outcome::Reply(_) => original,
            compensation => store_fault(
                "resident::compensation-failed",
                &format!("startup failed as {original:?}; compensation failed as {compensation:?}"),
            ),
        },
    }
}

/// Stop the resident serving the current directory and answer with the
/// state it moved from and the dark it reached.
pub(crate) fn down(contract: &crate::cli::Contract, ambient: &Ambient) -> Outcome<StateAnswer> {
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            return environment_fault("resident::cwd-unavailable", &error.to_string());
        }
    };
    down_from(&current, contract, ambient)
}

pub(crate) fn down_from(
    current: &Path,
    contract: &crate::cli::Contract,
    ambient: &Ambient,
) -> Outcome<StateAnswer> {
    down_from_with(current, contract, ambient, crate::command_client::execute)
}

/// Stop the resident serving `current` through the operator connection even
/// while a lane identity is ambient, for callers whose root never seats
/// lanes: the demo drives a scratch root that publishes only an operator app
/// server, so honoring the ambient seat would dial a discovery path the
/// scratch root cannot publish and strand the resident it launched.
pub(crate) fn down_from_operator(
    current: &Path,
    contract: &crate::cli::Contract,
    ambient: &Ambient,
) -> Outcome<StateAnswer> {
    down_from_with(
        current,
        contract,
        ambient,
        crate::command_client::execute_operator,
    )
}

fn down_from_with(
    current: &Path,
    contract: &crate::cli::Contract,
    ambient: &Ambient,
    execute: fn(
        &Path,
        &crate::cli::Contract,
        crate::command_client::SelectedCommand<'_>,
        &Ambient,
    ) -> Outcome<CommandAnswer>,
) -> Outcome<StateAnswer> {
    let repository = match Repository::discover(current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, &error.to_string());
        }
    };
    if let Err(outcome) = super::keeper::hold_stopped(&repository) {
        return typed_outcome(outcome);
    }
    let paths = match ResidentClientPaths::from_repository(&repository) {
        Ok(paths) => paths,
        Err(outcome) => return typed_outcome(outcome),
    };
    let outcome = execute(
        &paths.serve,
        contract,
        crate::command_client::SelectedCommand::Down,
        ambient,
    );
    let outcome = decode_dark_outcome(outcome);
    if matches!(outcome, Outcome::Reply(_))
        && let Err(release) = wait_for_release(&paths)
    {
        return typed_outcome(release);
    }
    outcome
}

fn wait_for_release(paths: &ResidentClientPaths) -> CommandResult<()> {
    let possession = acquire_released_resident(paths)?;
    if paths.serve.exists() {
        return Err(environment_fault(
            "serve::release-incomplete",
            "resident lock was released while discovery remained published",
        ));
    }
    drop(possession);
    Ok(())
}

/// Wait one client bound for the resident to surrender its runtime lock.
fn acquire_released_resident(paths: &ResidentClientPaths) -> CommandResult<Possession> {
    let release_bound = crate::cli::answer_bound(CommandKind::UpChange);

    let name = LockName::new("resident").expect("resident is a non-empty lock name");
    let lock = LockFile::new(name, &paths.resident);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("spice-release-wait".to_owned())
        .spawn(move || {
            let _ = sender.send(lock.acquire());
        })
        .map_err(|error| environment_fault("resident::release-failed", &error.to_string()))?;

    match receiver.recv_timeout(release_bound) {
        Ok(Ok(possession)) => Ok(possession),
        Ok(Err(error)) => Err(environment_fault(
            "resident::release-failed",
            &error.to_string(),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(environment_fault(
            "serve::deadline-elapsed",
            &format!("resident did not release within the {release_bound:?} client bound"),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(environment_fault(
            "resident::release-failed",
            "resident release waiter ended without reporting lock acquisition",
        )),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StartupCommit {
    first: u64,
    last: u64,
}

/// Admit the durable subscription table exactly as the journal holds it.
///
/// The table carries one runtime policy subscription per resident team in team
/// order. Duplicate, unordered, partial, or extra authority is refused instead
/// of repaired; no policy is derived here.
pub(super) fn startup_subscriptions(
    lanes: &[wire::LaneResidency],
    stored: Vec<wire::TeamSubscription>,
) -> CommandResult<Vec<wire::TeamSubscription>> {
    if stored.windows(2).any(|pair| pair[0].team == pair[1].team) {
        return Err(store_fault(
            "team::subscription-ambiguous",
            "one team has more than one durable runtime policy subscription",
        ));
    }
    let team_ids = lanes
        .iter()
        .map(|row| row.team)
        .collect::<std::collections::BTreeSet<_>>();
    if !stored
        .iter()
        .map(|subscription| subscription.team)
        .eq(team_ids)
    {
        return Err(store_fault(
            "team::subscription-misaligned",
            "the durable runtime policy table is not one row per resident team in team order",
        ));
    }
    for subscription in &stored {
        let teamed = wire::head::teamed(
            subscription.team,
            lanes
                .iter()
                .filter(|row| row.team == subscription.team)
                .map(|row| row.lane),
        );
        if teamed != subscription.formed_at.is_some() {
            return Err(store_fault(
                "team::formation-misaligned",
                "a durable team names one formation instant exactly while it is teamed",
            ));
        }
    }
    Ok(stored)
}

pub(super) fn commit_startup(
    bus: &Bus,
    repository: &Repository,
    surface: &crate::trust_command::Surface,
    epoch: CanonicalUuid,
    migration: Option<journal::Migration>,
) -> CommandResult<StartupCommit> {
    commit_startup_with(bus, repository, surface, epoch, migration, || {})
}

pub(super) fn commit_startup_with<F>(
    bus: &Bus,
    repository: &Repository,
    surface: &crate::trust_command::Surface,
    epoch: CanonicalUuid,
    migration: Option<journal::Migration>,
    before_seating: F,
) -> CommandResult<StartupCommit>
where
    F: FnOnce(),
{
    let (lanes, subscriptions, topology_revision) = bus
        .residency_head()
        .map_err(|error| store_fault("resident::head-unreadable", &error.to_string()))?
        .map_or_else(
            || Ok((Vec::new(), Vec::new(), 0)),
            |head| {
                let head = head.into_body();
                let subscriptions = startup_subscriptions(&head.lanes, head.subscriptions)?;
                Ok((head.lanes, subscriptions, head.topology_revision))
            },
        )?;
    let mut first = None;
    let mut cause = FactCause::Root;
    if let Some(migration) = migration {
        let mut write = write(Actor::Spice, epoch, Producer::ResidentRecognition)?;
        write
            .fact(
                wire::body::JournalMigrated {
                    from: migration.from(),
                    to: migration.to(),
                    epoch,
                },
                cause,
            )
            .map_err(|error| store_fault("journal::write-invalid", &error.to_string()))?;
        let committed = commit(bus, write)?;
        first = Some(committed.first());
        cause = FactCause::Existing(seq(committed.last()));
    }

    let trust = bus
        .trust_head(surface.digest())
        .map_err(|error| store_fault("trust::head-unreadable", &error.to_string()))?;
    if trust.is_none() {
        let worktree = repository
            .worktree_root()
            .to_str()
            .ok_or_else(|| {
                store_fault(
                    "trust::worktree-unrepresentable",
                    "resident worktree root is not UTF-8",
                )
            })?
            .to_owned();
        let previous = latest_surface_digest(bus)?;
        let mut pending = write(Actor::Spice, epoch, Producer::CommandDoor)?;
        let guarded_repository = repository.clone();
        let guarded_surface = surface.clone();
        pending.guard(JournalGuard::new(move |read| {
            require_current_surface(&guarded_repository, &guarded_surface)?;
            if read.trust_head(guarded_surface.digest())?.is_some() {
                return Err(GuardError::refused("trust::digest-already-pended"));
            }
            Ok(())
        }));
        let digest = surface.digest().clone();
        let pended = pending
            .fact(
                wire::body::TrustPended {
                    surface: crate::trust_command::SURFACE_NAME.to_owned(),
                    digest: digest.clone(),
                    worktree: Some(worktree.clone()),
                    previous,
                },
                cause,
            )
            .map_err(|error| store_fault("trust::fact-invalid", &error.to_string()))?;
        pending.trust_head(pended, move |_| TrustHead {
            digest,
            state: TrustState::Pended,
            worktree: Some(worktree),
        });
        let committed = commit(bus, pending)?;
        first.get_or_insert_with(|| committed.first());
        cause = FactCause::Existing(seq(committed.last()));
    }
    if let Some(committed) = super::principal_surface::bootstrap(bus, repository, epoch, cause)? {
        first.get_or_insert_with(|| committed.first());
        cause = FactCause::Existing(seq(committed.last()));
    }

    before_seating();
    let mut write = write(Actor::Spice, epoch, Producer::CommandDoor)?;
    let guarded_repository = repository.clone();
    let guarded_surface = surface.clone();
    write.guard(JournalGuard::new(move |_| {
        require_current_surface(&guarded_repository, &guarded_surface)
    }));
    let seated = write
        .fact(wire::body::UpSeated { epoch }, cause)
        .map_err(|error| store_fault("journal::write-invalid", &error.to_string()))?;
    let startup = lane_surface::append_startup_claims(bus, &lanes, epoch, &mut write, seated)?;
    write.residency_head(startup, move |_| ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision,
        lanes,
        subscriptions,
        renewals: Vec::new(),
    });
    let committed = commit(bus, write)?;
    Ok(StartupCommit {
        first: first.unwrap_or_else(|| committed.first()),
        last: committed.last(),
    })
}

/// Admit the rack an installed executable links through one `rack::admitted`
/// fact: the fact's sequence code is the generation short code rack URLs
/// carry. A staged app admits nothing.
fn admit_linked_rack(
    bus: &Bus,
    epoch: CanonicalUuid,
    app: &serve::canvas::CanvasAssets,
) -> CommandResult<()> {
    let (Some(archive), serve::canvas::CanvasAssets::EmbeddedArchive(linked)) =
        (spice::version::rack_archive(), app)
    else {
        return Ok(());
    };
    admit_rack(bus, epoch, archive, linked.generation()).map(|_| ())
}

/// The sequence of the fact admitting `archive`: the one already appended
/// for that archive, or a new `rack::admitted` fact naming `generation`.
pub(super) fn admit_rack(
    bus: &Bus,
    epoch: CanonicalUuid,
    archive: &wire::Digest,
    generation: &wire::Digest,
) -> CommandResult<wire::Seq> {
    let named = archive.clone();
    let admitted = bus
        .read(move |read| spice::rack_admission::admitted(read, &named))
        .map_err(|error| store_fault("rack::journal-unreadable", &error.to_string()))?;
    if let Some(admitted) = admitted {
        return Ok(admitted);
    }
    let mut write = write(Actor::Spice, epoch, Producer::ResidentRecognition)?;
    write
        .fact(
            wire::body::RackAdmitted {
                archive: archive.clone(),
                generation: generation.clone(),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("journal::write-invalid", &error.to_string()))?;
    commit(bus, write).map(|committed| seq(committed.first()))
}

pub(super) fn latest_surface_digest(bus: &Bus) -> CommandResult<Option<wire::Digest>> {
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("trust::journal-unreadable", &error.to_string()))?;
    Ok(snapshot
        .facts()
        .latest_trust_digest(crate::trust_command::SURFACE_NAME)
        .cloned())
}

mod deferral;
mod last_word;
pub(super) mod reap;
mod surface;
#[cfg(test)]
pub(super) use surface::announce_app;
use surface::run_surface;
