//! The Windows MSVC release set, cross-built from Linux in the Windows cross
//! image.

use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use super::*;

/// The release set's one target.
const TARGET: ReleaseTarget = ReleaseTarget::X86_64PcWindowsMsvc;

/// The artifact root below the output directory; the build key and command
/// records sit beside it, outside the inventory.
const ROOT: &str = "root";
const BUILD_KEY: &str = "build-key.json";

/// The repository-owned executables the Windows workflow runs beside the
/// candidate, by package and binary.
const RUNNERS: [(&str, &str); 2] = [("windows-execution", "windows-hosted"), ("xtask", "xtask")];

/// The identities only the host that built the cross image can name.
#[derive(Clone, Copy, Debug)]
pub struct CrossImage<'a> {
    /// The exact OCI ID of the image this build runs in.
    pub image: &'a str,
    /// The admitted MSVC CRT and SDK generation the image carries.
    pub msvc_generation: &'a str,
}

/// Cross-build the candidate, the hosted runner, the release publisher, and
/// every proof member's test executables for `x86_64-pc-windows-msvc`, and lay
/// them out under `out/root` with their build key at `out/build-key.json`.
/// The toolchain the build observed lies beside the candidate, where the
/// prebuilt stage checks it against the release pins.
///
/// The source is the release archive, as [`stage_target`] builds it: a
/// delivered archive's tree is the workspace, otherwise the stage root's
/// archive is extracted under the build root. The candidate is the exact
/// linked, canvas-carrying executable and PDB `stage-target` builds; nothing
/// here runs a Windows executable.
///
/// # Errors
///
/// Refuses what [`stage_target`] refuses before its first build, a failed
/// build, an executable Cargo did not report, or an output write failure.
pub fn cross_build_windows(
    environment: ReleaseEnvironment,
    image: CrossImage<'_>,
    app: AppOrigin,
    out: &Path,
    output: &mut impl Write,
) -> Result<(), TargetBuildError> {
    let workspace = workspace_root()?;
    let build_root = environment.build_root(&workspace);
    let stage_root = environment.stage_root_path(&workspace);
    let (archive, root) = match &environment.source_archive {
        Some(delivered) => (SourceArchive::delivered(delivered)?, workspace.clone()),
        None => local_source_tree(&workspace, &environment)?,
    };
    let version = spice::version::source_version(&root, dist::Build::Staged)
        .map_err(TargetBuildError::SourceVersion)?;
    let canvas = release_canvas(app, &root, &workspace, &stage_root, &environment, &archive)?;
    let linked_canvas = canvas.as_ref().map(|canvas| sha256(canvas.bytes()));
    let programs = Programs::discover(&root, &environment, TARGET)?;
    let executor = SystemExecutor::new(&root, &environment, &programs.rustc)?;
    let result = (|| {
        let toolchain = observe_toolchain(&executor, &programs, TARGET)?;
        let outputs = build_set(&root, &build_root, &version, canvas, &programs, &executor)?;
        archive.verify()?;
        let mut inputs = build_key(&root, &archive, image, &programs, &executor)?;
        if let Some(linked_canvas) = linked_canvas {
            inputs.insert("canvas".to_owned(), linked_canvas);
        }
        publish(out, &outputs, &inputs, &toolchain)
    })();
    render_command_records(output, &executor.records())?;
    result?;
    writeln!(output, "[windows-cross] root={}", out.join(ROOT).display())
        .map_err(TargetBuildError::Output)
}

/// One built file and where the artifact root names it.
struct Placed {
    source: PathBuf,
    path: String,
}

