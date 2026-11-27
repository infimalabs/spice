use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wire::manifest::{
    BuildPins, HostedWindowsIdentity, HostedWindowsImage, HostedWindowsToolchain, InventoryEntry,
    ProofEnvironment, ProofReceipt, ReleaseManifest, ReleaseTarget, TargetPayload,
};
use wire::{Digest, Domain};

use atomic::{RootedFiles, RootedPublish};

use super::{
    MANIFEST_POINTER, ManifestAdmissionError, ManifestError, ManifestInput, admit_manifest,
    object_path, publish_manifest,
};
use crate::{Battery, CheckKind, CheckStanding, EvidenceError, ReplayVerdict};

const OLD_POINTER: &[u8] = b"old release manifest";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Prepared {
    payload: Vec<u8>,
    receipt: Vec<u8>,
    replay: ReplayVerdict,
}

#[derive(Default)]
struct MemoryPublisher {
    files: BTreeMap<PathBuf, (u32, Vec<u8>)>,
    writes: Vec<PathBuf>,
    fail_at: Option<usize>,
}

impl RootedPublish for MemoryPublisher {
    fn publish(&mut self, path: &Path, mode: u32, bytes: &[u8]) -> Result<(), atomic::Error> {
        let attempt = self.writes.len();
        self.writes.push(path.to_path_buf());
        if self.fail_at == Some(attempt) {
            return Err(atomic::Error::Io {
                operation: atomic::Operation::Replace,
                path: path.to_path_buf(),
                source: io::Error::other("fixture interruption"),
                cleanup: None,
            });
        }
        self.files
            .insert(path.to_path_buf(), (mode, bytes.to_vec()));
        Ok(())
    }
}

#[test]
fn all_verified_targets_form_one_stable_manifest_and_pointer_last() {
    let source = digest(Domain::DistArtifact, b"source");
    let darwin = prepared(ReleaseTarget::Aarch64AppleDarwin, source.clone(), true);
    let linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, source.clone(), true);
    let windows = prepared(ReleaseTarget::X86_64PcWindowsMsvc, source.clone(), true);
    let mut first = MemoryPublisher::default();
    let published = publish_manifest(inputs(&darwin, &linux, &windows), &mut first)
        .expect("all clean proofs publish");
    let mut second = MemoryPublisher::default();
    let repeated = publish_manifest(inputs(&darwin, &linux, &windows), &mut second)
        .expect("the same proofs publish identically");

    assert_eq!(published, repeated);
    assert_eq!(first.files, second.files);
    assert_eq!(published.manifest.source, source);
    assert_eq!(published.manifest.targets.len(), 3);
    for (target, prepared) in published
        .manifest
        .targets
        .iter()
        .zip([&darwin, &linux, &windows])
    {
        let payload: TargetPayload =
            wire::decode(&prepared.payload).expect("fixture payload decodes");
        assert_eq!(target.payload_bytes, prepared.payload.len() as u64);
        assert_eq!(target.inventory, payload.inventory);
        assert_eq!(target.symbols, payload.symbols);
        assert_eq!(target.receipt_bytes, prepared.receipt.len() as u64);
    }
    assert_eq!(
        published
            .manifest
            .targets
            .iter()
            .map(|entry| entry.target)
            .collect::<Vec<_>>(),
        [
            ReleaseTarget::Aarch64AppleDarwin,
            ReleaseTarget::X86_64UnknownLinuxMusl,
            ReleaseTarget::X86_64PcWindowsMsvc,
        ]
    );
    assert_eq!(
        published.digest,
        Digest::compute(Domain::DistReleaseManifest, &published.bytes)
    );
    let decoded: ReleaseManifest =
        wire::decode(&published.bytes).expect("manifest bytes are canonical wire");
    assert_eq!(decoded, published.manifest);
    assert_eq!(first.writes.last(), Some(&published.pointer));
    assert_eq!(published.pointer, PathBuf::from(MANIFEST_POINTER));
    assert!(
        first.writes[..first.writes.len() - 1]
            .iter()
            .all(|path| path.starts_with("objects")),
        "every immutable object precedes the pointer"
    );
}

