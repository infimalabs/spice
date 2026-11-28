//! A non-Linux release stage links the staged Linux payload's canvas, and
//! refuses every canvas it cannot prove came from that payload and this tree.

use super::*;

const LINUX: ReleaseTarget = ReleaseTarget::X86_64UnknownLinuxMusl;
const DARWIN: ReleaseTarget = ReleaseTarget::Aarch64AppleDarwin;

fn archive(fixture: &Fixture) -> SourceArchive {
    SourceArchive::read(
        &fixture
            .root
            .join("dist/stage")
            .join(dist::SOURCE_ARCHIVE_FILE),
    )
    .expect("fixture source archive")
}

/// Stage the Linux target, whose executable links the fixture canvas.
fn stage_linux(fixture: &Fixture) -> TargetBuild {
    let executor = FakeExecutor::new(fixture, LINUX);
    stage_with(
        &fixture.root,
        &fixture.root.join("target"),
        LINUX,
        &fixture.programs(),
        &executor,
    )
    .expect("Linux target stages")
}

fn adopt(fixture: &Fixture) -> Result<crate::app::StagedCanvas, TargetBuildError> {
    let linux = adopted::linux_canvas(&fixture.root.join("dist/stage"), &archive(fixture).digest)?;
    crate::app::StagedCanvas::adopt(
        &fixture.root,
        &linux.bytes,
        &linux.generation,
        &fixture_build(),
    )
    .map_err(TargetBuildError::Application)
}

#[test]
fn darwin_links_the_staged_linux_canvas_without_compiling_the_app() {
    let fixture = Fixture::new();
    let linux = stage_linux(&fixture);
    let canvas = adopt(&fixture).expect("the staged Linux canvas is adopted");
    let executor = FakeExecutor::new(&fixture, DARWIN);
    let version = ProductVersion::parse(FIXTURE_VERSION).expect("fixture version is canonical");

    let darwin = stage_with_roots(
        &fixture.root,
        &fixture.root.join("target"),
        &fixture.root.join("dist/stage"),
        StageRequest {
            target: DARWIN,
            version: &version,
            signing: None,
            archive: &archive(&fixture),
            proof_container: FIXTURE_PROOF_IMAGE,
            canvas: Some(canvas),
            capture_symbols: true,
        },
        &fixture.programs(),
        &executor,
    )
    .expect("Darwin stages with the adopted canvas");

    assert!(!executor.labels().contains(&"app-build"));
    assert_eq!(darwin.staged.payload.app, linux.staged.payload.app);
    assert_eq!(darwin.staged.payload.canvas, linux.staged.payload.canvas);
}

#[test]
fn adoption_refuses_a_missing_or_foreign_linux_payload() {
    let fixture = Fixture::new();
    let stage_root = fixture.root.join("dist/stage");
    let digest = archive(&fixture).digest;
    assert!(matches!(
        adopted::linux_canvas(&stage_root, &digest),
        Err(TargetBuildError::AdoptedCanvas(_))
    ));

    stage_linux(&fixture);
    let other = Digest::compute(Domain::DistSourceArchive, MOVED_ARCHIVE);
    assert!(matches!(
        adopted::linux_canvas(&stage_root, &other),
        Err(TargetBuildError::AdoptedCanvas(_))
    ));
}

#[test]
fn adoption_refuses_an_executable_whose_rack_is_not_the_recorded_canvas() {
    let fixture = Fixture::new();
    let linux = stage_linux(&fixture);
    let entry = linux
        .staged
        .payload
        .inventory
        .iter()
        .find(|entry| entry.path == LINUX.executable_path())
        .expect("Linux payload carries its executable");
    let object = fixture
        .root
        .join("dist/stage")
        .join(format!("objects/{}-{:04o}", entry.digest, entry.mode));
    fs::write(&object, linked(LINUX, b"PK\x03\x04another rack", b"linked"))
        .expect("substitute the staged executable object");

    assert!(matches!(
        adopt(&fixture),
        Err(TargetBuildError::AdoptedCanvas(_))
    ));
}

#[test]
fn adoption_refuses_a_canvas_this_tree_does_not_reassemble() {
    let fixture = Fixture::new();
    stage_linux(&fixture);
    fs::write(fixture.root.join("docs/guide.md"), b"# another guide\n")
        .expect("change one tracked rack source");

    assert!(matches!(
        adopt(&fixture),
        Err(TargetBuildError::Application(
            crate::app::AppBuildError::GeneratedStage(_)
        ))
    ));
}
