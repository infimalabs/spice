//! Occupant process launch and its configuration refusals.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use sill::{
    Cancellation, CancellationToken, Deadlines, Environment, Observation, RestartDisposition, Spec,
    WaitPolicy,
};
use spice::ambient::Ambient;
use spice::repository::Repository;

use super::*;

pub(in crate::resident) struct OccupantLaunch {
    pub(in crate::resident) spec: Spec,
    pub(in crate::resident) shell: OsString,
    pub(in crate::resident) cancellation: CancellationToken,
    pub(in crate::resident) lease: Duration,
    pub(in crate::resident) capture: lane::capture::Capture,
    pub(in crate::resident) session: SessionProof,
    pub(in crate::resident) git: String,
    /// Request bytes a command-driven vendor reads first, or nothing for a
    /// vendor driven entirely by its arguments.
    pub(in crate::resident) opening: Option<Vec<u8>>,
    /// The model the driver selects once the vendor lists what it serves, or
    /// nothing for a vendor whose arguments already fix it.
    pub(in crate::resident) model_selection: Option<vendor::omp::ModelSelection>,
    /// The first turn's message, which the driver writes once the vendor has
    /// answered its `spice` scheme registration and model selection, marked
    /// fresh or resumed so a resumed seat's notice is read only then.
    pub(in crate::resident) first_prompt: Option<crate::resident::supervisor::command::FirstTurn>,
}

impl OccupantLaunch {
    /// The supervised observation of this launch's record stream.
    pub(in crate::resident) fn observation(
        capture: lane::capture::Capture,
    ) -> CommandResult<Observation> {
        capture
            .observation(CAPTURE_LIMIT, RECORD_QUEUE)
            .map_err(|error| store_fault("lane::capture-invalid", &error.to_string()))
    }
}

/// What a launch observes beyond its own configuration: the Git working tree
/// it advances, shadows and briefs from, the RTK executable it probes, and
/// the host Cargo toolchain it isolates.
pub(in crate::resident) trait LaunchProbes {
    /// Advance the worktree to its baseline, naming what happened.
    fn advance(&self, repository: &Repository) -> String;

    /// Extend `base` with the installed Git hooks and then the Git shadow
    /// environment for `repository`.
    fn shadow(&self, repository: &Repository, base: Environment) -> CommandResult<Environment>;

    /// Observe RTK launchability under the occupant's exact environment.
    fn rtk(
        &self,
        executable: &std::ffi::OsStr,
        working: &Path,
        environment: &Environment,
    ) -> wire::body::RtkHealth;

    /// The isolated Cargo environment for `lane` in `repository`, whose
    /// `Cargo.lock` is already a regular file.
    fn cargo(
        &self,
        repository: &Repository,
        lane: CanonicalUuid,
    ) -> CommandResult<Vec<(OsString, OsString)>>;

    /// The session state a launch briefing reads, rooted at `worktree`.
    fn session(&self, worktree: PathBuf) -> crate::session_command::Environment;

    /// The resident's own value of the environment variable `name`.
    fn variable(&self, name: &str) -> Option<OsString>;

    /// Git selected by the same platform policy as installer and gate launches.
    #[cfg(windows)]
    fn git(&self, directory: &Path) -> Option<PathBuf> {
        let selected = gate::git::program(directory, self.variable("PATH").as_deref()).ok()?;
        selected.announce_secondary();
        Some(selected.path().to_path_buf())
    }
}

/// The probes a supervised launch runs against the host whose environment
/// `ambient` holds.
pub(in crate::resident) struct HostProbes<'a> {
    pub(in crate::resident) ambient: &'a Ambient,
}