#[test]
fn missing_extra_duplicate_and_reordered_targets_refuse_before_publication() {
    let source = digest(Domain::DistArtifact, b"source");
    let darwin = prepared(ReleaseTarget::Aarch64AppleDarwin, source.clone(), true);
    let linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, source.clone(), true);
    let windows = prepared(ReleaseTarget::X86_64PcWindowsMsvc, source, true);

    let cases = [
        (
            vec![input(&darwin), input(&linux)],
            "missing",
            matches_error as fn(&ManifestError) -> bool,
        ),
        (
            vec![
                input(&darwin),
                input(&linux),
                input(&windows),
                input(&darwin),
            ],
            "extra",
            matches_error,
        ),
        (
            vec![input(&darwin), input(&linux), input(&linux)],
            "duplicate",
            duplicate_error,
        ),
        (
            vec![input(&darwin), input(&windows), input(&linux)],
            "reordered",
            order_error,
        ),
    ];

    for (inputs, label, expected) in cases {
        let mut publisher = MemoryPublisher::default();
        let error =
            publish_manifest(inputs, &mut publisher).expect_err("invalid target set must refuse");
        assert!(expected(&error), "{label} produced {error:?}");
        assert!(publisher.writes.is_empty(), "{label} wrote before refusal");
    }
}

fn matches_error(error: &ManifestError) -> bool {
    matches!(error, ManifestError::EntryCount(_))
}

fn duplicate_error(error: &ManifestError) -> bool {
    matches!(error, ManifestError::DuplicateTarget(_))
}

fn order_error(error: &ManifestError) -> bool {
    matches!(error, ManifestError::TargetOrder { .. })
}

#[test]
fn mixed_sources_mismatched_evidence_and_non_clean_proofs_refuse_without_writes() {
    let first_source = digest(Domain::DistArtifact, b"first source");
    let other_source = digest(Domain::DistArtifact, b"other source");
    let darwin = prepared(
        ReleaseTarget::Aarch64AppleDarwin,
        first_source.clone(),
        true,
    );
    let linux = prepared(
        ReleaseTarget::X86_64UnknownLinuxMusl,
        first_source.clone(),
        true,
    );
    let windows = prepared(
        ReleaseTarget::X86_64PcWindowsMsvc,
        first_source.clone(),
        true,
    );
    let other_linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, other_source, true);
    let other_archive_linux = reprepare(&linux, |payload| {
        payload.archive = digest(Domain::DistSourceArchive, b"other source archive");
    });
    let drifted_linux = reprepare(&linux, |payload| {
        payload.pins.trunk.push_str("-drifted");
        payload.pins_digest = digest(
            Domain::DistPins,
            &wire::encode(&payload.pins).expect("drifted pins encode"),
        );
    });
    let refused_linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, first_source, false);

    let mut publisher = MemoryPublisher::default();
    let mixed = publish_manifest(inputs(&darwin, &other_linux, &windows), &mut publisher)
        .expect_err("mixed sources refuse");
    assert!(matches!(mixed, ManifestError::SourceMismatch { .. }));
    assert!(publisher.writes.is_empty());

    let mut publisher = MemoryPublisher::default();
    let mixed = publish_manifest(
        inputs(&darwin, &other_archive_linux, &windows),
        &mut publisher,
    )
    .expect_err("targets staged from two source archives refuse");
    assert!(
        matches!(
            mixed,
            ManifestError::Mixed {
                field: "source archive",
                ..
            }
        ),
        "{mixed:?}"
    );
    assert!(publisher.writes.is_empty());

    let mut publisher = MemoryPublisher::default();
    let mixed = publish_manifest(inputs(&darwin, &drifted_linux, &windows), &mut publisher)
        .expect_err("mixed toolchain pins refuse");
    assert!(matches!(mixed, ManifestError::Mixed { .. }));
    assert!(publisher.writes.is_empty());

    let mismatched = ManifestInput::new(&darwin.payload, &linux.receipt);
    let mut publisher = MemoryPublisher::default();
    let mismatch = publish_manifest([mismatched, input(&linux), input(&windows)], &mut publisher)
        .expect_err("cross-target evidence refuses");
    assert!(matches!(mismatch, ManifestError::Evidence { .. }));
    assert!(publisher.writes.is_empty());

    let mut publisher = MemoryPublisher::default();
    let refused = publish_manifest(inputs(&darwin, &refused_linux, &windows), &mut publisher)
        .expect_err("non-clean proof refuses");
    assert!(matches!(
        refused,
        ManifestError::Evidence {
            source: EvidenceError::Refused,
            ..
        }
    ));
    assert!(publisher.writes.is_empty());
}

