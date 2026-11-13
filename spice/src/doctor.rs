//! Bounded acquisition for the daemonless Doctor report.

mod subsystems;

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use spice::ambient::Ambient;
use spice::config::{self, Resolved, Root};
use spice::repository::Repository;
use wire::{
    DoctorCommand, DoctorConfigProvenance, DoctorEnvironment, DoctorExtension,
    DoctorExtensionNamespace, DoctorLocations, DoctorObservation, DoctorReport, DoctorTarget,
    DoctorTool, DoctorToolRequirement, DoctorToolSource, Outcome, Refusal, Reply, ReplyPosition,
    SpiceVariable,
};

use crate::cli::{self, ExternalTool};

/// Inspect the repository containing `working` without opening authority or
/// launching any required command tool. The optional configured RTK executable
/// crosses its bounded health contract because availability alone is unsafe.
pub(crate) fn inspect(
    working: &Path,
    environment: &[(SpiceVariable, bool)],
    ambient: &Ambient,
    configuration: Option<&config::RepositoryCapture>,
) -> Outcome<DoctorReport<Root>> {
    let repository = match Repository::discover(working) {
        Ok(repository) => repository,
        Err(error) => {
            return crate::environment_fault(
                spice::repository::ResolveError::REASON,
                error.to_string(),
            );
        }
    };
    let loaded = match config::capture_repository_scoped(&repository, &[], configuration) {
        Ok(loaded) => loaded,
        Err(error) => return Outcome::Refusal(configuration_refusal(&error)),
    };
    let resolved = loaded.resolved().clone();
    let probe = FilesystemProbe { working, ambient };

    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body: report(working, &repository, resolved, environment, &probe),
    })
}

fn report(
    working: &Path,
    repository: &Repository,
    resolved: Resolved,
    environment: &[(SpiceVariable, bool)],
    probe: &impl ExecutableProbe,
) -> DoctorReport<Root> {
    let locations = config::locations(repository);
    let provenance = resolved
        .provenance()
        .iter()
        .map(|row| DoctorConfigProvenance {
            path: row.path().to_string(),
            source: row.source(),
        })
        .collect();
    let configuration = resolved.into_root();
    let commands = cli::contracts()
        .iter()
        .map(|contract| command_report(contract, &configuration, probe))
        .collect();

    DoctorReport {
        target: DoctorTarget {
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        },
        locations: DoctorLocations {
            working: working.display().to_string(),
            repository: repository.worktree_root().display().to_string(),
            repository_common: repository.common_dir().display().to_string(),
            tracked_configuration: locations.tracked().display().to_string(),
            worktree_configuration: locations.worktree().display().to_string(),
            local_configuration: locations.local().display().to_string(),
        },
        environment: SpiceVariable::ALL
            .iter()
            .map(|variable| DoctorEnvironment {
                variable: *variable,
                observation: if environment
                    .iter()
                    .any(|(candidate, present)| candidate == variable && *present)
                {
                    DoctorObservation::Ready {}
                } else {
                    DoctorObservation::Unavailable {
                        reason: format!("{} is absent", variable.as_str()),
                    }
                },
            })
            .collect(),
        subsystems: subsystems::diagnose(repository),
        configuration,
        provenance,
        commands,
        extensions: extension_report(),
    }
}

fn extension_report() -> Vec<DoctorExtension> {
    let mut report =
        Vec::with_capacity(spice::extension::VENDORS.len() + 1 + spice::extension::GATES.len());
    report.extend(
        spice::extension::VENDORS
            .iter()
            .map(|entry| DoctorExtension {
                namespace: DoctorExtensionNamespace::Vendor,
                name: entry.vendor.name().to_owned(),
                provider: entry.provider.to_owned(),
                observation: DoctorObservation::Ready {},
            }),
    );
    report.push(DoctorExtension {
        namespace: DoctorExtensionNamespace::Wrapper,
        name: spice::extension::SHELL_WRAPPER.name.to_owned(),
        provider: spice::extension::SHELL_WRAPPER.provider.to_owned(),
        observation: DoctorObservation::Ready {},
    });
    report.extend(
        spice::extension::GATES
            .iter()
            .map(|component| DoctorExtension {
                namespace: DoctorExtensionNamespace::Gate,
                name: component.name.to_owned(),
                provider: component.provider.to_owned(),
                observation: DoctorObservation::Ready {},
            }),
    );
    report
}

