use super::*;

const IDENTITY: &str = "Developer ID Application: Example (TEAM123456)";

/// Every labeled step the Developer ID signing path runs.
pub(super) const STEPS: [&str; 8] = [
    "developer-id-identity",
    "notary-profile",
    "developer-id-sign",
    "developer-id-verify",
    "notary-archive",
    "notary-submit",
    "notary-log",
    "gatekeeper-assess",
];

impl FakeExecutor {
    /// Answer one signing step; signing rewrites the named executable so the
    /// signed bytes differ from every earlier processing step's.
    pub(super) fn signing_step(&self, command: &CommandSpec, refused: bool) -> Vec<u8> {
        match command.label {
            "developer-id-sign" if !refused => {
                fs::write(
                    PathBuf::from(command.arguments.last().expect("signed executable")),
                    linked(self.target, &self.archive, b"signed"),
                )
                .expect("sign exact stripped executable fixture");
            }
            "developer-id-identity" if self.keychain_holds_identity.get() => {
                return format!("  1) ABCDEF \"{IDENTITY}\"\n     1 valid identities found\n")
                    .into_bytes();
            }
            "developer-id-identity" => return b"     0 valid identities found\n".to_vec(),
            "notary-submit" => {
                return b"{\"id\":\"submission-1\",\"status\":\"Accepted\",\"message\":\"ok\"}\n"
                    .to_vec();
            }
            _ => {}
        }
        format!("{} complete\n", command.label).into_bytes()
    }
}

fn stage_signed(
    fixture: &Fixture,
    executor: &FakeExecutor,
) -> Result<TargetBuild, TargetBuildError> {
    let signing = DarwinSigning::new(
        dist::DeveloperIdentity::new(IDENTITY).expect("fixture identity"),
        dist::NotaryProfile::new("release").expect("fixture profile"),
    );
    let archive = SourceArchive::read(
        &fixture
            .root
            .join("dist/stage")
            .join(dist::SOURCE_ARCHIVE_FILE),
    )
    .expect("fixture source archive reads");
    stage_with_roots(
        &fixture.root,
        &fixture.root.join("target"),
        &fixture.root.join("dist/stage"),
        StageRequest {
            target: ReleaseTarget::Aarch64AppleDarwin,
            version: &ProductVersion::parse(FIXTURE_VERSION).expect("fixture version"),
            signing: Some(&signing),
            archive: &archive,
            proof_container: FIXTURE_PROOF_IMAGE,
            canvas: None,
        },
        &fixture.programs(),
        executor,
    )
}

#[test]
fn a_signed_darwin_stage_seals_the_notarized_bytes_it_leaves_on_disk() {
    let fixture = Fixture::new();
    let executor = FakeExecutor::new(&fixture, ReleaseTarget::Aarch64AppleDarwin);

    let build = stage_signed(&fixture, &executor).expect("signed Darwin stage succeeds");

    let labels = executor.labels();
    let position = |label| {
        labels
            .iter()
            .position(|candidate| *candidate == label)
            .unwrap_or_else(|| panic!("missing {label} in {labels:?}"))
    };
    assert!(position("notary-profile") < position("app-build"));
    assert!(position("native-strip") < position("developer-id-sign"));
    assert!(position("developer-id-sign") < position("notary-submit"));
    assert!(position("gatekeeper-assess") < position("canvas-version"));
    let signed = linked(
        ReleaseTarget::Aarch64AppleDarwin,
        &fixture.archive,
        b"signed",
    );
    assert_eq!(
        fs::read(&build.executable).expect("staged executable reads"),
        signed
    );
    assert_eq!(
        build.staged.payload.native,
        Digest::compute(Domain::DistArtifact, &signed)
    );
    assert_symbol_result(&fixture, &fixture.programs(), &build);
}

#[test]
fn an_identity_missing_from_the_keychain_refuses_before_any_build() {
    let fixture = Fixture::new();
    let executor = FakeExecutor::new(&fixture, ReleaseTarget::Aarch64AppleDarwin);
    executor.keychain_holds_identity.set(false);

    let error = stage_signed(&fixture, &executor).expect_err("missing identity refuses");

    assert!(matches!(error, TargetBuildError::Signing(_)), "{error:?}");
    assert!(!executor.labels().contains(&"app-build"));
    assert!(!fixture.pointer(ReleaseTarget::Aarch64AppleDarwin).exists());
}
