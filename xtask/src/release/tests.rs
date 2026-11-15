use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use spice::serve::canvas::BUILD;

mod artifact_capture;
#[cfg(unix)]
mod signing;

static NEXT: AtomicU64 = AtomicU64::new(0);
const COMMIT: &str = "1111111111111111111111111111111111111111";
const SOURCE_ARCHIVE: &[u8] = b"fixture release source archive";
const MOVED_ARCHIVE: &[u8] = b"rewritten release source archive";
const TEST_WASM: &[u8] = b"lane-built wasm";
const GENERATION: &str = "6666666666666666666666666666666666666666666666666666666666666666";
const UUID: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_UUID: &str = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE";

struct Fixture {
    root: PathBuf,
    archive: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "spice-release-builder-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("dist/stage")).expect("fixture stage directory");
        let version = root.join(dist::VERSION_FILE);
        fs::create_dir_all(version.parent().expect("tracked version parent"))
            .expect("fixture version directory");
        fs::write(version, format!("{FIXTURE_VERSION}\n")).expect("write tracked fixture version");
        let build_record = dist::build_record_bytes(&fixture_build());
        fs::write(root.join(dist::BUILD_RECORD_FILE), &build_record)
            .expect("write exported fixture identity");
        fs::write(
            root.join("dist/stage").join(dist::SOURCE_ARCHIVE_FILE),
            SOURCE_ARCHIVE,
        )
        .expect("write fixture source archive");
        let generations = root
            .join(spice::serve::APP_STAGE)
            .join(spice::serve::APP_STAGE_GENERATIONS);
        let app = generations.join(GENERATION);
        fs::create_dir_all(&app).expect("fixture app generation");
        let shell = format!("<main id=\"app\" data-app-generation=\"{GENERATION}\"></main>");
        let mut files = spice::serve::APP
            .assets()
            .iter()
            .map(|asset| {
                let contents = match asset.file() {
                    "app_bg.wasm" => TEST_WASM,
                    "app.html" => shell.as_bytes(),
                    _ => b"lane-built app asset",
                };
                fs::write(app.join(asset.file()), contents).expect("write lane-built app asset");
                (format!("app/{}", asset.file()), contents)
            })
            .collect::<Vec<_>>();
        let staged = spice::serve::canvas::gzip::write(
            files.iter().map(|(path, bytes)| (path.as_str(), *bytes)),
        )
        .expect("app archive writes");
        fs::write(
            generations
                .join(GENERATION)
                .with_extension(spice::serve::canvas::ARCHIVE_EXTENSION),
            &staged,
        )
        .expect("write lane app archive");
        for (source, member, contents) in [
            ("LICENSE", "LICENSE.txt".to_owned(), b"license\n"),
            ("spice/agent/SKILL.md", "SKILL.md".to_owned(), b"# skill\n"),
            (
                "gate/defaults/spice.toml",
                "spice.toml".to_owned(),
                b"[policy]",
            ),
            ("docs/guide.md", "docs/guide.md".to_owned(), b"# guide\n"),
        ] {
            let source = root.join(source);
            fs::create_dir_all(source.parent().expect("source has a parent"))
                .expect("create tracked source directory");
            fs::write(&source, contents).expect("write tracked source");
            files.push((member, contents));
        }
        files.push((BUILD.to_owned(), build_record.as_slice()));
        let archive = spice::serve::canvas::gzip::write(
            files.iter().map(|(path, bytes)| (path.as_str(), *bytes)),
        )
        .expect("rack writes");
        fs::write(
            root.join(spice::serve::APP_STAGE)
                .join(spice::serve::APP_STAGE_CURRENT),
            format!("{GENERATION}\n"),
        )
        .expect("write lane app pointer");
        Self { root, archive }
    }

    fn programs(&self) -> Programs {
        let program = |name: &str| self.root.join("programs").join(name);
        Programs {
            cargo: program("cargo"),
            rustc: program("rustc"),
            trunk: program("trunk"),
            xtask: program("xtask"),
            macos: Some(MacPrograms {
                dsymutil: program("dsymutil"),
                dwarfdump: program("dwarfdump"),
                strip: program("strip"),
            }),
        }
    }

    fn pointer(&self, target: ReleaseTarget) -> PathBuf {
        self.root
            .join("dist/stage/targets")
            .join(target.as_str())
            .join("payload.json")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("fixture cleanup");
    }
}

struct FakeExecutor {
    root: PathBuf,
    target: ReleaseTarget,
    archive: Vec<u8>,
    trunk: RefCell<String>,
    trunk_drift_after_native: Cell<bool>,
    refusing: Cell<Option<&'static str>>,
    tracked_dirty: Cell<bool>,
    untracked: RefCell<String>,
    index_flags: RefCell<String>,
    moved_after_native: Cell<bool>,
    moved_symbols_after_uuid: Cell<bool>,
    native_ran: Cell<bool>,
    executable_uuid: RefCell<Vec<u8>>,
    dsym_uuid: RefCell<Vec<u8>>,
    records: RefCell<Vec<CommandRecord>>,
    keychain_holds_identity: Cell<bool>,
}

impl FakeExecutor {
    fn new(fixture: &Fixture, target: ReleaseTarget) -> Self {
        let sysroot = fixture.root.join("rust-sysroot");
        for installed in [WASM_TARGET, target.as_str()] {
            let library = sysroot.join("lib/rustlib").join(installed).join("lib");
            fs::create_dir_all(&library).expect("create installed target fixture");
            fs::write(library.join("libstd-fixture.rlib"), b"std")
                .expect("write installed target fixture");
        }
        Self {
            root: fixture.root.clone(),
            target,
            archive: fixture.archive.clone(),
            trunk: RefCell::new(PINNED_TRUNK.to_owned()),
            trunk_drift_after_native: Cell::new(false),
            refusing: Cell::new(None),
            tracked_dirty: Cell::new(false),
            untracked: RefCell::new(String::new()),
            index_flags: RefCell::new("H xtask/src/lib.rs\0".to_owned()),
            moved_after_native: Cell::new(false),
            moved_symbols_after_uuid: Cell::new(false),
            native_ran: Cell::new(false),
            executable_uuid: RefCell::new(format!("UUID: {UUID} (arm64) executable\n").into()),
            dsym_uuid: RefCell::new(format!("UUID: {UUID} (arm64) symbols\n").into()),
            records: RefCell::new(Vec::new()),
            keychain_holds_identity: Cell::new(true),
        }
    }

    fn labels(&self) -> Vec<&'static str> {
        self.records.borrow().iter().map(|row| row.label).collect()
    }
}