fn command_report(
    contract: &cli::Contract,
    configuration: &Root,
    probe: &impl ExecutableProbe,
) -> DoctorCommand {
    let tools: Vec<DoctorTool> = contract
        .tools
        .iter()
        .map(|tool| DoctorTool {
            name: tool.name.to_owned(),
            owner: tool.owner.to_owned(),
            mode: tool.mode,
            requirement: tool.requirement,
            source: tool.source,
            bounds: tool.bounds,
            cancellation: tool.cancellation,
            network: tool.network,
            observation: tool_observation(tool, configuration, probe),
        })
        .collect();
    // An optional tool's absence disables only its own integration, so only
    // required and conditional tools decide whether the command can run.
    let observation = tools
        .iter()
        .filter(|tool| tool.requirement != DoctorToolRequirement::Optional)
        .find(|tool| !matches!(tool.observation, DoctorObservation::Ready {}))
        .map_or_else(
            || {
                if tools.is_empty() {
                    DoctorObservation::NotApplicable {
                        reason: "command declares no external tools".to_owned(),
                    }
                } else {
                    DoctorObservation::Ready {}
                }
            },
            |tool| tool.observation.clone(),
        );

    DoctorCommand {
        command: contract.path.join(" "),
        observation,
        tools,
    }
}

fn tool_observation(
    tool: &ExternalTool,
    configuration: &Root,
    probe: &impl ExecutableProbe,
) -> DoctorObservation {
    match tool.source {
        DoctorToolSource::CommandOperand => DoctorObservation::InputRequired {
            reason: "the caller supplies this executable at invocation".to_owned(),
        },
        DoctorToolSource::BootstrapEnvironment => match probe.named(tool.name) {
            Ok(()) => DoctorObservation::Ready {},
            Err(reason) => DoctorObservation::Unavailable { reason },
        },
        DoctorToolSource::GatePolicyContract => {
            gate_observation("policy", gate::declared_tools(), probe)
        }
        DoctorToolSource::ReferenceTransactionContract => match probe.named(tool.name) {
            Ok(()) => DoctorObservation::Ready {},
            Err(reason) => DoctorObservation::Unavailable { reason },
        },
        DoctorToolSource::ProofRuntime => configuration.proof().container().map_or_else(
            || DoctorObservation::InputRequired {
                reason: "local proof.container must name the sealed Proof runtime".to_owned(),
            },
            |runtime| match probe.exact(runtime.as_path()) {
                Ok(()) => DoctorObservation::Ready {},
                Err(reason) => DoctorObservation::Unavailable {
                    reason: format!("configured Proof runtime is unavailable: {reason}"),
                },
            },
        ),
        DoctorToolSource::ProofBattery => DoctorObservation::Unsupported {
            reason: concat!(
                "container-internal battery programs cannot be inspected without ",
                "starting the sealed Proof environment"
            )
            .to_owned(),
        },
        DoctorToolSource::TaskBoundaryContract => match probe.named(tool.name) {
            Ok(()) => DoctorObservation::Ready {},
            Err(reason) => DoctorObservation::Unavailable { reason },
        },
        DoctorToolSource::AdjudicationContract | DoctorToolSource::SymbolLookupContract => {
            match probe.named(tool.name) {
                Ok(()) => DoctorObservation::Ready {},
                Err(reason) => DoctorObservation::Unavailable { reason },
            }
        }
        DoctorToolSource::WorktreeConfiguration => {
            let executable = configuration.rtk().executable();
            match probe.rtk(executable) {
                Ok(()) => DoctorObservation::Ready {},
                Err(reason) => DoctorObservation::Unavailable {
                    reason: format!("configured RTK executable {executable:?} is native: {reason}"),
                },
            }
        }
        DoctorToolSource::VendorConfiguration => {
            let program = configuration.vendor().omp().binary().as_str();
            match probe.configured(program) {
                Ok(()) => DoctorObservation::Ready {},
                Err(reason) => DoctorObservation::Unavailable {
                    reason: format!(
                        "configured vendor {} program {program:?} is unavailable: {reason}",
                        tool.name
                    ),
                },
            }
        }
    }
}

