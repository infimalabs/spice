use super::*;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn session_environment(ambient: &Ambient) -> session_command::Environment {
    session_environment_from(
        |name| ambient.get(name).map(OsStr::to_os_string),
        std::env::current_dir().ok(),
    )
    .with_ambient(ambient.clone())
}

/// The session state a process whose variables `variable` reads would see
/// from `cwd`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn session_environment_from(
    variable: impl Fn(&str) -> Option<OsString>,
    cwd: Option<PathBuf>,
) -> session_command::Environment {
    let vendors = spice::extension::SESSION_VENDORS
        .iter()
        .copied()
        .map(|vendor| {
            session_command::VendorEnvironment::new(
                vendor,
                variable(vendor.thread_id_environment()),
            )
        })
        .collect();
    session_command::Environment::new(
        vendors,
        variable("SPICE_AGENT_VENDOR"), // env-policy: allow
        spice::ambient::session::home(&variable),
        cwd,
    )
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn working_directory() -> Result<PathBuf, Outcome<()>> {
    std::env::current_dir().map_err(|error| {
        environment_fault(
            "environment::working-directory-unavailable",
            error.to_string(),
        )
    })
}

/// Terminate the process after the command result has returned to the binary
/// rim.
pub fn terminate<B>(outcome: &Outcome<B>) -> ! {
    terminate_code(exit_status(outcome));
}

#[expect(
    clippy::disallowed_methods,
    reason = "app_command's stdin watcher thread must end the process and cannot return an outcome to main, so every exit routes through this one call"
)]
pub(crate) fn terminate_code(code: i32) -> ! {
    std::process::exit(code);
}

pub(crate) const fn exit_status<B>(outcome: &Outcome<B>) -> i32 {
    match outcome {
        Outcome::Reply(_) => REPLY_EXIT,
        Outcome::Refusal(_) => REFUSAL_EXIT,
        Outcome::Fault(_) => FAULT_EXIT,
    }
}

#[cfg(test)]
mod tests {
    use spice::cli;
    use spice::placeholder::PlaceholderReason;
    use wire::{Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition};

    use super::render::{self, Stream};
    use super::{
        FAULT_EXIT, REFUSAL_EXIT, REPLY_EXIT, exit_status, not_landed_from, target_unsupported,
    };

    fn reply() -> Outcome<()> {
        Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: (),
        })
    }

    fn refusal() -> Outcome<()> {
        Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "command::refused".to_owned(),
            None,
            None,
            None,
        )))
    }

    fn fault() -> Outcome<()> {
        Outcome::Fault(Fault {
            class: FaultClass::Environment,
            reason: "environment::unavailable".to_owned(),
            detail: "required service is unavailable".to_owned(),
        })
    }

    #[test]
    fn typed_outcomes_map_to_the_public_exit_contract() {
        assert_eq!(exit_status(&reply()), REPLY_EXIT);
        assert_eq!(exit_status(&refusal()), REFUSAL_EXIT);
        assert_eq!(exit_status(&fault()), FAULT_EXIT);
        assert_eq!((REPLY_EXIT, REFUSAL_EXIT, FAULT_EXIT), (0, 2, 1));
    }

    #[test]
    fn daemonless_local_os_contracts_have_named_wasm_refusals() {
        for (path, reason) in [
            (&["demo"][..], "demo::target-unsupported"),
            (&["install"][..], "install::target-unsupported"),
            (&["lock"][..], "lock::target-unsupported"),
            (&["lock", "run"][..], "lock::target-unsupported"),
            (&["restore", "verify"][..], "restore::target-unsupported"),
            (&["release", "notes"][..], "release::target-unsupported"),
        ] {
            let fault = target_unsupported(path)
                .unwrap_or_else(|| panic!("{} needs a wasm refusal", path.join(" ")));
            assert_eq!(fault.class, FaultClass::Environment);
            assert_eq!(fault.reason, reason);
            assert!(!fault.detail.is_empty());
        }
        assert!(target_unsupported(&["version"]).is_none());
    }

    #[test]
    fn landing_terminal_distinguishes_owned_and_unaccounted_paths() {
        let Outcome::Refusal(owned) = not_landed_from::<()>(
            &["fixture", "owned"],
            cli::Landing::GapOwned("FIXTURE-owner01"),
        ) else {
            panic!("an owned gap must remain one visible refusal");
        };
        assert_eq!(owned.reason(), PlaceholderReason::NotLanded.as_str());
        let diagnostic = owned.diagnostic().expect("owned gap is diagnostic");
        // The witness serializes beside the reason, so it is a string an
        // operator matches on rather than private naming.
        assert_eq!(diagnostic.witness(), Some("landing-owner"));
        assert_eq!(diagnostic.expected(), Some("FIXTURE-owner01"));
        assert_eq!(diagnostic.observed(), Some("fixture owned"));

        let Outcome::Fault(unaccounted) =
            not_landed_from::<()>(&["fixture", "unknown"], cli::Landing::Unaccounted)
        else {
            panic!("an unaccounted path must fault");
        };
        assert_eq!(
            unaccounted.reason,
            PlaceholderReason::LandingUnaccounted.as_str()
        );
    }

    /// The stream an outcome speaks on and the exit it carries read the same.
    ///
    /// These are two independent classifications of one enum, written in two
    /// modules, and nothing but this makes them agree. A caller redirecting by
    /// stream and a caller branching on the code are asking the same question,
    /// so an outcome that answered on standard output while exiting nonzero
    /// would strand whichever of them guessed wrong — and a variant added to
    /// one match without the other is exactly how that happens.
    #[test]
    fn the_stream_an_outcome_speaks_on_agrees_with_the_exit_it_carries() {
        for outcome in [reply(), refusal(), fault()] {
            assert_eq!(
                render::stream(&outcome) == Stream::Out,
                exit_status(&outcome) == REPLY_EXIT,
                "{outcome:?} must read the same either way",
            );
        }
    }
}
