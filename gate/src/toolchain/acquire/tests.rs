use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use super::*;
use crate::toolchain::{ArmMeasurement, measure_toolchain_arm};

mod environment;
mod scanners;

/// One scratch tree beside one rim, listing exactly the paths written into it.
struct Fixture {
    root: PathBuf,
    tree: PathBuf,
    paths: Vec<String>,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = crate::toolchain::exclusive_scratch_root(label);
        fs::create_dir_all(root.join("rim/bin")).expect("create fixture rim");
        let tree = root.join("tree");
        fs::create_dir(&tree).expect("create fixture tree");
        Self {
            root,
            tree,
            paths: Vec::new(),
        }
    }

    fn write(&mut self, path: &str, text: &str) {
        let file = self.tree.join(path);
        fs::create_dir_all(file.parent().expect("a fixture file has a parent"))
            .expect("create fixture directory");
        fs::write(&file, text).expect("write fixture file");
        if let Err(at) = self
            .paths
            .binary_search_by(|known| known.as_str().cmp(path))
        {
            self.paths.insert(at, path.to_owned());
        }
    }

    /// One rim program with exact bytes and the requested mode.
    fn program(&self, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
        let path = self.root.join("rim/bin").join(name);
        fs::write(&path, bytes).expect("write fixture program");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .expect("set fixture program mode");
        path
    }

    fn source(&self, changes: &[&str]) -> ToolchainSource<'_> {
        ToolchainSource {
            tree: None,
            root: &self.tree,
            repository: &self.root,
            paths: &self.paths,
            rim: RimCapture::new(
                self.root.join("rim"),
                Some(OsString::from("bin")),
                changes.iter().map(|path| (*path).to_owned()),
            )
            .expect("the fixture rim is absolute"),
            reached_paths: None,
            baselines: None,
            compiler_environment: &[],
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove toolchain fixture");
    }
}

/// Observe and admit one arm whose compiled analysis declares no program.
fn admitted(
    source: &ToolchainSource<'_>,
    id: GateId,
    environment_names: &[String],
) -> ArmMeasurement {
    let declaration = TOOLCHAIN_ARMS
        .into_iter()
        .find(|declaration| declaration.id == id)
        .expect("the arm is declared");
    assert!(
        declaration.tools.is_empty(),
        "{} declares programs, so its analysis is not reached in process",
        id.as_str()
    );
    let bounds = crate::input::current_bounds().expect("compiled bounds are valid");
    let observation = observe(
        scoped_declaration(declaration, source),
        source,
        &bounds,
        environment_names,
    );
    measure_toolchain_arm(observation).expect("the observation is admitted")
}

/// The one-based character column of `needle` within `line`.
fn column(line: &str, needle: &str) -> u32 {
    let start = line
        .find(needle)
        .expect("the fixture line names the needle");
    u32::try_from(line[..start].chars().count() + 1).expect("a fixture column fits")
}

fn cargo_requirement() -> &'static ToolRequirement {
    TOOLCHAIN_ARMS
        .iter()
        .flat_map(|declaration| declaration.tools)
        .find(|requirement| requirement.program == "cargo")
        .expect("a declaration requires cargo")
}

fn whole(bytes: &[u8]) -> Option<Stream<'_>> {
    Some(Stream {
        bytes,
        truncated: false,
        complete: true,
    })
}

#[test]
fn executable_authority_moves_with_exact_bytes_without_launching() {
    let fixture = Fixture::new("gate-tool-byte-authority");
    fixture.program("rustfmt", b"first executable authority", 0o755);
    let source = fixture.source(&[]);
    let before = toolchain_executable_authority(&source).unwrap();
    fixture.program("rustfmt", b"second and longer executable authority", 0o755);
    let after = toolchain_executable_authority(&source).unwrap();

    assert_ne!(before, after);
}