#[test]
fn every_receipt_binding_is_rechecked_before_the_first_write() {
    let source = digest(Domain::DistArtifact, b"source");
    let darwin = prepared(ReleaseTarget::Aarch64AppleDarwin, source.clone(), true);
    let linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, source.clone(), true);
    let windows = prepared(ReleaseTarget::X86_64PcWindowsMsvc, source, true);
    for field in 0..4 {
        let mut receipt: ProofReceipt =
            wire::decode(&linux.receipt).expect("fixture receipt decodes");
        match field {
            0 => receipt.target = ReleaseTarget::Aarch64AppleDarwin,
            1 => receipt.payload = digest(Domain::DistTargetPayload, b"other payload"),
            2 => receipt.battery = digest(Domain::ProofBattery, b"other battery"),
            3 => {
                receipt.environment = ProofEnvironment::native(digest(
                    Domain::ProofNativeRunner,
                    b"wrong environment",
                ));
            }
            _ => unreachable!("the field sweep is closed"),
        }
        let receipt = wire::encode(&receipt).expect("tampered receipt remains canonical");
        let tampered = ManifestInput::new(&linux.payload, &receipt);
        let mut publisher = MemoryPublisher::default();

        let error = publish_manifest([input(&darwin), tampered, input(&windows)], &mut publisher)
            .expect_err("every receipt binding is load-bearing");

        assert!(matches!(error, ManifestError::Evidence { .. }));
        assert!(
            publisher.writes.is_empty(),
            "receipt field {field} wrote before refusal"
        );
    }
}

#[test]
fn interrupted_object_publication_preserves_the_previous_pointer() {
    let source = digest(Domain::DistArtifact, b"source");
    let darwin = prepared(ReleaseTarget::Aarch64AppleDarwin, source.clone(), true);
    let linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, source.clone(), true);
    let windows = prepared(ReleaseTarget::X86_64PcWindowsMsvc, source, true);
    let pointer = PathBuf::from(MANIFEST_POINTER);
    let mut publisher = MemoryPublisher {
        files: BTreeMap::from([(pointer.clone(), (0o644, OLD_POINTER.to_vec()))]),
        writes: Vec::new(),
        fail_at: Some(2),
    };

    let error = publish_manifest(inputs(&darwin, &linux, &windows), &mut publisher)
        .expect_err("interrupted object write fails");

    assert!(matches!(error, ManifestError::Publish { .. }));
    assert_eq!(publisher.files[&pointer].1, OLD_POINTER);
    assert!(!publisher.writes.contains(&pointer));
}