impl Executor for FakeExecutor {
    fn execute(&self, command: CommandSpec) -> Result<CommandRecord, TargetBuildError> {
        let refused = self.refusing.get() == Some(command.label);
        if command.label == "native-build" && !refused {
            let destination = native_path(&self.root, self.target);
            fs::create_dir_all(destination.parent().expect("native parent"))
                .expect("native directory");
            fs::write(&destination, linked(self.target, &self.archive, b"linked"))
                .expect("link fixture executable");
            if self.target == ReleaseTarget::X86_64PcWindowsMsvc {
                fs::write(destination.with_extension("pdb"), b"fixture PDB")
                    .expect("write fixture PDB");
            }
            self.native_ran.set(true);
            if self.moved_after_native.get() {
                fs::write(
                    self.root.join("dist/stage").join(dist::SOURCE_ARCHIVE_FILE),
                    MOVED_ARCHIVE,
                )
                .expect("rewrite fixture source archive");
            }
        }
        if command.label == "dsym-generate" && !refused {
            let dwarf = PathBuf::from(&command.arguments[2]).join("Contents/Resources/DWARF/spice");
            fs::create_dir_all(dwarf.parent().expect("dSYM DWARF parent"))
                .expect("create derived dSYM bundle");
            fs::write(
                PathBuf::from(&command.arguments[2]).join("Contents/Info.plist"),
                b"fixture plist",
            )
            .expect("write derived dSYM metadata");
            fs::write(dwarf, b"fixture DWARF").expect("write derived DWARF member");
        }
        if command.label == "native-strip" && !refused {
            fs::write(
                PathBuf::from(&command.arguments[0]),
                linked(self.target, &self.archive, b"stripped"),
            )
            .expect("strip exact linked executable fixture");
            if self.moved_symbols_after_uuid.get() {
                let dwarf = dsym_path(Path::new(&command.arguments[0]))
                    .join("Contents/Resources/DWARF/spice");
                fs::rename(&dwarf, dwarf.with_extension("captured"))
                    .expect("retain captured DWARF fixture");
                fs::write(dwarf, b"changed DWARF")
                    .expect("substitute same-length regular DWARF fixture");
            }
        }
        let stdout = match command.label {
            "rust-version" => b"rustc 1.97.1 (fixture)\n".to_vec(),
            "rust-sysroot" => {
                format!("{}\n", self.root.join("rust-sysroot").display()).into_bytes()
            }
            "trunk-version" if self.trunk_drift_after_native.get() && self.native_ran.get() => {
                b"trunk 0.21.13\n".to_vec()
            }
            "trunk-version" => format!("{}\n", self.trunk.borrow()).into_bytes(),
            "native-build" => {
                let executable = native_path(&self.root, self.target);
                format!(
                    "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"spice\",\"kind\":[\"bin\"]}},\"executable\":{}}}\n",
                    serde_json::to_string(&executable.display().to_string())
                        .expect("fixture path encodes")
                )
                .into_bytes()
            }
            "executable-uuid" => self.executable_uuid.borrow().clone(),
            "dsym-uuid" => self.dsym_uuid.borrow().clone(),
            "app-build" | "dsym-generate" | "native-strip" => {
                format!("{} complete\n", command.label).into_bytes()
            }
            #[cfg(unix)]
            label if signing::STEPS.contains(&label) => self.signing_step(&command, refused),
            "canvas-version" => format!("spice {FIXTURE_VERSION} app {GENERATION}\n").into_bytes(),
            "source-status" if self.tracked_dirty.get() => b" M xtask/src/lib.rs\n".to_vec(),
            "source-status" => self.untracked.borrow().clone().into_bytes(),
            "source-index-flags" => self.index_flags.borrow().clone().into_bytes(),
            label => panic!("unexpected command label {label}"),
        };
        let record = CommandRecord {
            label: command.label,
            program: command.program,
            arguments: command.arguments,
            code: Some(i32::from(refused)),
            stdout,
            stderr: Vec::new(),
        };
        self.records.borrow_mut().push(record.clone());
        Ok(record)
    }

    fn records(&self) -> Vec<CommandRecord> {
        self.records.borrow().clone()
    }
}

/// A linkable image whose app section holds `archive`, followed by `tag` so
/// each processing step leaves distinct executable bytes.
fn linked(target: ReleaseTarget, archive: &[u8], tag: &[u8]) -> Vec<u8> {
    let mut image = if target == ReleaseTarget::X86_64PcWindowsMsvc {
        pe(archive)
    } else {
        rack::link::object(archive, rack::link::Target::LinuxX86_64)
    };
    image.extend(tag);
    image
}

fn native_path(root: &Path, target: ReleaseTarget) -> PathBuf {
    root.join("target/release-appliance")
        .join(target.as_str())
        .join("release")
        .join(if target == ReleaseTarget::X86_64PcWindowsMsvc {
            "spice.exe"
        } else {
            "spice"
        })
}