impl LaunchProbes for HostProbes<'_> {
    fn advance(&self, repository: &Repository) -> String {
        spice::task_git::advance_at_launch(repository, self.ambient).to_string()
    }

    fn shadow(&self, repository: &Repository, base: Environment) -> CommandResult<Environment> {
        let hooked = spice::git_shadow::hooks(repository, base)
            .map_err(|error| store_fault("lane::git-hooks-unavailable", &error.to_string()))?;
        spice::git_shadow::environment(repository, hooked)
            .map_err(|error| store_fault("lane::git-shadow-unavailable", &error.to_string()))
    }

    fn rtk(
        &self,
        executable: &std::ffi::OsStr,
        working: &Path,
        environment: &Environment,
    ) -> wire::body::RtkHealth {
        spice::rtk::probe_health_in_environment(executable, working, environment)
    }

    fn cargo(
        &self,
        repository: &Repository,
        lane: CanonicalUuid,
    ) -> CommandResult<Vec<(OsString, OsString)>> {
        let unavailable = |error: spice::cargo_isolation::IsolationError| {
            store_fault("lane::cargo-isolation-unavailable", &error.to_string())
        };
        let host = crate::cargo_command::host(repository, self.ambient).map_err(unavailable)?;
        // The lane-specific home is the resident reader pin: one occupied seat
        // admits one process group, and a replacement occupant cannot prepare
        // the same lane home until that seat is vacant. Manual Cargo children
        // can overlap a home advance, so their preparer additionally holds a
        // kernel pin for the exact child lifetime.
        let home =
            spice::cargo_isolation::prepare_lane(repository, lane, &host).map_err(unavailable)?;
        Ok(isolated_cargo_environment(&host, home.home()))
    }

    fn session(&self, worktree: PathBuf) -> crate::session_command::Environment {
        crate::session_environment(self.ambient).rooted(worktree)
    }

    fn variable(&self, name: &str) -> Option<OsString> {
        self.ambient.get(name).map(std::ffi::OsStr::to_os_string)
    }
}

pub(in crate::resident) struct OccupantLaunchRequest<'a> {
    pub(in crate::resident) worktree: &'a str,
    pub(in crate::resident) vendor: &'a wire::Name,
    pub(in crate::resident) epoch: CanonicalUuid,
    pub(in crate::resident) lane: CanonicalUuid,
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) resume: vendor::Resume,
    pub(in crate::resident) fast_mode: bool,
    /// Model spelling this seat selected, or the vendor's configured model.
    pub(in crate::resident) model: Option<&'a str>,
    /// Effort spelling this seat selected, or the vendor's configured effort.
    pub(in crate::resident) effort: Option<&'a str>,
}

/// What one launch selects beyond the repository it runs in.
struct Selection<'a> {
    vendor: &'a wire::Name,
    lane: CanonicalUuid,
    /// Whether the request named no session to resume. omp pins a minted
    /// session for a fresh start too, so this is read before pinning.
    fresh: bool,
    pinned: Pinned,
    fast_mode: bool,
    model: Option<&'a str>,
    effort: Option<vendor::Effort>,
    /// The notice naming kept commands this lane still runs, if any.
    running: Option<String>,
}