#[test]
fn filesystem_publication_commits_exact_objects_and_pointer() {
    let scratch = Scratch::new();
    let source = digest(Domain::DistArtifact, b"source");
    let darwin = prepared(ReleaseTarget::Aarch64AppleDarwin, source.clone(), true);
    let linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, source.clone(), true);
    let windows = prepared(ReleaseTarget::X86_64PcWindowsMsvc, source, true);
    let mut publisher = RootedFiles::new(&scratch.root);

    let published = publish_manifest(inputs(&darwin, &linux, &windows), &mut publisher)
        .expect("filesystem publication succeeds");
    let darwin_payload: TargetPayload =
        wire::decode(&darwin.payload).expect("Darwin payload decodes");
    let admitted = admit_manifest(
        &scratch.root,
        &darwin_payload.app,
        ReleaseTarget::Aarch64AppleDarwin,
        &darwin_payload.native,
    )
    .expect("the complete published object graph admits");

    assert_eq!(admitted, published);
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let linked = scratch.root.join("linked-stage");
        symlink(&scratch.root, &linked).expect("stage symlink fixture");
        assert!(matches!(
            admit_manifest(
                &linked,
                &darwin_payload.app,
                ReleaseTarget::Aarch64AppleDarwin,
                &darwin_payload.native,
            ),
            Err(ManifestAdmissionError::StageSymlink(_))
        ));
    }
    assert_eq!(
        fs::read(scratch.root.join(&published.pointer)).expect("pointer is readable"),
        published.bytes
    );
    let objects = fs::read_dir(scratch.root.join("objects"))
        .expect("objects directory is readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("every object entry is readable");
    assert_eq!(
        objects.len(),
        7,
        "three payloads, three receipts, one manifest"
    );
    assert!(
        objects
            .iter()
            .all(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            fs::metadata(scratch.root.join(&published.pointer))
                .expect("pointer metadata is readable")
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }
}

#[test]
fn manifest_admission_refuses_missing_stale_and_mixed_objects() {
    let source = digest(Domain::DistArtifact, b"source");
    let darwin = prepared(ReleaseTarget::Aarch64AppleDarwin, source.clone(), true);
    let linux = prepared(ReleaseTarget::X86_64UnknownLinuxMusl, source.clone(), true);
    let windows = prepared(ReleaseTarget::X86_64PcWindowsMsvc, source, true);
    let expected: TargetPayload = wire::decode(&darwin.payload).expect("payload decodes");

    let missing = Scratch::new();
    let mut publisher = RootedFiles::new(&missing.root);
    let published = publish_manifest(inputs(&darwin, &linux, &windows), &mut publisher)
        .expect("fixture publishes");
    fs::remove_file(missing.root.join(object_path(
        "receipt",
        &published.manifest.targets[1].receipt,
    )))
    .expect("remove receipt object");
    assert!(matches!(
        admit_manifest(
            &missing.root,
            &expected.app,
            expected.target,
            &expected.native,
        ),
        Err(ManifestAdmissionError::Input { .. })
    ));

    let stale = Scratch::new();
    let mut publisher = RootedFiles::new(&stale.root);
    publish_manifest(inputs(&darwin, &linux, &windows), &mut publisher).expect("fixture publishes");
    let mut pointer: ReleaseManifest =
        wire::decode(&fs::read(stale.root.join(MANIFEST_POINTER)).expect("read pointer"))
            .expect("pointer decodes");
    pointer.version += 1;
    fs::write(
        stale.root.join(MANIFEST_POINTER),
        wire::encode(&pointer).expect("stale pointer encodes"),
    )
    .expect("write stale pointer");
    assert!(matches!(
        admit_manifest(
            &stale.root,
            &expected.app,
            expected.target,
            &expected.native,
        ),
        Err(ManifestAdmissionError::ManifestVersion(_))
    ));

    let divergent = Scratch::new();
    let mut publisher = RootedFiles::new(&divergent.root);
    let published = publish_manifest(inputs(&darwin, &linux, &windows), &mut publisher)
        .expect("fixture publishes");
    let mut manifest = published.manifest;
    manifest.targets[0].inventory[0].bytes += 1;
    let bytes = wire::encode(&manifest).expect("divergent manifest encodes");
    let identity = Digest::compute(Domain::DistReleaseManifest, &bytes);
    fs::write(
        divergent.root.join(object_path("manifest", &identity)),
        &bytes,
    )
    .expect("write divergent manifest object");
    fs::write(divergent.root.join(MANIFEST_POINTER), bytes)
        .expect("write divergent manifest pointer");
    assert!(matches!(
        admit_manifest(
            &divergent.root,
            &expected.app,
            expected.target,
            &expected.native,
        ),
        Err(ManifestAdmissionError::Binding {
            field: "payload inventory",
            ..
        })
    ));

    let mixed = Scratch::new();
    let mut publisher = RootedFiles::new(&mixed.root);
    let published = publish_manifest(inputs(&darwin, &linux, &windows), &mut publisher)
        .expect("fixture publishes");
    fs::write(
        mixed.root.join(object_path(
            "payload",
            &published.manifest.targets[1].payload,
        )),
        &darwin.payload,
    )
    .expect("replace Linux object with Darwin bytes");
    assert!(matches!(
        admit_manifest(
            &mixed.root,
            &expected.app,
            expected.target,
            &expected.native,
        ),
        Err(ManifestAdmissionError::Binding { .. })
    ));
}

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-proof-manifest-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("unique scratch root is created");
        Self { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("scratch root is removed");
    }
}

fn inputs<'a>(
    darwin: &'a Prepared,
    linux: &'a Prepared,
    windows: &'a Prepared,
) -> [ManifestInput<'a>; 3] {
    [input(darwin), input(linux), input(windows)]
}

fn input(prepared: &Prepared) -> ManifestInput<'_> {
    ManifestInput::new(&prepared.payload, &prepared.receipt)
}