fn pe(archive: &[u8]) -> Vec<u8> {
    const PE_HEADER: usize = 0x80;
    const OPTIONAL_HEADER: usize = 0xf0;
    const TABLE: usize = PE_HEADER + 4 + 20 + OPTIONAL_HEADER;
    const RAW: usize = 0x200;

    let mut image = vec![0_u8; RAW];
    image[0..2].copy_from_slice(b"MZ");
    image[0x3c..0x40].copy_from_slice(&(PE_HEADER as u32).to_le_bytes());
    image[PE_HEADER..PE_HEADER + 4].copy_from_slice(b"PE\0\0");
    let coff = PE_HEADER + 4;
    image[coff..coff + 2].copy_from_slice(&0x8664_u16.to_le_bytes());
    image[coff + 2..coff + 4].copy_from_slice(&1_u16.to_le_bytes());
    image[coff + 16..coff + 18].copy_from_slice(&(OPTIONAL_HEADER as u16).to_le_bytes());
    let optional = coff + 20;
    image[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
    image[optional + 108..optional + 112].copy_from_slice(&16_u32.to_le_bytes());
    image[TABLE..TABLE + 8].copy_from_slice(b"spicezip");
    image[TABLE + 8..TABLE + 12].copy_from_slice(&(archive.len() as u32).to_le_bytes());
    image[TABLE + 16..TABLE + 20].copy_from_slice(&(archive.len() as u32).to_le_bytes());
    image[TABLE + 20..TABLE + 24].copy_from_slice(&(RAW as u32).to_le_bytes());
    image.extend_from_slice(archive);
    image
}

#[test]
fn all_targets_use_the_fixed_commands_and_real_artifact_bytes() {
    for target in [
        ReleaseTarget::Aarch64AppleDarwin,
        ReleaseTarget::X86_64UnknownLinuxMusl,
        ReleaseTarget::X86_64PcWindowsMsvc,
    ] {
        let fixture = Fixture::new();
        let programs = fixture.programs();
        let executor = FakeExecutor::new(&fixture, target);

        let build = stage_with(
            &fixture.root,
            &fixture.root.join("target"),
            target,
            &programs,
            &executor,
        )
        .expect("target stage succeeds");

        let build_records = build
            .commands
            .iter()
            .filter(|record| record.label.ends_with("-build"))
            .collect::<Vec<_>>();
        assert_eq!(
            build_records
                .iter()
                .map(|record| record.label)
                .collect::<Vec<_>>(),
            ["app-build", "native-build"]
        );
        assert_eq!(build_records[0].arguments, [OsString::from("app")]);
        assert_eq!(build_records[0].program, programs.xtask);
        let staged = assert_native_build(
            build_records[1],
            &programs,
            target,
            &fixture.root,
            &fixture.archive,
        );
        let rust_observations = build
            .commands
            .iter()
            .filter(|record| record.label == "rust-version")
            .collect::<Vec<_>>();
        assert_eq!(rust_observations.len(), 2);
        assert!(
            rust_observations
                .iter()
                .all(|record| record.program == programs.rustc),
            "preflight and publisher recheck must execute the same resolved compiler"
        );
        assert!(build_records.iter().all(|record| {
            record.success() && !record.stdout.is_empty() && record.stderr.is_empty()
        }));
        assert_eq!(
            build.staged.payload.app,
            GENERATION.parse().expect("fixture generation is canonical")
        );
        assert_eq!(
            build.staged.payload.canvas,
            Digest::compute(Domain::DistArtifact, &fixture.archive)
        );
        assert_eq!(build.staged.canvas, *staged.canvas());
        let mut inventory = vec![target.executable_path()];
        if let Some(debug) = target.debug_path() {
            inventory.push(debug);
        }
        assert_eq!(
            build
                .staged
                .payload
                .inventory
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            inventory
        );
        let native = linked(
            target,
            &fixture.archive,
            if target == ReleaseTarget::Aarch64AppleDarwin {
                b"stripped"
            } else {
                b"linked"
            },
        );
        assert_eq!(
            build.staged.payload.native,
            Digest::compute(Domain::DistArtifact, &native)
        );
        assert_symbol_result(&fixture, &programs, &build);
        assert_eq!(
            fs::read(fixture.pointer(target)).expect("target pointer reads"),
            build.staged.payload_bytes
        );
    }
}

/// Sealed Linux links the generation staged by the release host instead of
/// compiling a second, host-dependent Wasm image inside the container.
#[test]
fn a_prebuilt_canvas_skips_the_target_app_build() {
    let fixture = Fixture::new();
    let target = ReleaseTarget::X86_64UnknownLinuxMusl;
    let programs = fixture.programs();
    let executor = FakeExecutor::new(&fixture, target);
    let archive = SourceArchive::read(
        &fixture
            .root
            .join("dist/stage")
            .join(dist::SOURCE_ARCHIVE_FILE),
    )
    .expect("fixture source archive");
    let canvas = crate::app::StagedCanvas::read(&fixture.root, &fixture_build())
        .expect("fixture host app is staged");
    let version = ProductVersion::parse(FIXTURE_VERSION).expect("fixture version is canonical");

    let build = stage_with_roots(
        &fixture.root,
        &fixture.root.join("target"),
        &fixture.root.join("dist/stage"),
        StageRequest {
            target,
            version: &version,
            signing: None,
            archive: &archive,
            proof_container: FIXTURE_PROOF_IMAGE,
            canvas: Some(canvas),
        },
        &programs,
        &executor,
    )
    .expect("target stage reuses host canvas");

    assert!(!executor.labels().contains(&"app-build"));
    assert_eq!(
        build.staged.payload.app,
        GENERATION.parse().expect("fixture generation is canonical")
    );
}

#[test]
fn a_substituted_dsym_member_refuses_before_target_publication() {
    let fixture = Fixture::new();
    let target = ReleaseTarget::Aarch64AppleDarwin;
    let executor = FakeExecutor::new(&fixture, target);
    executor.moved_symbols_after_uuid.set(true);
    let error = stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        target,
        &fixture.programs(),
        &executor,
    )
    .expect_err("a substituted dSYM member refuses before target publication");
    assert!(matches!(error, TargetBuildError::Stage(_)), "{error:?}");
    assert!(!fixture.pointer(target).exists());
}

fn fixture_build() -> wire::manifest::BuildInfo {
    wire::manifest::BuildInfo {
        product: wire::manifest::ProductVersion::parse(FIXTURE_VERSION).expect("fixture version"),
        source: Some(wire::manifest::BuildSource {
            commit: COMMIT.to_owned(),
            committed: 1_700_000_000,
        }),
    }
}

/// The native build runs Cargo, carries line tables for declared debug
/// artifacts, and links last an object carrying exactly the staged archive.
fn assert_native_build(
    record: &CommandRecord,
    programs: &Programs,
    target: ReleaseTarget,
    root: &Path,
    archive: &[u8],
) -> crate::app::StagedCanvas {
    assert_eq!(record.program, programs.cargo);
    assert_eq!(
        record
            .arguments
            .iter()
            .any(|argument| argument == "profile.release.debug=\"line-tables-only\""),
        target.debug_path().is_some()
    );
    assert_eq!(
        record
            .arguments
            .iter()
            .any(|argument| argument == "profile.release.strip=\"symbols\""),
        target == ReleaseTarget::X86_64UnknownLinuxMusl
    );
    let binary = record
        .arguments
        .windows(2)
        .find_map(|pair| (pair[0] == "--bin").then_some(pair[1].as_os_str()))
        .expect("the native build selects one binary");
    assert_eq!(binary, OsStr::new("spice"));
    assert!(
        !record
            .arguments
            .iter()
            .any(|argument| argument == "--no-default-features")
    );
    let link = record
        .arguments
        .iter()
        .filter_map(|argument| argument.to_str())
        .find_map(|argument| argument.strip_prefix("link-arg="))
        .expect("the native build links the stable canvas object");
    let object = fs::read(link).expect("the canvas object is written");
    let staged =
        crate::app::StagedCanvas::read(root, &fixture_build()).expect("fixture canvas is staged");
    let object_target = match target {
        ReleaseTarget::Aarch64AppleDarwin => rack::link::Target::MacAarch64,
        ReleaseTarget::X86_64UnknownLinuxMusl => rack::link::Target::LinuxX86_64,
        ReleaseTarget::X86_64PcWindowsMsvc => rack::link::Target::WindowsX86_64,
    };
    assert_eq!(object, rack::link::object(archive, object_target));
    staged
}