/// The bytes hashed are the bytes of the file that answered, and what it
/// printed is retained exactly rather than a cleaned-up rendering of it.
#[test]
fn an_identified_program_carries_its_resolved_bytes_and_exact_printed_version() {
    let fixture = Fixture::new("gate-tool-identity");
    let requirement = cargo_requirement();
    let bytes = b"fixture cargo executable";
    let resolved = fixture.program(requirement.program, bytes, 0o755);
    let printed = format!("{} \n", requirement.exact_version);
    let asked = std::cell::RefCell::new(Vec::new());

    let evidence = probe_with(
        requirement,
        &fixture.source(&[]),
        read_program,
        &|program, _| {
            asked.borrow_mut().push(program.to_path_buf());
            Ok(printed.clone())
        },
    );

    assert_eq!(*asked.borrow(), std::slice::from_ref(&resolved));
    assert_eq!(
        evidence,
        ToolEvidence::Identified {
            name: requirement.name.to_owned(),
            requested_program: requirement.program.to_owned(),
            resolved_program: resolved.to_string_lossy().into_owned(),
            version: printed,
            digest: Digest::compute(Domain::GateTool, bytes).as_str().to_owned(),
        }
    );
}

/// An absent program and an unusable one are different facts about the
/// machine: only the absent one is missing, and the unusable one keeps the
/// exact file it resolved to.
#[test]
fn an_unresolved_program_is_missing_and_an_unusable_one_keeps_its_resolution() {
    let fixture = Fixture::new("gate-tool-unusable");
    let requirement = cargo_requirement();
    let never: VersionProbe<'_> =
        &|program, _| panic!("{} was probed without being launchable", program.display());

    assert_eq!(
        probe_with(requirement, &fixture.source(&[]), read_program, never),
        ToolEvidence::Missing {
            name: requirement.name.to_owned(),
            requested_program: requirement.program.to_owned(),
        }
    );

    let resolved = fixture.program(requirement.program, b"not executable", 0o644);
    let unprobed = |failure| ToolEvidence::Unprobed {
        name: requirement.name.to_owned(),
        requested_program: requirement.program.to_owned(),
        resolved_program: resolved.to_string_lossy().into_owned(),
        failure,
    };
    assert_eq!(
        probe_with(requirement, &fixture.source(&[]), read_program, never),
        unprobed(ProbeFailure::NotStarted)
    );

    fixture.program(requirement.program, b"refusing", 0o755);
    assert_eq!(
        probe_with(requirement, &fixture.source(&[]), read_program, &|_, _| {
            Err(ProbeFailure::Refused { code: 41 })
        }),
        unprobed(ProbeFailure::Refused { code: 41 })
    );
}

#[test]
fn a_version_probe_identifies_only_a_clean_whole_decodable_version() {
    let exited = |code: i32| Completion::Exited(ExitStatus::from_raw(code << 8));
    let printed = "cargo 1.0.0 (fixture)\n";

    assert_eq!(
        printed_version(&exited(0), whole(printed.as_bytes())),
        Ok(printed.to_owned())
    );
    assert_eq!(
        printed_version(&exited(41), whole(printed.as_bytes())),
        Err(ProbeFailure::Refused { code: 41 })
    );
    assert_eq!(
        printed_version(
            &Completion::Exited(ExitStatus::from_raw(9)),
            whole(printed.as_bytes())
        ),
        Err(ProbeFailure::Signaled)
    );
    assert_eq!(
        printed_version(&Completion::TimedOut(ExitStatus::from_raw(9)), None),
        Err(ProbeFailure::TimedOut)
    );
    assert_eq!(
        printed_version(&Completion::Cancelled(ExitStatus::from_raw(9)), None),
        Err(ProbeFailure::Signaled)
    );
    assert_eq!(
        printed_version(&exited(0), None),
        Err(ProbeFailure::Incomplete)
    );
    for (truncated, complete) in [(true, true), (false, false)] {
        assert_eq!(
            printed_version(
                &exited(0),
                Some(Stream {
                    bytes: printed.as_bytes(),
                    truncated,
                    complete,
                })
            ),
            Err(ProbeFailure::Incomplete)
        );
    }
    assert_eq!(
        printed_version(&exited(0), whole(b"cargo \xff\n")),
        Err(ProbeFailure::Undecodable)
    );
    for empty in [&b"\n"[..], b"\r\n", b""] {
        assert_eq!(
            printed_version(&exited(0), whole(empty)),
            Err(ProbeFailure::Incomplete)
        );
    }
}