pub(in crate::resident) fn occupant_launch(
    paths: &RuntimePaths,
    request: OccupantLaunchRequest<'_>,
    reporter: Arc<dyn CaptureReporter>,
    probes: &dyn LaunchProbes,
) -> CommandResult<OccupantLaunch> {
    let OccupantLaunchRequest {
        worktree,
        vendor: name,
        epoch,
        lane,
        occupant,
        resume,
        fast_mode,
        model,
        effort,
    } = request;
    let effort = effort
        .map(str::parse)
        .transpose()
        .map_err(|error: vendor::EffortError| {
            store_fault("lane::effort-invalid", &error.to_string())
        })?;
    let repository = admit_worktree(paths, worktree)?;
    // Run ahead of configuration load so the launch reads the advanced tree.
    let git = probes.advance(&repository);
    let resolved = spice::config::load_repository(&repository, &[])
        .map_err(|error| configuration_refusal(&error))?;
    let fresh = resume.session_id().is_none();
    let scripted = name.as_str() == vendor::OMP_SCRIPTED.name();
    let pinned = if scripted || name.as_str() == vendor::OMP.name() {
        pin_omp(
            &crate::resident::supervisor::supervisor_directory(paths, lane, occupant),
            &repository,
            resolved.root().vendor().omp(),
            OmpSeat::new(model, scripted),
            resume,
            probes,
        )?
    } else {
        Pinned::nothing(resume)
    };
    let selection = Selection {
        vendor: name,
        lane,
        fresh,
        pinned,
        fast_mode,
        model,
        effort,
        running: terminal::running_notice(&terminal::Jobs::for_lane(paths, lane)),
    };
    let (plan, rtk, shell) = launch_plan(&repository, &resolved, selection, probes)?;
    let model = plan.launch().model.clone();
    let effort = plan.launch().effort.as_str().to_owned();
    let fast_mode = plan.launch().fast_mode.unwrap_or(false);
    let record = plan
        .record()
        .map_err(|error| store_fault("lane::launch-invalid", &error.to_string()))?;
    let environment = Environment::new(
        plan.environment()
            .entries()
            .iter()
            .chain(record.environment())
            .cloned(),
    )
    .map_err(|error| store_fault("lane::environment-invalid", &error.to_string()))?;
    let (process, cancellation, lease) = launch_process(&repository, &resolved, environment, lane)?;
    let spec = record.spec(&process);
    let stream = record.stream();
    let expected = record.resume().session_id().map(str::to_owned);
    let opening = record.opening().map(<[u8]>::to_vec);
    let model_selection = record.model_selection().cloned();
    let first_prompt = first_turn(&record, fresh);
    let tape = tape::Tape::open(paths.lane_tape(lane).join(name.as_str()))
        .map_err(|error| store_fault("lane::capture-tape-unavailable", &error.to_string()))?;
    let (session_reply, session) = mpsc::sync_channel(4);
    let state = Arc::new(Mutex::new(DeclarationState {
        reply: session_reply,
        seen: None,
        witness: DeclarationWitness::default(),
    }));
    let declaration_reporter: Arc<dyn CaptureReporter> = Arc::new(DeclarationReporter {
        inner: reporter,
        lane,
        occupant,
        worktree: repository.worktree_root().display().to_string(),
        vendor: name.clone(),
        model,
        effort,
        fast_mode,
        git: git.clone(),
        rtk,
        epoch,
        stream,
        expected,
        state: Arc::clone(&state),
    });
    let capture = lane::capture::Capture::new(lane, occupant, record, tape, declaration_reporter);
    Ok(OccupantLaunch {
        spec,
        shell,
        cancellation,
        lease,
        capture,
        session: SessionProof {
            session,
            state,
            horizon: SESSION_PROOF,
        },
        git,
        opening,
        model_selection,
        first_prompt,
    })
}

/// The first turn `record` composes, marked fresh or resumed so a resumed
/// seat's notice is read only as it is delivered.
fn first_turn(
    record: &vendor::Record,
    fresh: bool,
) -> Option<crate::resident::supervisor::command::FirstTurn> {
    record.first_prompt().map(|prompt| {
        let prompt = prompt.to_owned();
        if fresh {
            crate::resident::supervisor::command::FirstTurn::Fresh(prompt)
        } else {
            crate::resident::supervisor::command::FirstTurn::Resumed(prompt)
        }
    })
}

/// Compose the vendor launch plan this lane's configuration admits.
fn launch_plan(
    repository: &Repository,
    resolved: &spice::config::Resolved,
    selection: Selection<'_>,
    probes: &dyn LaunchProbes,
) -> CommandResult<(spice::agent_launch::Plan, wire::body::RtkHealth, OsString)> {
    let Selection {
        vendor: name,
        lane,
        fresh,
        pinned,
        fast_mode,
        model,
        effort,
        running,
    } = selection;
    let route = resolved
        .root()
        .wrappers()
        .spice()
        .map(|reference| spice::agent_wrapper::resolve(reference, repository))
        .transpose()
        .map_err(|error| store_fault(error.reason(), &error.to_string()))?;
    let executable = match &route {
        Some(route) => route.executable().to_path_buf(),
        None => std::env::current_exe()
            .map_err(|error| store_fault("lane::executable-unresolved", &error.to_string()))?,
    };
    let (environment, rtk, shell) = launch_environment(
        repository,
        &executable,
        resolved.root().rtk().executable(),
        lane,
        probes,
    )?;
    let Some(vendor) = spice::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == name.as_str())
        .map(|entry| entry.vendor)
    else {
        return Err(vendor_unsupported(name.as_str().to_owned()));
    };
    let mut prompt = vendor.neutral_launch_prompt(spice::install::SKILL_RESOURCE);
    // A fresh session starts with nothing but this turn, so the resident
    // hands it the briefing itself. A resumed one already holds its context;
    // the driver appends the notice naming what it holds and what is pending
    // as it delivers the turn, never from this plan.
    if fresh {
        let session = probes.session(repository.worktree_root().to_path_buf());
        prompt.push_str("\n\n");
        prompt.push_str(&crate::session_command::lane_launch_briefing(
            &session, lane,
        ));
    }
    if let Some(running) = running {
        prompt.push_str("\n\n");
        prompt.push_str(&running);
    }
    let plan = spice::agent_launch::compose(
        resolved.root(),
        spice::agent_launch::Inputs {
            worktree: repository.worktree_root().to_path_buf(),
            vendor: name.clone(),
            prompt,
            bootstrap: spice::install::skill_bootstrap().to_owned(),
            environment,
            lane,
            resume: pinned.resume,
            config_overlay: pinned.config_overlay,
            steering_extension: pinned.steering_extension,
            fast_mode,
            model: model.map(str::to_owned),
            effort,
        },
    )
    .map_err(|error| match error {
        spice::agent_launch::ComposeError::UnsupportedVendor(other) => vendor_unsupported(other),
        other => store_fault("lane::launch-invalid", &other.to_string()),
    })?;
    Ok((plan, rtk, shell))
}

