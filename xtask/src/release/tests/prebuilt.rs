use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use super::super::prebuilt::{
    Bundle, CANDIDATE, DEBUG, PrebuiltRequest, TOOLCHAIN, Toolchain, stage_prebuilt,
};
use super::*;

const TARGET: ReleaseTarget = ReleaseTarget::X86_64PcWindowsMsvc;

/// A bundle root under the fixture carrying `candidate`, the fixture PDB,
/// and `toolchain`, with the manifest digests of exactly those bytes.
fn bundle(fixture: &Fixture, candidate: &[u8], toolchain: &Toolchain) -> Bundle {
    let root = fixture.root.join("bundle");
    let toolchain = serde_json::to_vec(toolchain).expect("toolchain record encodes");
    let mut files = BTreeMap::new();
    for (path, bytes) in [
        (CANDIDATE, candidate),
        (DEBUG, b"fixture PDB".as_slice()),
        (TOOLCHAIN, &toolchain),
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().expect("bundle file parent")).expect("bundle directory");
        fs::write(&file, bytes).expect("write bundle file");
        files.insert(path.to_owned(), format!("{:x}", Sha256::digest(bytes)));
    }
    Bundle::new(&root, files)
}

fn pinned_toolchain() -> Toolchain {
    let pins = required_pins(FIXTURE_PROOF_IMAGE);
    Toolchain {
        rust: pins.rust,
        wasm_target: pins.wasm_target,
        trunk: pins.trunk,
    }
}

fn stage(
    fixture: &Fixture,
    executor: &FakeExecutor,
    bundle: &Bundle,
) -> Result<TargetBuild, TargetBuildError> {
    let stage_root = fixture.root.join("dist/stage");
    let archive = SourceArchive::read(&stage_root.join(dist::SOURCE_ARCHIVE_FILE))
        .expect("fixture source archive");
    stage_prebuilt(
        &fixture.root,
        &stage_root,
        PrebuiltRequest {
            version: &ProductVersion::parse(FIXTURE_VERSION).expect("fixture version"),
            archive: &archive,
            proof_container: FIXTURE_PROOF_IMAGE,
            canvas: staged_canvas(&fixture.root).expect("fixture canvas is staged"),
            bundle,
        },
        executor,
    )
}

/// Every published stage file, relative to the stage root.
fn published(fixture: &Fixture) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(directory).expect("stage directory reads") {
            let path = entry.expect("stage entry reads").path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let relative = path.strip_prefix(root).expect("under stage root");
                files.push((
                    relative.to_path_buf(),
                    fs::read(&path).expect("stage file reads"),
                ));
            }
        }
    }
    let root = fixture.root.join("dist/stage");
    let mut files = Vec::new();
    for published in ["objects", "targets"] {
        walk(&root, &root.join(published), &mut files);
    }
    files.sort();
    files
}

#[test]
fn a_prebuilt_candidate_publishes_the_built_stage_without_compiling() {
    let built_fixture = Fixture::new();
    let built = stage_with(
        &built_fixture.root,
        &built_fixture.root.join("target"),
        TARGET,
        &built_fixture.programs(),
        &FakeExecutor::new(&built_fixture, TARGET),
    )
    .expect("built stage succeeds");

    let fixture = Fixture::new();
    let executor = FakeExecutor::new(&fixture, TARGET);
    let bundle = bundle(
        &fixture,
        &linked(TARGET, &fixture.archive, b"linked"),
        &pinned_toolchain(),
    );
    let prebuilt = stage(&fixture, &executor, &bundle).expect("prebuilt stage succeeds");

    let commands = executor.records();
    assert_eq!(executor.labels(), ["canvas-version"]);
    assert_eq!(
        commands[0].program,
        fixture.root.join("bundle").join(CANDIDATE)
    );
    assert_eq!(prebuilt.staged.payload_bytes, built.staged.payload_bytes);
    assert_eq!(published(&fixture), published(&built_fixture));
}

#[test]
fn bytes_the_manifest_does_not_name_or_a_failing_check_refuse_the_stage() {
    let fixture = Fixture::new();
    let executor = FakeExecutor::new(&fixture, TARGET);
    let candidate = linked(TARGET, &fixture.archive, b"linked");
    let bundle = bundle(&fixture, &candidate, &pinned_toolchain());
    fs::write(
        fixture.root.join("bundle").join(CANDIDATE),
        linked(TARGET, &fixture.archive, b"substituted"),
    )
    .expect("substitute candidate bytes");
    let error = stage(&fixture, &executor, &bundle).expect_err("substituted bytes refuse");
    assert!(matches!(error, TargetBuildError::Prebuilt(_)), "{error:?}");
    assert!(executor.labels().is_empty());
    assert!(!fixture.pointer(TARGET).exists());

    let drifted = Toolchain {
        trunk: "trunk 0.0.0".to_owned(),
        ..pinned_toolchain()
    };
    let bundle = self::bundle(&fixture, &candidate, &drifted);
    let error = stage(&fixture, &executor, &bundle).expect_err("a drifted toolchain refuses");
    assert!(matches!(error, TargetBuildError::Pins(_)), "{error:?}");
    assert!(executor.labels().is_empty());

    let bundle = self::bundle(&fixture, &candidate, &pinned_toolchain());
    executor.refusing.set(Some("canvas-version"));
    let error = stage(&fixture, &executor, &bundle).expect_err("a failing version check refuses");
    assert!(matches!(error, TargetBuildError::Stage(_)), "{error:?}");
    assert!(!fixture.pointer(TARGET).exists());
}