fn assert_symbol_result(fixture: &Fixture, programs: &Programs, build: &TargetBuild) {
    let target = build.target;
    let Some(symbols) = &build.symbols else {
        assert_ne!(target, ReleaseTarget::Aarch64AppleDarwin);
        assert!(build.staged.payload.symbols.is_none());
        return;
    };
    assert_eq!(target, ReleaseTarget::Aarch64AppleDarwin);
    assert_eq!(symbols.uuid(), UUID);
    let inventory = build
        .staged
        .payload
        .inventory
        .iter()
        .find(|entry| entry.path == "symbols/spice.symbols")
        .expect("Darwin inventory binds symbols");
    let metadata = build
        .staged
        .payload
        .symbols
        .as_ref()
        .expect("Darwin payload carries typed symbols");
    assert_eq!(
        (
            &metadata.path,
            metadata.mode,
            metadata.bytes,
            &metadata.digest
        ),
        (
            &inventory.path,
            inventory.mode,
            inventory.bytes,
            &inventory.digest
        )
    );
    assert_eq!(metadata.uuid, UUID);
    assert_eq!(metadata.executable, build.staged.payload.native);
    let bytes = fs::read(fixture.root.join(format!(
        "dist/stage/objects/{}-{:04o}",
        inventory.digest, inventory.mode
    )))
    .expect("staged symbol artifact reads");
    let artifact =
        wire::manifest::MacSymbolArtifact::decode(&bytes).expect("symbol artifact decodes");
    assert_eq!(artifact.uuid, UUID);
    assert_eq!(artifact.executable, build.staged.payload.native);
    assert_eq!(artifact.commit, COMMIT);
    assert!(!artifact.dirty);
    assert_eq!(artifact.info, b"fixture plist");
    assert_eq!(artifact.dwarf, b"fixture DWARF");
    assert_eq!(inventory.bytes, bytes.len() as u64);
    assert_eq!(
        inventory.digest,
        Digest::compute(Domain::DistArtifact, &bytes)
    );
    let executable = fixture
        .root
        .join("target/release-appliance")
        .join(target.as_str())
        .join("release/spice");
    let bundle = dsym_path(&executable);
    assert_eq!(symbols.bundle(), bundle);
    let macos = programs.macos.as_ref().expect("fixture macOS tools");
    let record = |label| {
        build
            .commands
            .iter()
            .find(|record| record.label == label)
            .unwrap_or_else(|| panic!("missing {label} command"))
    };
    assert_eq!(record("dsym-generate").program, macos.dsymutil);
    assert_eq!(
        record("dsym-generate").arguments,
        [executable.as_os_str(), OsStr::new("-o"), bundle.as_os_str()]
    );
    assert_eq!(record("executable-uuid").program, macos.dwarfdump);
    assert_eq!(
        record("executable-uuid").arguments,
        [OsStr::new("--uuid"), executable.as_os_str()]
    );
    let symbol_uuid = &record("dsym-uuid").arguments;
    assert_eq!(symbol_uuid[0], OsStr::new("--uuid"));
    assert!(
        Path::new(&symbol_uuid[1]).starts_with("/dev/fd"),
        "dSYM UUID must read the held DWARF descriptor: {symbol_uuid:?}"
    );
    assert_eq!(record("native-strip").program, macos.strip);
    assert_eq!(record("native-strip").arguments, [executable.as_os_str()]);
}

fn release_environment(cargo_target_dir: Option<&str>) -> ReleaseEnvironment {
    ReleaseEnvironment {
        path: Some(OsString::from("/tool/bin")),
        home: Some(OsString::from("/home/release")),
        cargo_home: Some(OsString::from("/cargo/home")),
        system_root: Some(OsString::from("/windows")),
        cargo_target_dir: cargo_target_dir.map(OsString::from),
        ..ReleaseEnvironment::default()
    }
}

/// The cleared release environment freezes the compiler discovery already made.
///
/// `programs.rustc` is the workspace-root, selected-`CARGO_HOME`, five-rank
/// answer. Installing it as `RUSTC` puts that answer at Cargo's highest rank
/// for app-build and native-build; copying `CARGO_BUILD_RUSTC`
/// would be a second, lower-ranked answer rather than additional authority.
#[test]
fn system_executor_overrides_machine_local_cargo_build_state() {
    let fixture = Fixture::new();
    let release = release_environment(None);
    let programs = fixture.programs();

    let executor =
        SystemExecutor::new(&fixture.root, &release, &programs.rustc).expect("release executor");
    let entries = executor.environment.entries();
    let value = |name: &str| {
        entries
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_os_str()))
            .unwrap_or_else(|| panic!("missing release environment {name}"))
    };

    assert_eq!(
        value("CARGO_BUILD_BUILD_DIR"),
        fixture
            .root
            .join("target/release-appliance-build")
            .as_os_str()
    );
    assert_eq!(value("CARGO_INCREMENTAL"), "0");
    assert_eq!(value("RUSTC"), programs.rustc.as_os_str());
    assert_eq!(value("SystemRoot"), "/windows");
    assert!(
        entries.iter().all(|(key, _)| key != "CARGO_BUILD_RUSTC"),
        "the resolved RUSTC already outranks CARGO_BUILD_RUSTC"
    );
    // Pinned to the resolved answer rather than left absent: absence would let
    // the sole app-build child, `xtask app`, pick a build root of its own.
    assert_eq!(
        value("CARGO_TARGET_DIR"),
        fixture.root.join("target").as_os_str()
    );
    assert_eq!(value("RUSTC_WRAPPER"), "");
    assert_eq!(value("RUSTC_WORKSPACE_WRAPPER"), "");
}

/// The workspace default and the acquired value are asserted against
/// different roots on purpose. A sealed image mounts the workspace
/// read-only, so a build root that silently stayed under it would be the
/// one failure this resolver exists to prevent, and two cases that agreed
/// on a path could not tell the two arms apart.
#[test]
fn the_build_root_is_the_acquired_scratch_or_the_workspace_and_never_both() {
    let root = Path::new("/workspace");

    assert_eq!(
        release_environment(None).build_root(root),
        root.join("target"),
        "an unset CARGO_TARGET_DIR must fall back to the workspace",
    );
    assert_eq!(
        release_environment(Some("/cargo-target")).build_root(root),
        Path::new("/cargo-target"),
        "an acquired CARGO_TARGET_DIR must displace the workspace entirely",
    );

    let executor = SystemExecutor::new(
        root,
        &release_environment(Some("/cargo-target")),
        Path::new("/tool/bin/rustc"),
    )
    .expect("release executor");
    let entries = executor.environment.entries();
    let value = |name: &str| {
        entries
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_os_str()))
            .unwrap_or_else(|| panic!("missing release environment {name}"))
    };
    assert_eq!(
        value("CARGO_BUILD_BUILD_DIR"),
        OsStr::new("/cargo-target/release-appliance-build"),
    );
    // The `app-build` step runs this same binary, which acquires its build
    // root from `CARGO_TARGET_DIR` at its own rim. Withholding it here
    // would send the app rebuild back under the workspace no matter what
    // the resolver above decided.
    assert_eq!(value("CARGO_TARGET_DIR"), OsStr::new("/cargo-target"));
}

