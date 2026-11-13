//! Native resident and mounted-command process ownership.

use super::*;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn mounted_registry(
    form: Form,
    configuration_owner_selected: bool,
    lock_selected: bool,
) -> cli::MountRegistry {
    match load_mounted_registry(form) {
        Ok(mounts) => mounts,
        Err(error)
            if mount_error_is_ignorable(&error, configuration_owner_selected, lock_selected) =>
        {
            cli::MountRegistry::default()
        }
        Err(error) => deliver_mount_refusal(error, form),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn mount_error_is_ignorable(
    error: &cli::MountLoadError,
    configuration_owner_selected: bool,
    lock_selected: bool,
) -> bool {
    (configuration_owner_selected && !error.affects_mounts())
        || (lock_selected
            && matches!(
                error,
                cli::MountLoadError::Configuration(error) if lock_configuration_error(error)
            ))
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn lock_configuration_error(error: &spice::config::LoadError) -> bool {
    error.resolution().is_some_and(|resolution| {
        !resolution.issues().is_empty()
            && resolution.issues().iter().all(|issue| {
                issue.path().is_some_and(|path| {
                    matches!(
                        path.segments().next(),
                        Some(spice::config::PathSegment::Key("lock"))
                    )
                })
            })
    })
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn load_mounted_registry(form: Form) -> Result<cli::MountRegistry, cli::MountLoadError> {
    let working_directory = std::env::current_dir().unwrap_or_else(|error| {
        deliver(
            &environment_fault::<()>("cli::working-directory-unavailable", error.to_string()),
            form,
        );
    });
    cli::MountRegistry::load(&working_directory)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn deliver_mount_refusal(error: cli::MountLoadError, form: Form) -> ! {
    let observed = error.to_string();
    let outcome: Outcome<()> = Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "cli::mount-registry-refused".to_owned(),
            Some("repository-command-mounts".to_owned()),
            Some("one valid tracked command mount registry".to_owned()),
            Some(observed.clone()),
        )
        .with_detail(observed),
    ));
    deliver(&outcome, form)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn run_mounted(selection: cli::MountSelection<'_>) -> ! {
    let mount = selection.mount();
    let (program, declared_arguments) = mount
        .argv()
        .split_first()
        .expect("typed command mounts always contain a program");
    let status = std::process::Command::new(program)
        .args(declared_arguments)
        .args(selection.arguments())
        .current_dir(mount.repository())
        .status();

    match status {
        Ok(status) => terminate_child(status),
        Err(error) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "spice {}: {error}", mount.path().join(" "));
            let code = match error.kind() {
                io::ErrorKind::NotFound => 127,
                io::ErrorKind::PermissionDenied => 126,
                _ => FAULT_EXIT,
            };
            terminate_code(code);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn resident_down(
    leaf: &clap::ArgMatches,
    contract: &cli::Contract,
    form: Form,
    ambient: &spice::ambient::Ambient,
) -> ! {
    if leaf.get_flag("keeper") {
        deliver_display(&resident::keeper::uninstall(ambient), form);
    }
    deliver_display(&resident::down(contract, ambient), form);
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn resident_up(
    leaf: &clap::ArgMatches,
    form: Form,
    test_app_stage: bool,
    ambient: &spice::ambient::Ambient,
) -> ! {
    if leaf.get_flag("keeper") {
        deliver_display(&resident::keeper::install(leaf, ambient), form);
    }
    if !leaf.get_flag("foreground") {
        deliver_display(&resident::keeper::up_detached(leaf, ambient), form);
    }
    let child = match resident::ChildLaunch::capture(ambient) {
        Ok(child) => child,
        Err(outcome) => deliver(&outcome, form),
    };
    let status = child.as_ref().map(|child| child.status.clone());
    // A foreground resident has useful startup state before it has a terminal
    // outcome. Human mode keeps diagnostics on stderr and publishes the bound
    // endpoint on stdout, where a shell can read it while the resident remains
    // alive. JSON mode keeps both streams reserved for its one eventual machine
    // document. The handles stay unlocked: each line takes the stream lock for
    // its own write only, so an off-loop thread that prints while the resident
    // runs (launch establishment reporting a sealed tape member) never waits on
    // a guard the loop would hold for its whole lifetime.
    let outcome = match (form, child) {
        (Form::Text, child) => resident::up(
            leaf,
            child,
            test_app_stage,
            &mut io::stderr(),
            &mut io::stdout(),
            ambient,
        ),
        // A detached child is itself a JSON process so any terminal outcome in
        // the common log stays typed. Its progress still belongs in that log,
        // while the token-decorated endpoint must not.
        (Form::Json, child @ Some(_)) => resident::up(
            leaf,
            child,
            test_app_stage,
            &mut io::stderr(),
            &mut io::sink(),
            ambient,
        ),
        (Form::Json, None) => resident::up(
            leaf,
            None,
            test_app_stage,
            &mut io::sink(),
            &mut io::sink(),
            ambient,
        ),
    };
    if let Some(status) = status {
        status.failed(&outcome);
    }
    deliver(&outcome, form)
}
