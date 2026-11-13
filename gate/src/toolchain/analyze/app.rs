//! App web analysis for the selected tree and its owned build roots.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use sill::Environment;

use super::build_root::{AppSource, AppWorkspace};
use super::*;
use crate::execution_diagnostics::{self, ExecutionPhase};

/// Run the declared app checks in order and stop at the first non-clean one.
///
/// The analysis is run against a cell its workspace reports a failed removal
/// into, and that report outranks whatever the analysis concluded. A workspace
/// left on disk refuses every later app analysis of the same tree, so an arm
/// that passed while leaking one would report a clean tree and then be unable to
/// measure it again; the refusal here is the same refusal, delivered on the run
/// that caused it instead of the next one.
pub(in crate::toolchain) fn app_web_typecheck(
    declaration: ToolchainDeclaration,
    selection: CargoSelection<'_>,
    repository: &Path,
    bounds: &Bounds,
    resources: CompilerResources<'_>,
    deferred_trunk_refusal: Option<Analysis>,
) -> Analysis {
    let removal = Cell::new(None);
    let tools = AppTools {
        cargo: resources.program(CARGO),
        trunk: resources.program(TRUNK),
        deferred_trunk_refusal,
        compiler_environment: resources.environment(),
    };
    let analysis = app_analysis(
        declaration,
        selection,
        repository,
        tools,
        &removal,
        &mut |root, invocation, environment| {
            run_compile_analysis(
                declaration.id,
                root,
                invocation,
                bounds,
                environment,
                |code, message| FindingDetail::AppWebOutput { code, message },
            )
        },
    );
    match removal.take() {
        Some(report) => Analysis::terminal(ArmOutcome::ParseFailure, report),
        None => analysis,
    }
}

struct AppTools<'a> {
    cargo: Option<&'a str>,
    trunk: Option<&'a str>,
    deferred_trunk_refusal: Option<Analysis>,
    compiler_environment: &'a [(std::ffi::OsString, std::ffi::OsString)],
}

/// One declared app compiler run against the mirrored source root.
type AppRunner<'a> = &'a mut dyn FnMut(
    &Path,
    CompileInvocation<'_>,
    &Environment,
) -> Result<CompileSuccess, CompileFailure>;