/// `HOME` is Cargo's own rule for locating a Cargo home, so the fallback
/// has to reproduce it rather than invent a second answer, and an explicit
/// `CARGO_HOME` has to win over it.
#[test]
fn the_cargo_home_prefers_the_acquired_value_and_falls_back_to_cargos_own_rule() {
    assert_eq!(
        release_environment(None).cargo_home(),
        Some(OsString::from("/cargo/home")),
    );

    let inherited = ReleaseEnvironment {
        home: Some(OsString::from("/home/release")),
        ..ReleaseEnvironment::default()
    };
    assert_eq!(
        inherited.cargo_home(),
        Some(OsString::from("/home/release/.cargo")),
    );

    let bare = ReleaseEnvironment::default();
    assert_eq!(
        bare.cargo_home(),
        None,
        "with neither value there is no Cargo home to guess at",
    );
}

#[cfg(unix)]
#[test]
fn program_discovery_preserves_a_multicall_proxy_name() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let fixture = Fixture::new();
    let programs = fixture.root.join("programs");
    fs::create_dir(&programs).expect("program directory");
    let rustup = programs.join("rustup");
    fs::write(&rustup, b"#!/bin/sh\n").expect("rustup fixture");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("rustup executable mode");
    symlink("rustup", programs.join("cargo")).expect("cargo proxy symlink");

    let cargo = find_program(programs.as_os_str(), "cargo").expect("cargo proxy");

    assert_eq!(cargo, fs::canonicalize(&programs).unwrap().join("cargo"));
    assert_eq!(cargo.file_name(), Some(OsStr::new("cargo")));
    assert!(
        fs::symlink_metadata(cargo)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn tool_drift_refuses_before_the_first_build_or_publication() {
    let fixture = Fixture::new();
    let target = ReleaseTarget::Aarch64AppleDarwin;
    let programs = fixture.programs();
    let executor = FakeExecutor::new(&fixture, target);
    *executor.trunk.borrow_mut() = "trunk 0.21.13".to_owned();

    let error = stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        target,
        &programs,
        &executor,
    )
    .expect_err("tool drift refuses");

    assert!(matches!(error, TargetBuildError::Pins(_)));
    assert!(
        !executor
            .labels()
            .iter()
            .any(|label| label.ends_with("-build"))
    );
    assert!(!fixture.pointer(target).exists());
}

/// A refusing source status is a failed reading, not a clean checkout.
///
/// Every other source-check test drives what the status command *said*,
/// which is why none of them reach the exit status. Nothing downstream can
/// stand in for it either: the inventory is read as text, an empty
/// inventory is a checkout with nothing to report, and a command that
/// refuses says nothing. So a status the appliance failed to take would
/// read exactly like a checkout that matched HEAD.
#[test]
fn a_refusing_source_status_refuses_rather_than_reading_a_clean_checkout() {
    let fixture = Fixture::new();
    let executor = FakeExecutor::new(&fixture, ReleaseTarget::Aarch64AppleDarwin);
    executor.refusing.set(Some("source-status"));

    let error = verify_checkout(&executor, &fixture.root.join("git"))
        .expect_err("a refusing source status refuses the check");

    assert!(
        matches!(
            &error,
            TargetBuildError::CommandFailed(record) if record.label == "source-status"
        ),
        "{error:?}"
    );
    assert_eq!(executor.labels(), ["source-status"]);
}

#[test]
fn only_release_output_and_unhidden_tracked_source_survive_the_source_check() {
    let fixture = Fixture::new();
    let executor = FakeExecutor::new(&fixture, ReleaseTarget::Aarch64AppleDarwin);
    let git = fixture.root.join("git");

    *executor.untracked.borrow_mut() =
        "?? dist/stage/objects/1f/2e\n?? dist/stage/targets/aarch64-apple-darwin\n?? dist/stage/release/manifest.json\n?? dist/stage/source.tar\n".to_owned();
    verify_checkout(&executor, &git).expect("release output is the build's own product");

    *executor.untracked.borrow_mut() =
        "?? dist/stage/objects/1f/2e\n?? dist/stage/release/manifest.extra\n".to_owned();
    let stray = verify_checkout(&executor, &git).expect_err("a stray untracked path refuses");
    assert!(
        matches!(&stray, TargetBuildError::SourceDirty(reason) if reason.ends_with("manifest.extra")),
        "{stray:?}"
    );

    // Status trusts these index flags, so each flagged entry refuses on its own.
    executor.untracked.borrow_mut().clear();
    for (entry, flag) in [
        ("S xtask/src/release/source.rs", "skip-worktree"),
        ("s xtask/src/release/source.rs", "skip-worktree"),
        ("h xtask/src/release/source.rs", "assume-unchanged"),
    ] {
        *executor.index_flags.borrow_mut() = format!("H Cargo.toml\0{entry}\0");
        let hidden = verify_checkout(&executor, &git).expect_err("a hidden entry refuses");
        assert!(
            matches!(&hidden, TargetBuildError::SourceDirty(reason)
                if reason.contains(flag) && reason.ends_with("xtask/src/release/source.rs")),
            "{entry}: {hidden:?}"
        );
    }

    executor.tracked_dirty.set(true);
    let modified = verify_checkout(&executor, &git).expect_err("a modified tracked path refuses");
    assert!(
        matches!(&modified, TargetBuildError::SourceDirty(reason)
            if reason.starts_with("tracked source differs from HEAD")),
        "{modified:?}"
    );
}

#[test]
fn archive_movement_after_build_preserves_the_previous_target_pointer() {
    let fixture = Fixture::new();
    let target = ReleaseTarget::Aarch64AppleDarwin;
    let programs = fixture.programs();
    let executor = FakeExecutor::new(&fixture, target);
    executor.moved_after_native.set(true);
    let pointer = fixture.pointer(target);
    fs::create_dir_all(pointer.parent().expect("pointer parent")).expect("target directory");
    fs::write(&pointer, b"previous complete target").expect("previous pointer");

    let error = stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        target,
        &programs,
        &executor,
    )
    .expect_err("source movement refuses pointer");

    assert!(matches!(error, TargetBuildError::Stage(_)));
    assert_eq!(
        fs::read(pointer).expect("previous pointer remains"),
        b"previous complete target"
    );
    assert_eq!(
        executor
            .labels()
            .into_iter()
            .filter(|label| label.ends_with("-build"))
            .collect::<Vec<_>>(),
        ["app-build", "native-build"]
    );
}

