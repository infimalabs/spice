use super::*;

#[test]
fn a_named_artifact_replaced_after_admission_refuses() {
    let fixture = Fixture::new();
    let artifact = fixture.root.join("artifact");
    let replacement = fixture.root.join("replacement");
    fs::write(&artifact, b"admitted bytes").expect("write admitted artifact");
    fs::write(&replacement, b"replacement bytes").expect("write replacement artifact");

    let error = read_regular_with(&artifact, || {
        fs::remove_file(&artifact).expect("unlink admitted artifact name");
        fs::rename(&replacement, &artifact).expect("replace admitted artifact name");
    })
    .expect_err("a replaced artifact name refuses");

    assert!(matches!(
        error,
        TargetBuildError::Artifact(path) if path == artifact
    ));
}