/// The refusal naming every vendor with a landed renderer beside the one asked
/// for.
fn vendor_unsupported(asked: String) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        "lane::vendor-unsupported".to_owned(),
        Some("lane.vendor".to_owned()),
        Some(
            spice::extension::VENDORS
                .iter()
                .map(|entry| entry.vendor.name())
                .collect::<Vec<_>>()
                .join(" or "),
        ),
        Some(asked),
    )))
}

/// Carry only the ambient variables a supervised occupant is admitted to see.
/// The operator's SSH agent socket is among them so lane Git over SSH signs
/// through the agent that already holds the key, not through whatever
/// unattended passphrase source `ssh` might otherwise reach.
///
/// The selected appliance's directory leads `PATH` so every bare `spice` in
/// the installed skill reaches that executable directly. Its exact path is
/// also carried for hook and MCP commands and cross-repository identity checks.
fn launch_environment(
    repository: &Repository,
    executable: &Path,
    rtk_executable: &str,
    lane: CanonicalUuid,
    probes: &dyn LaunchProbes,
) -> CommandResult<(Environment, wire::body::RtkHealth, OsString)> {
    let shell = native_shell(probes, repository.worktree_root())?;
    let state = repository
        .admit_state()
        .map_err(|error| crate::state_failure(&error).outcome())?;
    let rtk_history = state.rtk_history(lane);
    fs::create_dir_all(
        rtk_history
            .parent()
            .expect("the RTK history contract always has a parent"),
    )
    .map_err(|error| store_fault("lane::rtk-history-unavailable", &error.to_string()))?;
    let mut values = host_environment::ambient(probes)?;
    values.extend(cargo_environment(repository, lane, probes)?);
    values.extend([
        (
            OsString::from(steer::hook::EXECUTABLE),
            executable.as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::rtk::EXECUTABLE_VARIABLE),
            OsString::from(rtk_executable),
        ),
        (
            OsString::from(spice::rtk::HISTORY_VARIABLE),
            rtk_history.into_os_string(),
        ),
    ]);
    let path = probes.variable("PATH");
    if let Some(path) = compose_shell_path(executable.parent(), path.as_deref())? {
        values.push(("PATH".into(), path));
    }
    let environment = Environment::new(values)
        .map_err(|error| store_fault("lane::environment-invalid", &error.to_string()))?;
    let environment = probes.shadow(repository, environment)?;
    let probe_environment = rtk_probe_environment(&environment);
    let health = probes.rtk(
        std::ffi::OsStr::new(rtk_executable),
        repository.worktree_root(),
        &probe_environment,
    );
    Ok((environment, health, shell))
}

fn cargo_environment(
    repository: &Repository,
    lane: CanonicalUuid,
    probes: &dyn LaunchProbes,
) -> CommandResult<Vec<(OsString, OsString)>> {
    let lock = repository.worktree_root().join("Cargo.lock");
    match fs::symlink_metadata(&lock) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(store_fault(
                "lane::cargo-isolation-unavailable",
                &format!("{} is not a regular file", lock.display()),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(store_fault(
                "lane::cargo-isolation-unavailable",
                &format!("inspect {}: {error}", lock.display()),
            ));
        }
    }
    probes.cargo(repository, lane)
}