/// A build command that refuses is the only thing that says the step failed.
///
/// Every artifact this step is followed by reading already exists: the staged
/// app generation and its canvas archive are present in any checkout the
/// appliance has built from before. A build that ignored the exit status would
/// therefore read the bytes that were already there, name their digests in the
/// payload, and publish a target pointer for a build that never ran. The exit
/// status is the whole of the difference, so the refusal has to be asserted
/// where nothing else can stand in for it.
#[test]
fn a_refusing_build_command_publishes_no_pointer_over_the_previous_one() {
    let fixture = Fixture::new();
    let target = ReleaseTarget::Aarch64AppleDarwin;
    let programs = fixture.programs();
    let executor = FakeExecutor::new(&fixture, target);
    executor.refusing.set(Some("app-build"));
    let pointer = fixture.pointer(target);
    fs::create_dir_all(pointer.parent().expect("pointer parent")).expect("target directory");
    fs::write(&pointer, b"previous complete target").expect("previous pointer");

    let error = stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        target,
        &programs,
        &executor,
    )
    .expect_err("a refusing build command refuses the stage");

    assert!(matches!(error, TargetBuildError::Stage(_)), "{error:?}");
    assert_eq!(
        fs::read(&pointer).expect("previous pointer remains"),
        b"previous complete target",
        "a refused build replaced the pointer of the last complete one"
    );
    assert_eq!(
        executor.labels().last(),
        Some(&"app-build"),
        "the refusal has to stop the first build step, not be noticed later"
    );
    assert!(
        crate::app::StagedCanvas::read(&fixture.root, &fixture_build()).is_ok(),
        "the archive the ignored status would have linked must still be readable"
    );

    assert_macos_failure("dsym-generate", |executor| {
        executor.refusing.set(Some("dsym-generate"));
    });
    assert_macos_failure("executable-uuid", |executor| {
        *executor.executable_uuid.borrow_mut() = b"malformed UUID\n".to_vec();
    });
    assert_macos_failure("dsym-uuid", |executor| {
        *executor.dsym_uuid.borrow_mut() =
            format!("UUID: {OTHER_UUID} (arm64) symbols\n").into_bytes();
    });
    assert_macos_failure("native-strip", |executor| {
        executor.refusing.set(Some("native-strip"));
    });
}

fn assert_macos_failure(expected_last: &'static str, configure: impl FnOnce(&FakeExecutor)) {
    let fixture = Fixture::new();
    let target = ReleaseTarget::Aarch64AppleDarwin;
    let programs = fixture.programs();
    let executor = FakeExecutor::new(&fixture, target);
    configure(&executor);
    let pointer = fixture.pointer(target);
    fs::create_dir_all(pointer.parent().expect("pointer parent")).expect("target directory");
    fs::write(&pointer, b"previous complete target").expect("previous pointer");

    let error = stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        target,
        &programs,
        &executor,
    )
    .expect_err("macOS symbol derivation failure refuses the stage");

    assert!(matches!(error, TargetBuildError::Stage(_)), "{error:?}");
    assert_eq!(executor.labels().last(), Some(&expected_last));
    assert_eq!(
        fs::read(pointer).expect("previous pointer remains"),
        b"previous complete target"
    );
}

#[test]
fn tool_drift_after_build_preserves_the_previous_target_pointer() {
    let fixture = Fixture::new();
    let target = ReleaseTarget::Aarch64AppleDarwin;
    let programs = fixture.programs();
    let executor = FakeExecutor::new(&fixture, target);
    executor.trunk_drift_after_native.set(true);
    let pointer = fixture.pointer(target);
    fs::create_dir_all(pointer.parent().expect("pointer parent")).expect("target directory");
    fs::write(&pointer, b"previous complete target").expect("previous pointer");

    let error = stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        target,
        &programs,
        &executor,
    )
    .expect_err("tool drift refuses pointer");

    assert!(matches!(error, TargetBuildError::Stage(_)));
    assert_eq!(
        fs::read(pointer).expect("previous pointer remains"),
        b"previous complete target"
    );
    assert_eq!(
        executor
            .labels()
            .into_iter()
            .filter(|label| label.ends_with("-build"))
            .collect::<Vec<_>>(),
        ["app-build", "native-build"]
    );
}

/// A throwaway tree whose Cargo configs a resolver walk can be aimed at.
///
/// Built under the temporary directory rather than beside the workspace, so
/// the walk this drives climbs directories this fixture owns outright and no
/// lane's checkout is read on the way up.
#[cfg(unix)]
struct ConfigTree {
    root: PathBuf,
}

#[cfg(unix)]
impl ConfigTree {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "spice-release-cargo-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("fixture root");
        Self { root }
    }

    /// Write one Cargo config into the `.cargo` directory of `directory`.
    ///
    /// The name is passed rather than assumed, because which of the two names
    /// a directory carries is itself one of the things under test.
    fn config(&self, directory: &str, name: &str, body: &str) -> PathBuf {
        let cargo = self.root.join(directory).join(".cargo");
        fs::create_dir_all(&cargo).expect("fixture cargo directory");
        let path = cargo.join(name);
        fs::write(&path, body).expect("fixture cargo config");
        path
    }

    /// A Cargo home holding a config, which is not inside a `.cargo` directory.
    ///
    /// Cargo reads `$CARGO_HOME/config.toml` directly, so a fixture that put it
    /// under a `.cargo` child would be measuring a file Cargo never opens.
    fn cargo_home(&self, name: &str, body: &str) -> OsString {
        let home = self.root.join("cargo-home");
        fs::create_dir_all(&home).expect("fixture cargo home");
        fs::write(home.join(name), body).expect("fixture cargo home config");
        home.into_os_string()
    }

    /// Install one executable at an absolute fixture-owned path.
    fn executable(&self, relative: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let executable = self.root.join(relative);
        fs::create_dir_all(executable.parent().expect("fixture executable parent"))
            .expect("fixture executable directory");
        fs::write(&executable, b"#!/bin/sh\n").expect("fixture executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("fixture executable mode");
        executable
    }

    /// A `PATH` whose walk answers, so a config that decides has beaten one.
    ///
    /// Without this every config assertion below would also pass for a
    /// resolver that simply found nothing on `PATH`, which is not the claim.
    fn path_with_rustc(&self) -> OsString {
        let directory = self.root.join("path");
        self.executable("path/rustc");
        directory.into_os_string()
    }

    /// A complete release-tool `PATH`, including a compiler that must lose to
    /// the workspace-root Cargo configuration in the discovery fixture.
    fn release_path(&self) -> (OsString, PathBuf) {
        let directory = self.root.join("release-path");
        for program in ["cargo", "rustc", "trunk"] {
            self.executable(&format!("release-path/{program}"));
        }
        let rustc = directory.join("rustc");
        (directory.into_os_string(), rustc)
    }

    fn build_dir(&self, relative: &str) -> PathBuf {
        let directory = self.root.join(relative);
        fs::create_dir_all(&directory).expect("fixture build directory");
        directory
    }
}