fn gate_observation(
    profile: &str,
    declarations: Result<Vec<gate::ExecutableDeclaration>, gate::GateSetError>,
    probe: &impl ExecutableProbe,
) -> DoctorObservation {
    let declarations = match declarations {
        Ok(declarations) => declarations,
        Err(error) => {
            return DoctorObservation::Unsupported {
                reason: format!("the compiled Gate {profile} tool registry is incoherent: {error}"),
            };
        }
    };
    if declarations.is_empty() {
        return DoctorObservation::Unsupported {
            reason: format!("the compiled Gate {profile} tool registry declares no executable"),
        };
    }
    for declaration in declarations {
        if let Err(reason) = probe.configured(declaration.program()) {
            return DoctorObservation::Unavailable {
                reason: format!(
                    "required Gate tool {} is unavailable: {reason}",
                    declaration.name()
                ),
            };
        }
    }
    DoctorObservation::Ready {}
}

fn configuration_refusal(error: &config::LoadError) -> Refusal {
    Refusal::bare(wire::RefusalDiagnostic::new(
        "doctor::configuration-refused".to_owned(),
        None,
        Some("tracked and local inputs admitted by the configuration registry".to_owned()),
        Some(error.diagnostic()),
    ))
}

trait ExecutableProbe {
    fn named(&self, name: &str) -> Result<(), String>;
    fn exact(&self, path: &Path) -> Result<(), String>;

    /// Locate a configured program without running it: an absolute path is
    /// checked exactly, and a bare name is searched for on the search path.
    fn configured(&self, executable: &str) -> Result<(), String> {
        if Path::new(executable).is_absolute() {
            self.exact(Path::new(executable))
        } else {
            self.named(executable)
        }
    }

    fn rtk(&self, executable: &str) -> Result<(), String> {
        self.configured(executable)
    }
}

struct FilesystemProbe<'a> {
    working: &'a Path,
    ambient: &'a Ambient,
}

impl ExecutableProbe for FilesystemProbe<'_> {
    fn named(&self, name: &str) -> Result<(), String> {
        let Some(search_path) = self.ambient.get("PATH") else {
            return Err(format!(
                "{name} is not available because the executable search path is absent"
            ));
        };
        for directory in std::env::split_paths(search_path) {
            let directory = if directory.is_absolute() {
                directory
            } else {
                self.working.join(directory)
            };
            for candidate in executable_names(name) {
                if executable(&directory.join(candidate)).is_ok() {
                    return Ok(());
                }
            }
        }
        Err(format!(
            "{name} is not available on the configured executable search path"
        ))
    }

    fn exact(&self, path: &Path) -> Result<(), String> {
        executable(path)
    }

    fn rtk(&self, executable: &str) -> Result<(), String> {
        let health = spice::rtk::probe_health(OsStr::new(executable), self.working, self.ambient);
        if health.active() {
            Ok(())
        } else {
            Err(format!(
                "state={} version={} detail={}",
                health.state().as_str(),
                health.version().unwrap_or("unknown"),
                health.detail()
            ))
        }
    }
}

#[cfg(windows)]
fn executable_names(name: &str) -> Vec<String> {
    ["", ".exe", ".cmd", ".bat"]
        .into_iter()
        .map(|suffix| format!("{name}{suffix}"))
        .collect()
}

#[cfg(not(windows))]
fn executable_names(name: &str) -> Vec<String> {
    vec![name.to_owned()]
}