fn build_set<E: Executor>(
    root: &Path,
    build_root: &Path,
    version: &ProductVersion,
    canvas: Option<crate::app::StagedCanvas>,
    programs: &Programs,
    executor: &E,
) -> Result<Vec<Placed>, TargetBuildError> {
    let target_dir = candidate().target_dir(build_root);
    let app_root = release_app_root(build_root);
    let mut builder = CommandBuilder {
        root,
        app_root: &app_root,
        target: TARGET,
        target_dir: &target_dir,
        programs,
        version,
        executor,
        #[cfg(unix)]
        commit: "",
        #[cfg(unix)]
        signing: None,
        capture_symbols: false,
        symbols: None,
        symbol_artifact: None,
        linux_debug_artifact: None,
        windows_debug_artifact: None,
        canvas,
        executable: None,
    };
    let linked = builder.build_app().map_err(TargetBuildError::Build)?;
    builder
        .build_native(TARGET, &linked)
        .map_err(TargetBuildError::Build)?;
    let executable = builder
        .executable
        .take()
        .expect("a built candidate names its executable");
    let mut placed = vec![Placed {
        path: prebuilt::CANDIDATE.to_owned(),
        source: executable,
    }];

    let mut arguments = vec![
        OsString::from("build"),
        OsString::from("--locked"),
        OsString::from("--release"),
    ];
    for (package, binary) in RUNNERS {
        arguments.extend([
            OsString::from("-p"),
            OsString::from(package),
            OsString::from("--bin"),
            OsString::from(binary),
        ]);
    }
    let runners = cargo_executables(programs, executor, "cross-runners", arguments, &target_dir)?;
    for (_, binary) in RUNNERS {
        let built = runners
            .iter()
            .find(|built| !built.test && built.name == binary)
            .ok_or_else(|| missing(binary))?;
        placed.push(Placed {
            path: format!("runner/{binary}.exe"),
            source: built.executable.clone(),
        });
    }

    for check in proof::Battery::current().checks() {
        let mut arguments = check
            .arguments()
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        arguments.push(OsString::from("--no-run"));
        let member = check.kind().as_str();
        let tests = cargo_executables(programs, executor, "cross-proof", arguments, &target_dir)?;
        let tests = tests
            .into_iter()
            .filter(|built| built.test)
            .collect::<Vec<_>>();
        if tests.is_empty() {
            return Err(missing(member));
        }
        for built in tests {
            placed.push(Placed {
                path: format!("proof/{member}/{}-{}.exe", built.kind, built.name),
                source: built.executable,
            });
        }
    }
    Ok(placed)
}

/// One executable Cargo reported linking.
struct Built {
    name: String,
    kind: String,
    test: bool,
    executable: PathBuf,
}

/// Run one Cargo build for the Windows target and collect every executable
/// it reports, in report order.
fn cargo_executables<E: Executor>(
    programs: &Programs,
    executor: &E,
    label: &'static str,
    mut arguments: Vec<OsString>,
    target_dir: &Path,
) -> Result<Vec<Built>, TargetBuildError> {
    arguments.extend([
        OsString::from("--target"),
        OsString::from(TARGET.as_str()),
        OsString::from("--target-dir"),
        target_dir.as_os_str().to_owned(),
        OsString::from("--message-format=json-render-diagnostics"),
    ]);
    let record =
        successful(executor.execute(CommandSpec::new(label, &programs.cargo, arguments))?)?;
    reported_executables(&record.stdout)
}

fn reported_executables(stdout: &[u8]) -> Result<Vec<Built>, TargetBuildError> {
    let mut built = Vec::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let message: serde_json::Value =
            serde_json::from_slice(line).map_err(|error| TargetBuildError::Observation {
                label: "cargo message",
                observed: error.to_string(),
            })?;
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(executable) = message["executable"].as_str() else {
            continue;
        };
        let (Some(name), Some(kind), Some(test)) = (
            message["target"]["name"].as_str(),
            message["target"]["kind"][0].as_str(),
            message["profile"]["test"].as_bool(),
        ) else {
            return Err(TargetBuildError::Observation {
                label: "cargo artifact",
                observed: String::from_utf8_lossy(line).into_owned(),
            });
        };
        built.push(Built {
            name: name.to_owned(),
            kind: kind.to_owned(),
            test,
            executable: PathBuf::from(executable),
        });
    }
    Ok(built)
}

fn missing(subject: &str) -> TargetBuildError {
    TargetBuildError::Observation {
        label: "windows cross executable",
        observed: format!("Cargo reported no {subject} executable"),
    }
}