#[cfg(unix)]
impl Drop for ConfigTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("config fixture cleanup");
    }
}

/// An environment naming a `PATH` and a Cargo home, and no compiler override.
#[cfg(unix)]
fn resolving_environment(path: OsString, cargo_home: Option<OsString>) -> ReleaseEnvironment {
    ReleaseEnvironment {
        path: Some(path),
        cargo_home,
        ..ReleaseEnvironment::default()
    }
}

/// Release discovery resolves the compiler from the build's exact hierarchy.
///
/// All three lower-ranked answers are live and distinct: the workspace-root
/// config, the selected non-ambient Cargo home, and `PATH` each name an
/// executable compiler. The workspace answer must win because the release
/// executor runs Cargo from that root. Falling back to the old direct `PATH`
/// walk selects `path_rustc`; resolving from the fixture parent instead of the
/// build cwd skips the workspace config and selects `cargo_home_rustc`.
/// The same discovery door is then offered a malformed config and no compiler
/// at all, proving both typed resolver outcomes cross it unchanged.
#[cfg(unix)]
#[test]
fn program_discovery_resolves_rustc_from_the_workspace_build_hierarchy() {
    let tree = ConfigTree::new();
    let workspace = tree.build_dir("workspace");
    let configured_rustc = tree.executable("configured/rustc");
    let cargo_home_rustc = tree.executable("cargo-home-rustc");
    let (path, path_rustc) = tree.release_path();
    let workspace_config = tree.config(
        "workspace",
        "config.toml",
        &format!(
            "[build]\nrustc = {:?}\n",
            configured_rustc.display().to_string()
        ),
    );
    let cargo_home = tree.cargo_home(
        "config.toml",
        &format!(
            "[build]\nrustc = {:?}\n",
            cargo_home_rustc.display().to_string()
        ),
    );
    let environment = resolving_environment(path, Some(cargo_home));

    let programs = Programs::discover(
        &workspace,
        &environment,
        ReleaseTarget::X86_64UnknownLinuxMusl,
    )
    .expect("the workspace release programs resolve");

    assert_eq!(programs.rustc, configured_rustc);
    assert_ne!(programs.rustc, path_rustc);
    assert_ne!(programs.rustc, cargo_home_rustc);
    assert_eq!(
        programs.cargo,
        fs::canonicalize(tree.root.join("release-path"))
            .expect("release path canonicalizes")
            .join("cargo")
    );

    fs::write(&workspace_config, "[build]\nrustc = 7\n")
        .expect("malformed workspace config installs");
    assert!(matches!(
        Programs::discover(
            &workspace,
            &environment,
            ReleaseTarget::X86_64UnknownLinuxMusl,
        ),
        Err(TargetBuildError::CargoConfig { ref path, .. }) if path == &workspace_config
    ));

    fs::remove_file(&workspace_config).expect("workspace config removes");
    fs::remove_file(tree.root.join("cargo-home/config.toml")).expect("Cargo home config removes");
    fs::remove_file(path_rustc).expect("PATH compiler removes");
    assert!(matches!(
        Programs::discover(
            &workspace,
            &environment,
            ReleaseTarget::X86_64UnknownLinuxMusl,
        ),
        Err(TargetBuildError::MissingProgram("rustc"))
    ));
}

/// A Cargo config naming `build.rustc` decides ahead of the `PATH` walk.
///
/// The `PATH` here really does answer -- it holds an executable `rustc` -- so
/// the config winning is the config outranking a live alternative rather than
/// filling a gap. Measured against the installed Cargo: a build with
/// `build.rustc` in a config and a healthy compiler on `PATH` dies on the one
/// the config names.
#[cfg(unix)]
#[test]
fn a_cargo_config_names_the_compiler_ahead_of_the_path_walk() {
    let tree = ConfigTree::new();
    tree.config("", "config.toml", "[build]\nrustc = \"/named/by/config\"\n");
    let environment = resolving_environment(tree.path_with_rustc(), None);

    assert_eq!(
        environment
            .rustc(&tree.build_dir("app"))
            .expect("a readable config resolves"),
        Some(PathBuf::from("/named/by/config")),
    );
}

/// The nearer config decides, and any of them decides over the Cargo home.
///
/// Three ranks in one assertion set because they are one ordering: the walk
/// runs outward from the build directory and the Cargo home is asked only
/// after it runs out. Splitting them into separate fixtures would let a
/// resolver that asked the Cargo home first pass two of the three.
#[cfg(unix)]
#[test]
fn the_nearest_cargo_config_decides_and_the_cargo_home_is_asked_last() {
    let tree = ConfigTree::new();
    tree.config("outer", "config.toml", "[build]\nrustc = \"/outer\"\n");
    tree.config("outer/mid", "config.toml", "[build]\nrustc = \"/mid\"\n");
    let cargo_home = tree.cargo_home("config.toml", "[build]\nrustc = \"/cargo-home\"\n");
    let path = tree.path_with_rustc();
    let build_dir = tree.build_dir("outer/mid/app");

    assert_eq!(
        resolving_environment(path.clone(), Some(cargo_home.clone()))
            .rustc(&build_dir)
            .expect("a readable config resolves"),
        Some(PathBuf::from("/mid")),
        "the nearer config did not decide",
    );
    assert_eq!(
        resolving_environment(path.clone(), Some(cargo_home.clone()))
            .rustc(&tree.build_dir("outer/app"))
            .expect("a readable config resolves"),
        Some(PathBuf::from("/outer")),
        "an ancestor config did not outrank the Cargo home",
    );
    assert_eq!(
        resolving_environment(path, Some(cargo_home))
            .rustc(&tree.build_dir("elsewhere/app"))
            .expect("a readable config resolves"),
        Some(PathBuf::from("/cargo-home")),
        "the Cargo home did not decide when nothing in the walk named a compiler",
    );
}

/// A config that leaves the key unset does not end the walk.
///
/// Cargo merges these files key by key, so a nearer config that says nothing
/// about `build.rustc` leaves the question to its parent. A resolver that
/// stopped at the first file it found would answer from `PATH` here, which is
/// a different compiler than the one Cargo would run.
#[cfg(unix)]
#[test]
fn a_cargo_config_without_the_key_leaves_the_question_to_its_parent() {
    let tree = ConfigTree::new();
    tree.config("outer", "config.toml", "[build]\nrustc = \"/outer\"\n");
    tree.config("outer/mid", "config.toml", "[net]\noffline = true\n");
    let environment = resolving_environment(tree.path_with_rustc(), None);

    assert_eq!(
        environment
            .rustc(&tree.build_dir("outer/mid/app"))
            .expect("a readable config resolves"),
        Some(PathBuf::from("/outer")),
    );
}