fn app_analysis(
    declaration: ToolchainDeclaration,
    selection: CargoSelection<'_>,
    repository: &Path,
    tools: AppTools<'_>,
    removal: &Cell<Option<String>>,
    run: AppRunner<'_>,
) -> Analysis {
    let CargoSelection {
        root,
        base,
        changes,
    } = selection;
    if !root.join("Cargo.toml").is_file() || !root.join("app/index.html").is_file() {
        return Analysis::not_applicable(declaration);
    }
    let Some(cargo) = tools.cargo else {
        return Analysis::terminal(
            ArmOutcome::ToolUnavailable,
            format!("{} declares no {CARGO} program", declaration.id.as_str()),
        );
    };
    // The mirror must outlive both compiler invocations; only the generated
    // trunk distribution belongs to the transient per-run workspace below.
    let source = match AppSource::create(root, repository, removal) {
        Ok(source) => source,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    let metadata_environment =
        match tool_environment_with("app reach", &[cargo], tools.compiler_environment) {
            Ok(environment) => environment,
            Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
        };

    let source = source.path();
    let app = source.join("app");
    if !frontend_change_reaches_app(changes) {
        match crate::workspace_tests::workspace_package_reach_at(
            source,
            base,
            changes,
            &metadata_environment,
            std::ffi::OsStr::new(cargo),
        ) {
            Ok(reach) if reach.reaches_root(&app) => {}
            Ok(_) => return Analysis::not_applicable(declaration),
            Err(error) => return Analysis::terminal(ArmOutcome::ParseFailure, error.to_string()),
        }
    }
    if let Some(refusal) = tools.deferred_trunk_refusal {
        return refusal;
    }
    let Some(trunk) = tools.trunk else {
        return Analysis::terminal(
            ArmOutcome::ToolUnavailable,
            format!("{} declares no {TRUNK} program", declaration.id.as_str()),
        );
    };
    let workspace = match AppWorkspace::create(root, removal) {
        Ok(workspace) => workspace,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    let outputs = match AppOutputs::create(&workspace, root, repository) {
        Ok(outputs) => outputs,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    let environment = match app_environment(cargo, trunk, &outputs, tools.compiler_environment) {
        Ok(environment) => environment,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    let invocations = [
        (
            ExecutionPhase::Clippy,
            CompileInvocation {
                name: "cargo app-wasm-typecheck",
                program: cargo,
                arguments: CARGO_WASM_ARGUMENTS,
                cwd: source,
                capture: CompileCapture::Prefix,
            },
        ),
        (
            ExecutionPhase::Trunk,
            CompileInvocation {
                name: "trunk app build",
                program: trunk,
                arguments: TRUNK_BUILD_ARGUMENTS,
                cwd: app.as_path(),
                capture: CompileCapture::Prefix,
            },
        ),
    ];
    for (phase, invocation) in invocations {
        match run(source, invocation, &environment) {
            Ok(success) => {
                execution_diagnostics::record_phase(phase, success.elapsed());
                if phase == ExecutionPhase::Trunk {
                    // Trunk's own log is the only Cargo/bindgen breakdown.
                    execution_diagnostics::record_output_tail(success.output());
                }
            }
            Err(failure) => return *failure.analysis,
        }
    }
    Analysis::terminal(
        ArmOutcome::Passed,
        format!(
            "{} completed both declared app analyses",
            declaration.id.as_str()
        ),
    )
}

fn frontend_change_reaches_app(changes: &[String]) -> bool {
    changes.iter().any(|path| {
        path.starts_with("app/")
            || path.starts_with("serve/")
            || matches!(path.as_str(), "package.json" | "package-lock.json")
    })
}

fn app_environment(
    cargo: &str,
    trunk: &str,
    outputs: &AppOutputs,
    inherited: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<Environment, String> {
    let path = tool_search_path("app", &[cargo, trunk, "/usr/bin/cc", "/bin/sh"])?;
    let mut entries = inherited
        .iter()
        .cloned()
        .collect::<BTreeMap<std::ffi::OsString, std::ffi::OsString>>();
    entries.insert("PATH".into(), path);
    entries.insert(
        CARGO_TARGET_DIRECTORY.into(),
        outputs.cargo_target.as_os_str().to_owned(),
    );
    entries.insert(
        CARGO_BUILD_DIRECTORY.into(),
        outputs.cargo_build.as_os_str().to_owned(),
    );
    entries.insert(RUSTC_WRAPPER.into(), std::ffi::OsString::new());
    entries.insert(RUSTC_WORKSPACE_WRAPPER.into(), std::ffi::OsString::new());
    entries.insert(CARGO_INCREMENTAL.into(), "0".into());
    entries.insert(
        TRUNK_DISTRIBUTION_DIRECTORY.into(),
        outputs.trunk_dist.as_os_str().to_owned(),
    );
    entries.insert("NO_COLOR".into(), "true".into());
    Environment::new(entries).map_err(|error| format!("app tool environment was refused: {error}"))
}

/// Writable output roots for tools examining one immutable selected tree.
///
/// The Cargo pair is deliberately not inside [`AppWorkspace`]: it is a fixed
/// repository path that every app analysis of this repository shares,
/// so the compiled artifacts survive the run that produced them. `input.rs`
/// records what that warmth is worth -- five cold samples of the `trunk build`
/// this arm runs took 15.0 to 24.9 seconds against the 2.6 to 2.9 they take
/// warm -- and a root minted per invocation would pay the cold price every time
/// while stranding the previous one.
///
/// Sharing it is safe because Cargo already arbitrates it. Two of these
/// analyses run at once against one repository were measured taking the same
/// pair: the second reported `Blocking waiting for file lock on build
/// directory` and then completed, both exiting zero. What must not be shared is
/// the generated output, and that is the part living under the owned root.
struct AppOutputs {
    cargo_target: PathBuf,
    cargo_build: PathBuf,
    trunk_dist: PathBuf,
}

impl AppOutputs {
    fn create(
        workspace: &AppWorkspace<'_>,
        selected: &Path,
        repository: &Path,
    ) -> Result<Self, String> {
        let (cargo_target, cargo_build) = app_output_roots(repository);
        let trunk_dist = workspace.path().join("trunk-dist");
        if let Err(error) = fs::create_dir_all(&cargo_target)
            .and_then(|()| fs::create_dir_all(&cargo_build))
            .and_then(|()| fs::create_dir(&trunk_dist))
        {
            return Err(format!(
                "app analysis output directories for {} could not be created: {error}",
                selected.display()
            ));
        }
        Ok(Self {
            cargo_target,
            cargo_build,
            trunk_dist,
        })
    }
}

pub(in crate::toolchain) fn app_output_roots(repository: &Path) -> (PathBuf, PathBuf) {
    (
        repository.join("target/xtask-app"),
        repository.join("target/xtask-app-build"),
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    use std::time::Duration;

    use sill::Completion;

    use super::super::compile::{CompileReply, compile_reply};
    use super::*;
    use crate::toolchain::captured::Stream;

    /// The app checks run in declared order and stop at the first refusal, so
    /// trunk never builds an app whose compiler already rejected it.
    #[test]
    fn app_analysis_stops_at_the_first_refusing_compiler() {
        let scratch = crate::toolchain::exclusive_scratch_root("gate-app-precedence");
        let tree = scratch.join("tree");
        let repository = scratch.join("repository");
        fs::create_dir_all(tree.join("app")).expect("create fixture app");
        fs::create_dir(&repository).expect("create fixture repository");
        fs::write(tree.join("Cargo.toml"), "[workspace]\n").expect("write fixture manifest");
        fs::write(tree.join("app/index.html"), "<main></main>\n").expect("write fixture app");
        let changes = ["app/index.html".to_owned()];
        let declaration = crate::toolchain::TOOLCHAIN_ARMS
            .into_iter()
            .find(|declaration| declaration.id == GateId::AppWebTypecheck)
            .expect("the app arm is declared");
        let analyze = |refusing: Option<&str>| {
            let removal = Cell::new(None);
            let mut ran = Vec::new();
            let analysis = app_analysis(
                declaration,
                CargoSelection::new(&tree, None, &changes),
                &repository,
                AppTools {
                    cargo: Some("/fixture/bin/cargo"),
                    trunk: Some("/fixture/bin/trunk"),
                    deferred_trunk_refusal: None,
                    compiler_environment: &[],
                },
                &removal,
                &mut |root, invocation, _| {
                    ran.push(invocation.name);
                    let code = if refusing == Some(invocation.name) {
                        17
                    } else {
                        0
                    };
                    let whole = |bytes| {
                        Some(Stream {
                            bytes,
                            truncated: false,
                            complete: true,
                        })
                    };
                    compile_reply(
                        CompileReply {
                            id: declaration.id,
                            root,
                            cwd: invocation.cwd,
                            name: invocation.name,
                            capture: invocation.capture,
                            detail: |code, message| FindingDetail::AppWebOutput { code, message },
                        },
                        &Captured {
                            completion: &Completion::Exited(ExitStatus::from_raw(code << 8)),
                            observation_failure: None,
                            stdout: whole(b"compiling fixture\n"),
                            stderr: whole(b""),
                        },
                        Duration::ZERO,
                    )
                },
            );
            assert_eq!(removal.take(), None, "the app workspace was not removed");
            (analysis, ran)
        };

        let (refused, ran) = analyze(Some("cargo app-wasm-typecheck"));
        assert_eq!(ran, ["cargo app-wasm-typecheck"]);
        assert_eq!(refused.outcome, ArmOutcome::NonzeroAnalysis);
        assert_eq!(refused.exit_code, Some(17));

        let ((passed, ran), observed) = execution_diagnostics::observe_execution(|| analyze(None));
        assert_eq!(ran, ["cargo app-wasm-typecheck", "trunk app build"]);
        assert_eq!(passed.outcome, ArmOutcome::Passed);
        assert_eq!(
            observed
                .phases
                .iter()
                .map(|timing| timing.phase)
                .collect::<Vec<_>>(),
            [ExecutionPhase::Clippy, ExecutionPhase::Trunk]
        );
        assert_eq!(observed.output_tail.as_deref(), Some("compiling fixture\n"));

        fs::remove_dir_all(&scratch).expect("remove app fixture");
    }

    #[test]
    fn app_normalizes_trunks_boolean_no_color_input() {
        let outputs = AppOutputs {
            cargo_target: PathBuf::from("/fixture/cargo-target"),
            cargo_build: PathBuf::from("/fixture/cargo-build"),
            trunk_dist: PathBuf::from("/fixture/trunk-dist"),
        };
        let inherited = [
            ("NO_COLOR".into(), "1".into()),
            ("PRESERVED_COMPILER_INPUT".into(), "value".into()),
        ];

        let environment = app_environment("/bin/echo", "/bin/echo", &outputs, &inherited)
            .expect("valid app environment");
        let entries = environment
            .entries()
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            entries.get(std::ffi::OsStr::new("NO_COLOR")),
            Some(&"true".into())
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new("PRESERVED_COMPILER_INPUT")),
            Some(&"value".into()),
        );
    }
}