/// The named inputs the release set is a function of, each as a SHA-256
/// digest: source archive, `Cargo.lock`, the exact compiler, the target, every
/// Cargo argument vector and candidate setting, the MSVC generation, and the
/// cross image. A candidate linking a canvas also names that canvas's exact
/// bytes, so bundles linking different canvases never share a key.
fn build_key<E: Executor>(
    root: &Path,
    archive: &SourceArchive,
    image: CrossImage<'_>,
    programs: &Programs,
    executor: &E,
) -> Result<BTreeMap<String, String>, TargetBuildError> {
    let lock = root.join("Cargo.lock");
    let lock = fs::read(&lock).map_err(|source| TargetBuildError::Io { path: lock, source })?;
    let features = executor
        .records()
        .iter()
        .filter(|record| record.program == programs.cargo)
        .map(|record| {
            record
                .arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let candidate_settings = candidate()
        .environment()
        .map(|(name, value)| format!("{name}={value}"))
        .chain([
            candidate().rustflags("{workspace}"),
            candidate().debug_config().to_owned(),
        ])
        .collect::<Vec<_>>();
    let features = serde_json::to_vec(&(features, candidate_settings)).map_err(|error| {
        TargetBuildError::Observation {
            label: "windows cross arguments",
            observed: error.to_string(),
        }
    })?;
    let compiler = successful(executor.execute(CommandSpec::new(
        "rustc-identity",
        &programs.rustc,
        ["-vV"],
    ))?)?;
    let image_id = image.image.strip_prefix("sha256:").unwrap_or(image.image);
    Ok(BTreeMap::from([
        ("cargo_lock".to_owned(), sha256(&lock)),
        ("cross_image".to_owned(), format!("sha256:{image_id}")),
        ("features".to_owned(), sha256(&features)),
        (
            "msvc".to_owned(),
            format!("sha256:{}", image.msvc_generation),
        ),
        ("rust_toolchain".to_owned(), sha256(&compiler.stdout)),
        ("source".to_owned(), format!("sha256:{}", archive.digest)),
        ("target".to_owned(), sha256(TARGET.as_str().as_bytes())),
    ]))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Replace `out` with the artifact root, the observed toolchain, and the
/// build key.
fn publish(
    out: &Path,
    placed: &[Placed],
    inputs: &BTreeMap<String, String>,
    toolchain: &prebuilt::Toolchain,
) -> Result<(), TargetBuildError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| TargetBuildError::Io { path, source }
    };
    let root = out.join(ROOT);
    match fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io(&root)(source)),
    }
    for file in placed {
        let destination = root.join(&file.path);
        let parent = destination
            .parent()
            .expect("artifact paths name a directory");
        fs::create_dir_all(parent).map_err(io(parent))?;
        fs::copy(&file.source, &destination).map_err(io(&file.source))?;
    }
    let recorded = root.join(prebuilt::TOOLCHAIN);
    let encoded = serde_json::to_vec(toolchain).map_err(|error| TargetBuildError::Observation {
        label: "windows cross toolchain",
        observed: error.to_string(),
    })?;
    fs::write(&recorded, encoded).map_err(io(&recorded))?;
    let key = out.join(BUILD_KEY);
    let encoded = serde_json::to_vec(inputs).map_err(|error| TargetBuildError::Observation {
        label: "windows cross build key",
        observed: error.to_string(),
    })?;
    fs::write(&key, encoded).map_err(io(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reported_executables_are_placed_with_their_test_profile() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"gate","kind":["lib"]},"profile":{"test":false},"executable":null}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"suite","kind":["test"]},"profile":{"test":true},"executable":"/t/suite-1.exe"}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        let built = reported_executables(stdout.as_bytes()).unwrap();
        assert_eq!(built.len(), 1);
        assert!(built[0].test);
        assert_eq!(
            (built[0].kind.as_str(), built[0].name.as_str()),
            ("test", "suite")
        );
        assert_eq!(built[0].executable, PathBuf::from("/t/suite-1.exe"));
    }
}