/// The variables binding an occupant to its private Cargo home and the host's
/// admitted native toolchain.
fn isolated_cargo_environment(
    host: &spice::cargo_isolation::Host,
    home: &Path,
) -> Vec<(OsString, OsString)> {
    vec![
        (
            OsString::from(spice::cargo_isolation::CARGO_HOME_VARIABLE),
            home.as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::cargo_isolation::BOOTSTRAP_HOME_VARIABLE),
            host.bootstrap_home().as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::cargo_isolation::NATIVE_CARGO_VARIABLE),
            host.cargo().as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::cargo_isolation::NATIVE_RUSTC_VARIABLE),
            host.rustc().as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::cargo_isolation::NATIVE_RUSTDOC_VARIABLE),
            host.rustdoc().as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::cargo_isolation::RUSTC_VARIABLE),
            host.rustc().as_os_str().to_owned(),
        ),
        (
            OsString::from(spice::cargo_isolation::RUSTDOC_VARIABLE),
            host.rustdoc().as_os_str().to_owned(),
        ),
    ]
}

/// Health probes are not agent commands and must never enter lane gain history.
fn rtk_probe_environment(environment: &Environment) -> Environment {
    Environment::new(
        environment
            .entries()
            .iter()
            .filter(|(name, _)| name != spice::rtk::HISTORY_VARIABLE)
            .cloned(),
    )
    .expect("filtering one valid environment entry preserves validity")
}

#[cfg(test)]
mod rtk_environment_tests {
    use super::*;

    fn fixture() -> Environment {
        Environment::new([
            ("PATH", "/fixture/bin"),
            (spice::rtk::EXECUTABLE_VARIABLE, "fixture-rtk"),
            (spice::rtk::HISTORY_VARIABLE, "/fixture/history.db"),
        ])
        .expect("fixture environment is valid")
    }

    #[test]
    fn configured_rtk_remains_available_after_an_inactive_launch_probe() {
        let health = spice::rtk::Health::new(
            spice::rtk::HealthState::Missing,
            None,
            "fixture executable unavailable during launch",
        );
        assert!(!health.active());
        let environment = fixture();
        assert!(
            environment
                .entries()
                .iter()
                .any(|(name, _)| { name == spice::rtk::EXECUTABLE_VARIABLE })
        );
        assert!(
            environment
                .entries()
                .iter()
                .any(|(name, _)| name == spice::rtk::HISTORY_VARIABLE)
        );
    }

    #[test]
    fn health_probe_cannot_write_the_lane_gain_database() {
        let probe = rtk_probe_environment(&fixture());
        assert!(
            probe
                .entries()
                .iter()
                .any(|(name, _)| name == spice::rtk::EXECUTABLE_VARIABLE)
        );
        assert!(
            probe
                .entries()
                .iter()
                .all(|(name, _)| name != spice::rtk::HISTORY_VARIABLE)
        );
    }
}

/// Ambient search path led by the selected appliance's directory, once.
fn compose_shell_path(
    executable_directory: Option<&Path>,
    ambient: Option<&std::ffi::OsStr>,
) -> CommandResult<Option<OsString>> {
    let mut entries = Vec::new();
    if let Some(directory) = executable_directory {
        entries.push(directory.to_path_buf());
    }
    if let Some(ambient) = ambient {
        entries.extend(
            std::env::split_paths(ambient)
                .filter(|entry| executable_directory.is_none_or(|directory| entry != directory)),
        );
    }
    if entries.is_empty() {
        return Ok(None);
    }
    std::env::join_paths(entries)
        .map(Some)
        .map_err(|error| store_fault("lane::environment-invalid", &error.to_string()))
}

#[cfg(test)]
mod shell_path_tests {
    use super::*;

    #[test]
    fn appliance_directory_leads_path_once_and_preserves_other_entries() {
        let executable = Path::new("/fixture/bin/spice");
        let ambient = std::env::join_paths([
            Path::new("/fixture/bin"),
            Path::new("/usr/bin"),
            Path::new("/fixture/bin"),
        ])
        .expect("fixture search path");

        let composed = compose_shell_path(executable.parent(), Some(ambient.as_os_str()))
            .expect("appliance search path composes")
            .expect("appliance supplies a search path");
        assert_eq!(
            std::env::split_paths(&composed).collect::<Vec<_>>(),
            [
                Path::new("/fixture/bin").to_path_buf(),
                Path::new("/usr/bin").to_path_buf(),
            ]
        );
    }