fn executable(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not executable", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct Probe {
        named: BTreeMap<String, Result<(), String>>,
        exact: Result<(), String>,
    }

    impl ExecutableProbe for Probe {
        fn named(&self, name: &str) -> Result<(), String> {
            self.named
                .get(name)
                .cloned()
                .unwrap_or_else(|| Err(format!("no fixture for {name}")))
        }

        fn exact(&self, _path: &Path) -> Result<(), String> {
            self.exact.clone()
        }
    }

    fn probe(exact: Result<(), String>) -> Probe {
        Probe {
            named: BTreeMap::from([("rtk".to_owned(), Ok(()))]),
            exact,
        }
    }

    fn default_command_report(contract: &cli::Contract, probe: &Probe) -> DoctorCommand {
        command_report(contract, &Root::default(), probe)
    }

    #[test]
    fn every_generated_contract_has_one_deterministic_report_row() {
        let first: Vec<_> = cli::contracts()
            .iter()
            .map(|contract| default_command_report(contract, &probe(Ok(()))))
            .collect();
        let second: Vec<_> = cli::contracts()
            .iter()
            .map(|contract| default_command_report(contract, &probe(Ok(()))))
            .collect();

        assert_eq!(first, second);
        assert_eq!(first.len(), cli::contracts().len());
        assert!(first.iter().all(|row| !row.command.is_empty()));
        assert!(first.iter().any(|row| {
            row.tools.is_empty()
                && matches!(row.observation, DoctorObservation::NotApplicable { .. })
        }));
    }

    #[test]
    fn missing_required_executables_are_not_ready() {
        let missing = probe(Err("fixture executable is absent".to_owned()));
        for command in cli::contracts().iter().filter(|contract| {
            contract
                .tools
                .iter()
                .any(|tool| tool.source == DoctorToolSource::GatePolicyContract)
        }) {
            let row = default_command_report(command, &missing);
            assert!(matches!(
                row.observation,
                DoctorObservation::Unavailable { ref reason }
                    if reason.contains("fixture executable is absent")
            ));
            assert!(row.tools.iter().all(|tool| {
                tool.requirement == DoctorToolRequirement::Required
                    && matches!(tool.observation, DoctorObservation::Unavailable { .. })
            }));
        }
    }

    #[test]
    fn an_absent_optional_tool_leaves_its_command_ready() {
        let judge = cli::JUDGE_EXECUTABLE;
        let command = |path: &[&str]| {
            cli::contracts()
                .iter()
                .find(|contract| contract.path == path)
                .expect("command contract exists")
        };
        let mut without_judge = probe(Ok(()));
        without_judge.named.insert("git".to_owned(), Ok(()));
        without_judge
            .named
            .insert(judge.to_owned(), Err("judge is absent".to_owned()));

        for path in [&["up"][..], &["task", "done"]] {
            let row = default_command_report(command(path), &without_judge);
            assert_eq!(row.observation, DoctorObservation::Ready {}, "{path:?}");
            assert!(row.tools.iter().any(|tool| {
                tool.name == judge
                    && tool.requirement == DoctorToolRequirement::Optional
                    && tool.observation
                        == DoctorObservation::Unavailable {
                            reason: "judge is absent".to_owned(),
                        }
            }));
        }

        let mut without_git = without_judge;
        without_git
            .named
            .insert("git".to_owned(), Err("git is absent".to_owned()));
        assert_eq!(
            default_command_report(command(&["task", "done"]), &without_git).observation,
            DoctorObservation::Unavailable {
                reason: "git is absent".to_owned(),
            }
        );
    }

    #[test]
    fn an_absent_conditional_tool_keeps_its_command_unavailable() {
        let contract = cli::contracts()
            .iter()
            .find(|contract| contract.path == ["gate", "reference-transaction"])
            .expect("conditional command contract exists");
        let mut without_git = probe(Ok(()));
        without_git
            .named
            .insert("git".to_owned(), Err("git is absent".to_owned()));

        let row = default_command_report(contract, &without_git);
        assert_eq!(
            row.observation,
            DoctorObservation::Unavailable {
                reason: "git is absent".to_owned(),
            }
        );
        assert!(row.tools.iter().any(|tool| {
            tool.name == "git"
                && tool.requirement == DoctorToolRequirement::Conditional
                && tool.observation
                    == DoctorObservation::Unavailable {
                        reason: "git is absent".to_owned(),
                    }
        }));
    }

    #[test]
    fn an_absent_vendor_program_leaves_occupant_launches_unavailable() {
        let mut with_vendor = probe(Ok(()));
        with_vendor.named.insert("omp".to_owned(), Ok(()));
        let mut without_vendor = probe(Ok(()));
        without_vendor
            .named
            .insert("omp".to_owned(), Err("omp is absent".to_owned()));

        for path in [&["lane", "wake"][..], &["lane", "import"]] {
            let contract = cli::contracts()
                .iter()
                .find(|contract| contract.path == path)
                .expect("occupant launch contract exists");
            assert_eq!(
                default_command_report(contract, &with_vendor).observation,
                DoctorObservation::Ready {},
                "{path:?}"
            );
            assert!(
                matches!(
                    default_command_report(contract, &without_vendor).observation,
                    DoctorObservation::Unavailable { ref reason }
                        if reason.contains("\"omp\"") && reason.contains("omp is absent")
                ),
                "{path:?}"
            );
        }
    }

    #[test]
    fn unsupported_checks_cannot_be_aggregated_as_ready() {
        for command in cli::contracts().iter().filter(|contract| {
            contract
                .tools
                .iter()
                .any(|tool| tool.source == DoctorToolSource::ProofBattery)
        }) {
            let row = default_command_report(command, &probe(Ok(())));
            assert!(!matches!(row.observation, DoctorObservation::Ready {}));
            assert!(
                row.tools.iter().any(|tool| {
                    matches!(tool.observation, DoctorObservation::Unsupported { .. })
                })
            );
        }
    }
}