fn prepared(target: ReleaseTarget, source: Digest, clean: bool) -> Prepared {
    let app = digest(Domain::DistArtifact, b"app");
    let native = digest(Domain::DistArtifact, target.as_str().as_bytes());
    let pins = BuildPins {
        rust: "rustc 1.97.1".to_owned(),
        wasm_target: "wasm32-unknown-unknown".to_owned(),
        trunk: "trunk 0.22.1".to_owned(),
        proof_container: format!("registry.example/spice-proof@sha256:{}", "0".repeat(64)),
    };
    let mut inventory = vec![InventoryEntry {
        path: target.executable_path().to_owned(),
        mode: 0o755,
        bytes: target.as_str().len() as u64,
        digest: native.clone(),
    }];
    let symbols = crate::test_support::symbols(target, &native, &mut inventory);
    inventory.sort_by(|left, right| left.path.cmp(&right.path));
    let payload = TargetPayload {
        version: wire::manifest::TARGET_PAYLOAD_VERSION,
        target,
        source,
        archive: digest(Domain::DistSourceArchive, b"source archive"),
        app,
        canvas: digest(Domain::DistArtifact, b"canvas archive"),
        native,
        symbols,
        pins_digest: digest(Domain::DistPins, &wire::encode(&pins).expect("pins encode")),
        inventory_digest: digest(
            Domain::DistInventory,
            &wire::encode(&inventory).expect("inventory encodes"),
        ),
        pins,
        inventory,
    };
    let payload_bytes = wire::encode(&payload).expect("payload encodes");
    let payload_digest = digest(Domain::DistTargetPayload, &payload_bytes);
    let environment = match target {
        ReleaseTarget::Aarch64AppleDarwin => {
            ProofEnvironment::native(digest(Domain::ProofNativeRunner, b"native runner"))
        }
        ReleaseTarget::X86_64UnknownLinuxMusl => {
            ProofEnvironment::oci(&payload.pins.proof_container)
                .expect("fixture OCI image is pinned")
        }
        ReleaseTarget::X86_64PcWindowsMsvc => {
            ProofEnvironment::hosted_windows(HostedWindowsIdentity {
                provider: "github-actions".to_owned(),
                repository: "infimalabs/spice".to_owned(),
                workflow: ".github/workflows/windows.yml".to_owned(),
                run_id: 17,
                run_attempt: 1,
                image: HostedWindowsImage {
                    label: "windows-2025".to_owned(),
                    provider_os: "win25".to_owned(),
                    version: "20261013.288.2".to_owned(),
                    os_version: "10.0.26100.33438".to_owned(),
                    architecture: "X64".to_owned(),
                },
                runner: digest(Domain::ProofNativeRunner, b"hosted runner"),
                source: "1".repeat(64),
                toolchain: HostedWindowsToolchain {
                    rust: "rustc 1.97.1".to_owned(),
                    cargo: "cargo 1.97.1".to_owned(),
                    git: "git version 2.51.0.windows.2".to_owned(),
                    trunk: "trunk 0.22.1".to_owned(),
                    msvc: "19.44.35207".to_owned(),
                    sdk: "10.0.26100.0".to_owned(),
                    rustc_path: r"C:\rustc.exe".to_owned(),
                    cargo_path: r"C:\cargo.exe".to_owned(),
                    git_path: r"C:\git.exe".to_owned(),
                    trunk_path: r"C:\trunk.exe".to_owned(),
                    cl_path: r"C:\cl.exe".to_owned(),
                    rc_path: r"C:\rc.exe".to_owned(),
                    link_path: r"C:\link.exe".to_owned(),
                    path: r"C:\tools".to_owned(),
                    include: r"C:\include".to_owned(),
                    lib: r"C:\lib".to_owned(),
                },
                executables: Vec::new(),
            })
        }
    };
    let battery = Battery::current().digest().clone();
    let replay = if clean {
        ReplayVerdict::fixture_clean(target, environment, battery)
    } else {
        let mut standings = [CheckStanding::Clean; CheckKind::ALL.len()];
        standings[0] = CheckStanding::Failed { code: Some(1) };
        ReplayVerdict::fixture_with(target, environment, battery, standings)
    };
    let receipt = replay
        .receipt(&payload, payload_digest)
        .expect("matching replay makes a receipt");
    Prepared {
        payload: payload_bytes,
        receipt: wire::encode(&receipt).expect("receipt encodes"),
        replay,
    }
}

/// `prepared` with its payload rewritten by `change` and its receipt re-bound
/// to the rewritten bytes.
fn reprepare(prepared: &Prepared, change: impl FnOnce(&mut TargetPayload)) -> Prepared {
    let mut payload: TargetPayload =
        wire::decode(&prepared.payload).expect("fixture payload decodes");
    change(&mut payload);
    let payload = wire::encode(&payload).expect("rewritten payload encodes");
    let receipt = prepared
        .replay
        .receipt(
            &wire::decode(&payload).expect("rewritten payload decodes"),
            digest(Domain::DistTargetPayload, &payload),
        )
        .expect("rewritten payload remains target matched");
    Prepared {
        payload,
        receipt: wire::encode(&receipt).expect("rewritten receipt encodes"),
        replay: prepared.replay.clone(),
    }
}

fn digest(domain: Domain, bytes: &[u8]) -> Digest {
    Digest::compute(domain, bytes)
}