    #[test]
    fn appliance_directory_preserves_other_binary_directories() {
        let ambient = std::env::join_paths([Path::new("/fixture/bin"), Path::new("/usr/bin")])
            .expect("fixture search path");

        let composed = compose_shell_path(
            Some(Path::new("/fixture/appliance")),
            Some(ambient.as_os_str()),
        )
        .expect("ambient search path composes")
        .expect("ambient search path remains present");
        assert_eq!(
            std::env::split_paths(&composed).collect::<Vec<_>>(),
            [
                Path::new("/fixture/appliance").to_path_buf(),
                Path::new("/fixture/bin").to_path_buf(),
                Path::new("/usr/bin").to_path_buf(),
            ]
        );
    }
}

/// What the lane fixed on disk before launch for a vendor that reads it back.
struct Pinned {
    resume: vendor::Resume,
    config_overlay: Option<PathBuf>,
    steering_extension: Option<PathBuf>,
}

impl Pinned {
    /// A vendor that reads nothing back from disk: the resume as requested.
    const fn nothing(resume: vendor::Resume) -> Self {
        Self {
            resume,
            config_overlay: None,
            steering_extension: None,
        }
    }
}

/// The lane's choices an oh-my-pi seat pins before launch.
struct OmpSeat<'a> {
    model: Option<&'a str>,
    /// The seat plays the cue script its model slot names.
    scripted: bool,
}

impl<'a> OmpSeat<'a> {
    const fn new(model: Option<&'a str>, scripted: bool) -> Self {
        Self { model, scripted }
    }
}

/// Pin what an oh-my-pi occupant reads from disk before it launches: the
/// session it resumes, and beside its supervisor record in `directory` the
/// overlay carrying its compaction threshold and
/// language-server disposition, the extension that hands the provider
/// steering as the lane wrote it, and a scripted seat's provider extension.
/// The caller selects omp before entering.
fn pin_omp(
    directory: &Path,
    repository: &Repository,
    omp: &vendor::config::OmpConfig,
    seat: OmpSeat<'_>,
    resume: vendor::Resume,
    probes: &dyn LaunchProbes,
) -> CommandResult<Pinned> {
    let model = seat.model.unwrap_or_else(|| omp.model());
    if seat.scripted {
        pin_script(directory, repository, model)?;
    }
    let resume = match resume.session_id() {
        Some(_) => resume,
        None => pin_session(repository, probes)?,
    };
    let threshold = omp.auto_compact_threshold_tokens_for(model);
    let config_overlay = pin_seat_file(
        &directory.join("omp.yml"),
        &vendor::omp::config_overlay(threshold, omp.lsp()),
        "lane::config-overlay-unavailable",
        "configuration overlay",
    )?;
    let steering_extension = pin_seat_file(
        &directory.join("omp-steering.mjs"),
        vendor::omp::STEERING_EXTENSION,
        "lane::steering-extension-unavailable",
        "steering extension",
    )?;
    Ok(Pinned {
        resume,
        config_overlay: Some(config_overlay),
        steering_extension: Some(steering_extension),
    })
}

/// Parse the cue script a scripted seat's model slot names, `PATH[@RATE]`
/// with `PATH` relative to the worktree, and write the provider extension
/// that plays it beside the seat's steering extension. A missing script or
/// a malformed rate or cue refuses the launch, naming it.
fn pin_script(directory: &Path, repository: &Repository, slot: &str) -> CommandResult<()> {
    let refusal = |detail: String| {
        Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "lane::script-invalid".to_owned(),
            Some("lane.model".to_owned()),
            Some("a readable cue script spelled PATH[@RATE]".to_owned()),
            Some(detail),
        )))
    };
    let selection = vendor::omp::scripted::Selection::parse(slot)
        .map_err(|error| refusal(error.to_string()))?;
    let path = repository.worktree_root().join(selection.path);
    let text = fs::read_to_string(&path)
        .map_err(|error| refusal(format!("{}: {error}", path.display())))?;
    let plan = vendor::omp::scripted::Plan::parse(&text, selection.rate)
        .map_err(|error| refusal(format!("{}: {error}", path.display())))?;
    pin_seat_file(
        &directory.join(vendor::omp::scripted::EXTENSION_FILE),
        &vendor::omp::scripted::extension(&plan),
        "lane::script-extension-unavailable",
        "scripted provider extension",
    )?;
    Ok(())
}