/// Within one directory the extension-less name is the one Cargo reads.
///
/// Measured against the installed Cargo: a `.cargo` holding both warns that
/// `config` is deprecated in favour of `config.toml` and then uses `config`.
/// The two files name different compilers here, because with the same name in
/// both a reader that opened the wrong one would answer correctly anyway.
#[cfg(unix)]
#[test]
fn the_extension_less_cargo_config_is_the_one_read() {
    let tree = ConfigTree::new();
    tree.config("", "config", "[build]\nrustc = \"/extension-less\"\n");
    tree.config("", "config.toml", "[build]\nrustc = \"/dot-toml\"\n");
    let environment = resolving_environment(tree.path_with_rustc(), None);

    assert_eq!(
        environment
            .rustc(&tree.build_dir("app"))
            .expect("a readable config resolves"),
        Some(PathBuf::from("/extension-less")),
    );
}

#[test]
fn invalid_compiler_configuration_is_not_an_absent_selection() {
    let path = Path::new("config.toml");
    for document in [
        "[build\nrustc = \"/x\"\n",
        "build = 7\n",
        "[build]\nrustc = 7\n",
    ] {
        assert!(matches!(
            named_rustc(document, path),
            Err(TargetBuildError::CargoConfig { path: observed, .. }) if observed == path
        ));
    }
}

#[test]
fn compiler_configuration_decodes_strings_without_discarding_literal_comments() {
    let path = Path::new("config.toml");
    assert_eq!(
        named_rustc(
            "[build] # selection\nrustc = \"C:\\\\tools\\\\rust#c.exe\" # explanation\n",
            path,
        )
        .expect("valid compiler configuration"),
        Some(PathBuf::from(r"C:\tools\rust#c.exe")),
    );
    assert_eq!(
        named_rustc("\"build.rustc\" = \"/not-a-build-setting\"\n", path)
            .expect("an unrelated quoted key is valid TOML"),
        None,
    );
}

/// A config that exists and cannot be opened refuses rather than answering.
///
/// The unreadable half of the law the shapes above cover from the other side.
/// A file Cargo would open and this one cannot is not an absent file, and
/// answering as though it were absent hands back the `PATH` compiler for a
/// build Cargo was never going to start. The body names a compiler, so a
/// resolver that opened the file successfully would answer with it rather than
/// refuse, and the mode is put back before the assertions run so a failing
/// assertion still leaves a readable tree to clean up.
#[cfg(unix)]
#[test]
fn a_cargo_config_that_cannot_be_opened_refuses_the_resolution() {
    use std::os::unix::fs::PermissionsExt as _;

    let tree = ConfigTree::new();
    let body = "[build]\nrustc = \"/named/by/config\"\n";
    let written = tree.config("", "config.toml", body);
    fs::set_permissions(&written, fs::Permissions::from_mode(0o000)).expect("deny the config");
    let environment = resolving_environment(tree.path_with_rustc(), None);

    let refused = environment.rustc(&tree.build_dir("app"));
    fs::set_permissions(&written, fs::Permissions::from_mode(0o644)).expect("restore the config");

    let error = refused.expect_err("a config that cannot be opened is refused");
    let refusal = error.to_string();
    assert!(
        refusal.contains(&written.display().to_string()),
        "the refusal never names the file it could not open: {refusal}",
    );
    assert!(
        refusal.contains("Permission denied"),
        "the refusal never says why the file could not be read: {refusal}",
    );
}

/// Both spellings resolve, and the ordinary company they keep is read through.
///
/// Every body here puts something above the key that a reader guarding itself
/// too broadly would stop on, so stopping costs the answer instead of passing
/// unnoticed. `rustc-wrapper` names what wraps the compiler rather than which
/// compiler runs and is exactly what this machine's own
/// `~/.cargo/config.toml` sets; a multi-line `rustflags` list is the other
/// everyday shape, and it is the reason lines without a `=` are walked through
/// rather than refused. Cargo reads all three without complaint, and a release
/// tool that refused them would refuse every stage on a developer's machine.
#[cfg(unix)]
#[test]
fn both_spellings_resolve_through_the_company_a_config_keeps() {
    for (label, body) in [
        (
            "under a header",
            "[build]\nrustc-wrapper = \"/sccache\"\nrustc = \"/named/by/config\"\n",
        ),
        (
            "as dotted keys",
            "build.rustc-wrapper = \"/sccache\"\nbuild.rustc = \"/named/by/config\"\n",
        ),
        (
            "below a multi-line list",
            concat!(
                "[build]\nrustflags = [\n  \"-C\",\n  \"target-cpu=native\",\n]\n",
                "rustc = \"/named/by/config\"\n"
            ),
        ),
    ] {
        let tree = ConfigTree::new();
        tree.config("", "config.toml", body);
        let environment = resolving_environment(tree.path_with_rustc(), None);

        assert_eq!(
            environment
                .rustc(&tree.build_dir("app"))
                .unwrap_or_else(|error| panic!("the config written {label} is readable: {error}")),
            Some(PathBuf::from("/named/by/config")),
            "the config written {label} did not decide the compiler",
        );
    }
}

/// Either compiler override outranks every config file.
///
/// Both spellings are driven against the same config, because a resolver that
/// asked the files before the environment would answer identically for the one
/// it still happened to ask first.
#[cfg(unix)]
#[test]
fn a_compiler_override_outranks_every_cargo_config() {
    for (label, rustc, build_rustc) in [
        ("RUSTC", Some(OsString::from("/by-rustc")), None),
        ("CARGO_BUILD_RUSTC", None, Some(OsString::from("/by-rustc"))),
    ] {
        let tree = ConfigTree::new();
        tree.config("", "config.toml", "[build]\nrustc = \"/by-config\"\n");
        let environment = ReleaseEnvironment {
            path: Some(tree.path_with_rustc()),
            cargo_home: Some(
                tree.cargo_home("config.toml", "[build]\nrustc = \"/by-cargo-home\"\n"),
            ),
            rustc,
            build_rustc,
            ..ReleaseEnvironment::default()
        };

        assert_eq!(
            environment
                .rustc(&tree.build_dir("app"))
                .expect("an override resolves without reading a config"),
            Some(PathBuf::from("/by-rustc")),
            "{label} did not outrank the config files",
        );
    }
}
