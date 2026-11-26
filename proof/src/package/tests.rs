use wire::manifest::{
    BuildPins, InventoryEntry, PROOF_RECEIPT_VERSION, ProofEnvironment, ProofReceipt,
    ReleaseManifestTarget, TARGET_PAYLOAD_VERSION, TargetPayload,
};

use super::*;

struct Fixture {
    version: ProductVersion,
    notes_identity: Digest,
    notes: Vec<u8>,
    manifest: Vec<u8>,
    executables: Vec<Vec<u8>>,
    payloads: Vec<Vec<u8>>,
    proofs: Vec<Vec<u8>>,
}

impl Fixture {
    fn input(&self) -> PackageInput<'_> {
        PackageInput::new(
            &self.version,
            self.notes_identity.clone(),
            member(
                NOTES_PATH,
                DATA_MODE,
                &self.notes,
                Domain::DistReleaseNotesContent,
            ),
            member(
                MANIFEST_PATH,
                DATA_MODE,
                &self.manifest,
                Domain::DistReleaseManifest,
            ),
            TARGETS
                .into_iter()
                .enumerate()
                .map(|(index, target)| {
                    PackageTargetInput::new(
                        target,
                        member(
                            &executable_path(target),
                            EXECUTABLE_MODE,
                            &self.executables[index],
                            Domain::DistArtifact,
                        ),
                        member(
                            &payload_path(target),
                            DATA_MODE,
                            &self.payloads[index],
                            Domain::DistTargetPayload,
                        ),
                        member(
                            &proof_path(target),
                            DATA_MODE,
                            &self.proofs[index],
                            Domain::ProofReceipt,
                        ),
                    )
                    .with_debug(target.debug_path().map(|_| {
                        member(
                            &debug_path(target).expect("debug path exists"),
                            DATA_MODE,
                            debug_bytes(target),
                            Domain::DistArtifact,
                        )
                    }))
                })
                .collect(),
        )
    }
}

#[test]
fn selected_targets_require_their_index_digests_and_source_bound_clean_proof() {
    let fixture = fixture();
    let input = fixture.input();
    let package = build_package_index(input.clone()).expect("index builds");
    let document = admit_package_index(package.bytes()).expect("canonical index admits");
    for (index, target) in input.targets.iter().enumerate() {
        assert_eq!(admit_package_target(&document, target), Ok(()));
        for field in 0..3 {
            let mut changed = document.clone();
            let digest = digest(Domain::DistArtifact, b"another artifact");
            match field {
                0 => changed.targets[index].executable = digest,
                1 => changed.targets[index].payload = digest,
                _ => changed.targets[index].proof = digest,
            }
            assert!(matches!(
                admit_package_target(&changed, target),
                Err(PackageError::Binding { .. }),
            ));
        }
        let mut changed = document.clone();
        changed.source = digest(Domain::DistSourceCommit, b"another source");
        assert!(matches!(
            admit_package_target(&changed, target),
            Err(PackageError::Binding { .. }),
        ));
        let mut refused = target.clone();
        let mut receipt: wire::manifest::ProofReceipt =
            wire::decode(&fixture.proofs[index]).expect("receipt decodes");
        receipt.standing = ProofStanding::Refused;
        refused.proof = member(
            &proof_path(target.target),
            DATA_MODE,
            wire::encode(&receipt).expect("receipt encodes").as_slice(),
            Domain::ProofReceipt,
        );
        let mut changed = document.clone();
        changed.targets[index].proof = refused.proof.digest.clone();
        assert!(matches!(
            admit_package_target(&changed, &refused),
            Err(PackageError::Binding { .. }),
        ));
    }
}

#[test]
fn runtime_package_can_omit_bound_debug_artifacts() {
    let fixture = fixture();
    let mut input = fixture.input();
    for target in &mut input.targets {
        target.debug = None;
    }
    let package = build_package_index(input.clone()).expect("runtime index builds");
    assert!(
        package
            .document()
            .targets
            .iter()
            .all(|target| target.debug_path.is_none())
    );
    let document = admit_package_index(package.bytes()).expect("runtime index admits");
    for target in &input.targets {
        assert_eq!(admit_package_target(&document, target), Ok(()));
    }
}