/// Write one file this occupant's omp reads at launch beside its supervisor
/// record.
///
/// omp compacts at its own share of the model's window unless told a fixed
/// limit, and it takes that limit only from a settings file, so every launch
/// writes an overlay stating the threshold the tracked policy derives for the
/// model this seat runs. The same file states whether this seat runs omp's
/// language-server integration, whether Anthropic requests use the priority
/// tier, all of which omp otherwise decides from persistent settings.
/// Steering transport follows omp's native settings. The extension rides beside it,
/// so neither touches omp's own settings.
fn pin_seat_file(
    path: &Path,
    contents: &str,
    code: &'static str,
    what: &str,
) -> CommandResult<PathBuf> {
    fs::create_dir_all(path.parent().expect("a seat file has a directory"))
        .and_then(|()| fs::write(path, contents))
        .map_err(|error| {
            store_fault(
                code,
                &format!(
                    "oh-my-pi {what} {} cannot be written: {error}",
                    path.display()
                ),
            )
        })?;
    Ok(path.to_path_buf())
}

/// Pin a fresh oh-my-pi session before launch so its identity is known.
///
/// omp names a session only once it starts and exports nothing to its tools,
/// so the lane mints the identity, writes the session file omp resumes, and
/// launches by that identity.
fn pin_session(
    repository: &Repository,
    probes: &dyn LaunchProbes,
) -> CommandResult<vendor::Resume> {
    let home = session_home(probes)?;
    let temp = session_temp(probes)?;
    let layout = vendor::OMP.transcript_layout();
    let sessions = home
        .join(layout.default_home)
        .join(layout.sessions_directory);
    let identity = CanonicalUuid::new(uuid::Uuid::now_v7()).to_string();
    let (path, contents) = vendor::omp::session_file(
        &sessions,
        &canonical(&home),
        &canonical(&temp),
        &canonical(repository.worktree_root()),
        &identity,
        &session_stamp(jiff::Timestamp::now()),
    );
    fs::create_dir_all(path.parent().expect("a session file has a directory"))
        .and_then(|()| fs::write(&path, contents))
        .map_err(|error| {
            store_fault(
                "lane::session-file-unavailable",
                &format!(
                    "oh-my-pi session {} cannot be written: {error}",
                    path.display()
                ),
            )
        })?;
    vendor::SessionId::new(identity)
        .map(vendor::Resume::Session)
        .map_err(|error| store_fault("lane::session-invalid", &error.to_string()))
}

/// The path as omp sees it: resolved through symlinks when that is possible.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The instant in omp's millisecond ISO-8601 spelling.
fn session_stamp(now: jiff::Timestamp) -> String {
    now.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Bind occupant lifetime, wait policy, and capture bounds for this launch.
fn launch_process(
    repository: &Repository,
    resolved: &spice::config::Resolved,
    environment: Environment,
    lane: CanonicalUuid,
) -> CommandResult<(vendor::Process, CancellationToken, Duration)> {
    let deadlines = Deadlines::new(resolved.root().lane().runtime().get(), OUTPUT_DRAIN, REAP)
        .map_err(|error| store_fault("lane::deadlines-invalid", &error.to_string()))?;
    let cancellation = CancellationToken::new();
    let wait = WaitPolicy::new(
        format!("lane {lane}"),
        Cancellation::Token(cancellation.clone()),
        RestartDisposition::CallerDecides,
    )
    .map_err(|error| store_fault("lane::wait-invalid", &error.to_string()))?;
    let process = vendor::Process::new(
        repository.worktree_root(),
        environment,
        deadlines,
        wait,
        CAPTURE_LIMIT,
        TERMINATION_GRACE,
    )
    .map_err(|error| store_fault("lane::process-invalid", &error.to_string()))?;
    Ok((process, cancellation, resolved.root().lane().lease().get()))
}

pub(super) fn configuration_refusal(error: &spice::config::LoadError) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        "lane::configuration-refused".to_owned(),
        Some("effective-configuration".to_owned()),
        Some(
            "tracked, worktree, and local inputs admitted by the configuration registry".to_owned(),
        ),
        Some(error.diagnostic()),
    )))
}

mod host_environment;
use host_environment::{native_shell, session_home, session_temp};

#[cfg(all(test, windows))]
mod windows_environment_tests;

#[cfg(test)]
pub(in crate::resident) mod test_support;
#[cfg(test)]
mod tests;