#[test]
fn canonical_package_identity_matches_inline_and_file_backed_members() {
    let inline_fixture = fixture();
    let index = build_package_index(inline_fixture.input()).expect("fixture index builds");
    let tree = build_package(inline_fixture.input()).expect("fixture package builds");
    assert_eq!(index, *tree.package());
    assert!(
        tree.members()
            .is_sorted_by(|left, right| left.path() < right.path())
    );
    let fixture = fixture();
    let body = fixture.executables[0].clone();
    let path = std::env::temp_dir().join(format!(
        "proof-package-member-{}-executable",
        std::process::id()
    ));
    std::fs::write(&path, &body).expect("file member writes");
    let member = PackageMember::admit_file(
        executable_path(TARGETS[0]),
        EXECUTABLE_MODE,
        File::open(&path).expect("file member opens"),
        body.len() as u64,
    )
    .expect("file member admits");
    let mut input = fixture.input();
    input.targets[0].executable = member;
    let tree = build_package(input.clone()).expect("file-backed package builds");
    assert_eq!(*tree.package(), index);
    let mut moved = body;
    moved[0] ^= 1;
    std::fs::write(&path, moved).expect("file member moves");
    assert!(build_package_index(input.clone()).is_err());
    assert!(build_package(input).is_err());
    std::fs::remove_file(path).expect("file member removes");
    assert!(PackageMember::admit_inline("release/notes.md", DATA_MODE, b"x", 0).is_err());
    let oversized = vec![0; PACKAGE_DOCUMENT_LIMIT as usize + 1];
    assert!(
        PackageMember::admit_inline(
            "release/notes.md",
            DATA_MODE,
            &oversized,
            PACKAGE_DOCUMENT_LIMIT,
        )
        .is_err()
    );
}

#[test]
fn every_external_identity_input_moves_or_refuses_the_package() {
    let baseline_fixture = fixture();
    let baseline = build_package(baseline_fixture.input())
        .expect("baseline")
        .package
        .identity;

    let mut notes = fixture();
    notes.notes.push(b'!');
    assert_ne!(
        build_package(notes.input())
            .expect("notes move")
            .package
            .identity,
        baseline
    );

    let mut version = fixture();
    version.version = ProductVersion::parse("1.2.4").expect("version");
    assert_ne!(
        build_package(version.input())
            .expect("version moves")
            .package
            .identity,
        baseline
    );

    let mut executable = fixture();
    executable.executables[0].push(0);
    assert!(matches!(
        build_package(executable.input()),
        Err(PackageError::Binding {
            field: "payload executable",
            ..
        })
    ));
}

#[test]
fn debug_artifacts_must_match_their_target_payload() {
    for (index, body) in [
        (0, b"moved symbols".as_slice()),
        (2, b"moved Windows PDB".as_slice()),
    ] {
        let fixture = fixture();
        let mut input = fixture.input();
        input.targets[index].debug = Some(member(
            &debug_path(TARGETS[index]).expect("fixture target has debug"),
            DATA_MODE,
            body,
            Domain::DistArtifact,
        ));
        assert!(matches!(
            build_package(input),
            Err(PackageError::Binding {
                field: "package debug artifact",
                ..
            })
        ));
    }
}

#[test]
fn publication_requires_all_targets() {
    let fixture = fixture();
    let mut input = fixture.input();
    input.targets.pop();
    assert!(matches!(
        build_package(input),
        Err(PackageError::TargetCount(count)) if count == TARGETS.len() - 1
    ));
}

#[test]
fn publication_requires_authenticated_modes_for_every_member_kind() {
    let fixture = fixture();
    for notes in [false, true] {
        let mut input = fixture.input();
        let member = if notes {
            &mut input.notes
        } else {
            &mut input.manifest
        };
        member.mode = EXECUTABLE_MODE;
        assert!(matches!(build_package(input), Err(PackageError::Closure)));
    }

    for index in 0..TARGETS.len() {
        for field in 0..4 {
            let mut input = fixture.input();
            let target = &mut input.targets[index];
            let member = match field {
                0 => &mut target.executable,
                1 => &mut target.payload,
                2 => &mut target.proof,
                _ => {
                    let Some(debug) = &mut target.debug else {
                        continue;
                    };
                    debug
                }
            };
            member.mode = if field == 0 {
                DATA_MODE
            } else {
                EXECUTABLE_MODE
            };
            assert!(matches!(build_package(input), Err(PackageError::Closure)));
        }
    }
}

#[test]
fn noncanonical_or_moved_sealed_evidence_refuses_before_packaging() {
    let mut payload = fixture();
    payload.payloads[0].push(b'\n');
    assert_eq!(
        build_package(payload.input()),
        Err(PackageError::Payload(crate::PayloadError::NonCanonical))
    );

    let mut proof = fixture();
    let mut receipt: ProofReceipt = wire::decode(&proof.proofs[0]).expect("receipt");
    receipt.standing = ProofStanding::Refused;
    proof.proofs[0] = wire::encode(&receipt).expect("moved receipt");
    assert!(matches!(
        build_package(proof.input()),
        Err(PackageError::Binding { .. })
    ));
}

fn fixture() -> Fixture {
    let source = digest(Domain::DistSourceCommit, b"source");
    let archive = digest(Domain::DistSourceArchive, b"source archive");
    let app_identity = digest(Domain::DistArtifact, b"app generation");
    let canvas = digest(Domain::DistArtifact, b"canvas archive");
    let battery = digest(Domain::ProofBattery, b"battery");
    let environment = ProofEnvironment::native(digest(Domain::ProofNativeRunner, b"runner"));
    let mut executables = Vec::new();
    let mut payloads = Vec::new();
    let mut proofs = Vec::new();
    let mut manifest_targets = Vec::new();
    for (index, target) in TARGETS.into_iter().enumerate() {
        let executable = format!("executable-{index}").into_bytes();
        let native = Digest::compute(Domain::DistArtifact, &executable);
        let pins = BuildPins {
            rust: "rustc 1.97.1".to_owned(),
            wasm_target: "wasm32-unknown-unknown".to_owned(),
            trunk: "trunk 0.22.1".to_owned(),
            proof_container: format!("registry.example/proof@sha256:{}", "1".repeat(64)),
        };
        let mut inventory = vec![InventoryEntry {
            path: target.executable_path().to_owned(),
            mode: EXECUTABLE_MODE,
            bytes: executable.len() as u64,
            digest: native.clone(),
        }];
        let symbols = if target == ReleaseTarget::Aarch64AppleDarwin {
            crate::test_support::symbols(target, &native, &mut inventory)
        } else {
            None
        };
        if target == ReleaseTarget::X86_64PcWindowsMsvc {
            let debug = debug_bytes(target);
            inventory.push(InventoryEntry {
                path: target.debug_path().expect("Windows debug path").to_owned(),
                mode: DATA_MODE,
                bytes: debug.len() as u64,
                digest: Digest::compute(Domain::DistArtifact, debug),
            });
        }
        inventory.sort_by(|left, right| left.path.cmp(&right.path));
        let payload = TargetPayload {
            version: TARGET_PAYLOAD_VERSION,
            target,
            source: source.clone(),
            archive: archive.clone(),
            app: app_identity.clone(),
            canvas: canvas.clone(),
            native: native.clone(),
            symbols,
            pins_digest: Digest::compute(Domain::DistPins, &wire::encode(&pins).expect("pins")),
            inventory_digest: Digest::compute(
                Domain::DistInventory,
                &wire::encode(&inventory).expect("inventory"),
            ),
            pins,
            inventory,
        };
        let payload_bytes = wire::encode(&payload).expect("payload");
        let payload_identity = Digest::compute(Domain::DistTargetPayload, &payload_bytes);
        let replay = digest(Domain::ProofReplay, format!("replay-{index}").as_bytes());
        let proof = ProofReceipt {
            version: PROOF_RECEIPT_VERSION,
            target,
            payload: payload_identity.clone(),
            battery: battery.clone(),
            replay: replay.clone(),
            environment: environment.clone(),
            standing: ProofStanding::Clean,
        };
        let proof_bytes = wire::encode(&proof).expect("proof");
        let proof_identity = Digest::compute(Domain::ProofReceipt, &proof_bytes);
        manifest_targets.push(ReleaseManifestTarget {
            target,
            payload: payload_identity,
            payload_bytes: payload_bytes.len() as u64,
            inventory: payload.inventory.clone(),
            symbols: payload.symbols.clone(),
            receipt: proof_identity,
            receipt_bytes: proof_bytes.len() as u64,
            battery: battery.clone(),
            replay,
            environment: environment.clone(),
            standing: ProofStanding::Clean,
        });
        executables.push(executable);
        payloads.push(payload_bytes);
        proofs.push(proof_bytes);
    }
    let manifest = ReleaseManifest {
        version: RELEASE_MANIFEST_VERSION,
        source,
        archive,
        targets: manifest_targets,
    };
    Fixture {
        version: ProductVersion::parse("1.2.3").expect("version"),
        notes_identity: digest(Domain::DistReleaseNotes, b"notes evidence"),
        notes: b"Curated release notes\n".to_vec(),
        manifest: wire::encode(&manifest).expect("manifest"),
        executables,
        payloads,
        proofs,
    }
}

fn debug_bytes(target: ReleaseTarget) -> &'static [u8] {
    match target {
        ReleaseTarget::Aarch64AppleDarwin => b"symbol fixture",
        ReleaseTarget::X86_64PcWindowsMsvc => b"PDB fixture",
        ReleaseTarget::X86_64UnknownLinuxMusl => {
            panic!("Linux has no separate debug artifact")
        }
    }
}

fn digest(domain: Domain, bytes: &[u8]) -> Digest {
    Digest::compute(domain, bytes)
}
