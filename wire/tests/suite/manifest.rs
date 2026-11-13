use std::cmp::Ordering;

use wire::manifest::{
    BuildPins, InventoryEntry, JOURNAL_BACKUP_VERSION, JournalBackupManifest, JournalBackupMember,
    PROOF_RECEIPT_VERSION, ProductVersion, ProofEnvironment, ProofReceipt, ProofStanding,
    RELEASE_MANIFEST_VERSION, ReleaseManifest, ReleaseManifestTarget, ReleaseTarget, TargetPayload,
};
use wire::{Digest, Domain, WireVersion};

/// Hex width of an OCI `sha256:` reference.
///
/// This is a foreign registry's spelling, not WIRE's. It is unrelated to the
/// BLAKE3 digests [`Digest`] mints and only happens to be the same width.
const OCI_SHA256_HEX_LEN: usize = 64;

/// Mint a fixture digest the way production mints one.
///
/// Fixtures compute rather than spell a digest out, so a fixture can never
/// carry a spelling that [`Digest::compute`] would not produce.
fn digest(marker: &str) -> Digest {
    Digest::compute(Domain::DistArtifact, marker.as_bytes())
}

#[test]
fn journal_backup_manifests_have_one_strict_current_wire_shape() {
    let member_bytes = b"captured journal authority";
    let manifest = JournalBackupManifest {
        version: JOURNAL_BACKUP_VERSION,
        wire: WireVersion::CURRENT,
        journal_schema: 1,
        authority: "spicejournal.sqlite3".to_owned(),
        members: vec![JournalBackupMember {
            path: "spicejournal.sqlite3".to_owned(),
            mode: 0o440,
            bytes: u64::try_from(member_bytes.len()).expect("fixture length crosses"),
            digest: Digest::compute(Domain::JournalBackupMember, member_bytes),
        }],
    };
    let encoded = wire::encode(&manifest).expect("backup manifest encodes");
    assert_eq!(
        wire::decode::<JournalBackupManifest>(&encoded).expect("backup manifest decodes"),
        manifest
    );

    let original: serde_json::Value =
        serde_json::from_slice(&encoded).expect("backup manifest is JSON");
    for path in [Vec::new(), vec!["members", "0"]] {
        let mut changed = original.clone();
        let mut object = &mut changed;
        for &segment in &path {
            object = if let Ok(index) = segment.parse::<usize>() {
                &mut object.as_array_mut().expect("fixture segment is an array")[index]
            } else {
                object
                    .as_object_mut()
                    .expect("fixture segment is an object")
                    .get_mut(segment)
                    .expect("fixture segment exists")
            };
        }
        object
            .as_object_mut()
            .expect("fixture target is an object")
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));
        assert!(
            wire::decode::<JournalBackupManifest>(
                &serde_json::to_vec(&changed).expect("changed manifest is JSON")
            )
            .is_err(),
            "unknown field at {path:?} must refuse"
        );
    }
}

fn payload(target: ReleaseTarget, marker: &str) -> TargetPayload {
    let artifact = digest(marker);
    TargetPayload {
        version: wire::manifest::TARGET_PAYLOAD_VERSION,
        target,
        source: digest("source"),
        archive: digest("archive"),
        app: artifact.clone(),
        canvas: digest("canvas"),
        native: digest("native"),
        symbols: None,
        pins: BuildPins {
            rust: "rustc 1.97.1".to_owned(),
            wasm_target: "wasm32-unknown-unknown".to_owned(),
            trunk: "trunk 0.22.1".to_owned(),
            proof_container: format!(
                "registry.example/proof@sha256:{}",
                "4".repeat(OCI_SHA256_HEX_LEN)
            ),
        },
        pins_digest: Digest::compute(Domain::DistPins, marker.as_bytes()),
        inventory: vec![InventoryEntry {
            path: "bin/spice".to_owned(),
            mode: 0o755,
            bytes: 12,
            digest: artifact,
        }],
        inventory_digest: Digest::compute(Domain::DistInventory, marker.as_bytes()),
    }
}

fn receipt(target: ReleaseTarget, marker: &str) -> ProofReceipt {
    ProofReceipt {
        version: PROOF_RECEIPT_VERSION,
        target,
        payload: Digest::compute(Domain::DistTargetPayload, marker.as_bytes()),
        battery: Digest::compute(Domain::ProofBattery, marker.as_bytes()),
        replay: Digest::compute(Domain::ProofReplay, marker.as_bytes()),
        environment: ProofEnvironment::oci(format!(
            "registry.example/proof@sha256:{}",
            "4".repeat(64)
        ))
        .expect("fixture image is pinned"),
        standing: ProofStanding::Clean,
    }
}

#[test]
fn proof_environments_have_one_strict_canonical_wire_shape() {
    let oci = ProofEnvironment::oci(format!("registry.example/proof@sha256:{}", "4".repeat(64)))
        .expect("canonical OCI identity");
    let native = ProofEnvironment::native(Digest::compute(Domain::ProofNativeRunner, b"runner"));

    assert_eq!(
        wire::encode(&oci).expect("OCI identity encodes"),
        format!(
            r#"{{"oci":{{"image":"registry.example/proof@sha256:{}"}}}}"#,
            "4".repeat(64)
        )
        .as_bytes()
    );
    assert_eq!(
        wire::decode::<ProofEnvironment>(&wire::encode(&native).expect("native identity encodes"))
            .expect("native identity decodes"),
        native
    );

    let digest = "4".repeat(64);
    let refused = [
        format!(r#"{{"container":{{"image":"sha256:{digest}"}}}}"#),
        r#"{"oci":{"image":"proof:latest"}}"#.to_owned(),
        format!(r#"{{"oci":{{"image":"Registry/proof@sha256:{digest}"}}}}"#),
        format!(r#"{{"oci":{{"image":"registry.example/proof:latest@sha256:{digest}"}}}}"#),
        format!(r#"{{"oci":{{"image":"registry.example//proof@sha256:{digest}"}}}}"#),
        format!(r#"{{"oci":{{"image":"registry.example/proof..alias@sha256:{digest}"}}}}"#),
        format!(r#"{{"oci":{{"image":"sha256:{}"}}}}"#, "A".repeat(64)),
        format!(r#"{{"oci":{{"image":"sha256:{digest}","unknown":true}}}}"#),
        format!(r#"{{"native":{{"runner":"{digest}","unknown":true}}}}"#),
    ];
    for refused in refused {
        assert!(
            wire::decode::<ProofEnvironment>(refused.as_bytes()).is_err(),
            "noncanonical environment must refuse: {refused}"
        );
    }
}

#[test]
fn release_payloads_round_trip_without_aliasing_targets() {
    let darwin = payload(ReleaseTarget::Aarch64AppleDarwin, "darwin");
    let linux = payload(ReleaseTarget::X86_64UnknownLinuxMusl, "linux");
    let darwin_bytes = wire::encode(&darwin).expect("darwin payload encodes");
    let linux_bytes = wire::encode(&linux).expect("linux payload encodes");

    assert_eq!(
        wire::decode::<TargetPayload>(&darwin_bytes).expect("darwin payload decodes"),
        darwin
    );
    assert_eq!(
        wire::decode::<TargetPayload>(&linux_bytes).expect("linux payload decodes"),
        linux
    );
    assert_ne!(darwin_bytes, linux_bytes);
}

#[test]
fn release_targets_encode_as_canonical_rust_triples() {
    assert_eq!(
        wire::encode(&ReleaseTarget::Aarch64AppleDarwin).expect("target encodes"),
        br#""aarch64-apple-darwin""#
    );
    assert_eq!(
        wire::encode(&ReleaseTarget::X86_64UnknownLinuxMusl).expect("target encodes"),
        br#""x86_64-unknown-linux-musl""#
    );
}

#[test]
fn product_versions_have_one_canonical_semantic_spelling() {
    for accepted in ["0.0.0", "1.2.3-rc.1", "1.2.3+build.01"] {
        let version = ProductVersion::parse(accepted).expect("canonical version");
        let bytes = wire::encode(&version).expect("version encodes");
        assert_eq!(
            wire::decode::<ProductVersion>(&bytes).expect("version decodes"),
            version
        );
    }
    for refused in ["", "v1.2.3", "01.2.3", "1.2", "1.2.3-01", "1.2.3+"] {
        assert!(ProductVersion::parse(refused).is_err(), "{refused}");
    }
}

#[test]
fn product_version_precedence_follows_semver_and_ignores_build_metadata() {
    let ascending = [
        "0.2.0-dev.0",
        "0.2.0-dev.0.dTYp0bz3B",
        "0.2.0-dev.0.dTYpFZV7w",
        "1.0.0-1",
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-rc.2",
        "1.0.0-rc.10",
        "1.0.0",
        "1.9.9",
        "1.10.0",
    ]
    .map(|text| ProductVersion::parse(text).expect("canonical version"));
    for (lower, pair) in ascending.iter().zip(ascending.iter().skip(1)) {
        assert_eq!(lower.precedence(pair), Ordering::Less, "{lower} < {pair}");
        assert_eq!(
            pair.precedence(lower),
            Ordering::Greater,
            "{pair} > {lower}"
        );
    }
    let old = ProductVersion::parse("1.2.3-dev.0+bin.old").expect("canonical version");
    let new = ProductVersion::parse("1.2.3-dev.0+bin.new").expect("canonical version");
    assert_ne!(old, new);
    assert_eq!(old.precedence(&new), Ordering::Equal);
}

#[test]
fn detached_proof_receipts_round_trip_in_the_current_shape() {
    let receipt = receipt(ReleaseTarget::X86_64UnknownLinuxMusl, "linux-proof");
    let bytes = wire::encode(&receipt).expect("proof receipt encodes");

    assert_eq!(
        wire::decode::<ProofReceipt>(&bytes).expect("proof receipt decodes"),
        receipt
    );

    let mut changed: serde_json::Value = serde_json::from_slice(&bytes).expect("receipt is JSON");
    changed["unknown"] = serde_json::Value::Bool(true);
    assert!(
        wire::decode::<ProofReceipt>(
            &serde_json::to_vec(&changed).expect("changed receipt is JSON")
        )
        .is_err(),
        "a receipt outside the current shape must refuse"
    );

    let mut retired: serde_json::Value = serde_json::from_slice(&bytes).expect("receipt is JSON");
    retired["version"] = serde_json::Value::from(1);
    let retired_environment = format!("registry.example/proof@sha256:{}", "4".repeat(64));
    retired["environment"] = serde_json::Value::String(retired_environment);
    assert!(
        wire::decode::<ProofReceipt>(
            &serde_json::to_vec(&retired).expect("retired receipt is JSON")
        )
        .is_err(),
        "the retired string-valued environment must not remain as an alternate schema"
    );
}

#[test]
fn release_manifests_round_trip_and_refuse_unknown_fields_at_every_level() {
    let darwin = receipt(ReleaseTarget::Aarch64AppleDarwin, "darwin-proof");
    let linux = receipt(ReleaseTarget::X86_64UnknownLinuxMusl, "linux-proof");
    let manifest = ReleaseManifest {
        version: RELEASE_MANIFEST_VERSION,
        source: digest("source"),
        archive: digest("archive"),
        targets: [darwin, linux]
            .into_iter()
            .map(|receipt| {
                let receipt_digest = Digest::compute(
                    Domain::ProofReceipt,
                    &wire::encode(&receipt).expect("receipt encodes"),
                );
                ReleaseManifestTarget {
                    target: receipt.target,
                    payload: receipt.payload,
                    payload_bytes: 1,
                    inventory: vec![InventoryEntry {
                        path: "bin/spice".to_owned(),
                        mode: 0o755,
                        bytes: 1,
                        digest: digest("release manifest executable"),
                    }],
                    symbols: None,
                    receipt: receipt_digest,
                    receipt_bytes: 1,
                    battery: receipt.battery,
                    replay: receipt.replay,
                    environment: receipt.environment,
                    standing: receipt.standing,
                }
            })
            .collect(),
    };
    let encoded = wire::encode(&manifest).expect("release manifest encodes");
    assert_eq!(
        wire::decode::<ReleaseManifest>(&encoded).expect("release manifest decodes"),
        manifest
    );
    let original: serde_json::Value =
        serde_json::from_slice(&encoded).expect("release manifest is JSON");

    for path in [
        Vec::new(),
        vec!["targets", "0"],
        vec!["targets", "0", "inventory", "0"],
    ] {
        let mut changed = original.clone();
        let mut object = &mut changed;
        for &segment in &path {
            object = if let Ok(index) = segment.parse::<usize>() {
                &mut object.as_array_mut().expect("fixture segment is an array")[index]
            } else {
                object
                    .as_object_mut()
                    .expect("fixture segment is an object")
                    .get_mut(segment)
                    .expect("fixture segment exists")
            };
        }
        object
            .as_object_mut()
            .expect("fixture target is an object")
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));

        assert!(
            wire::decode::<ReleaseManifest>(
                &serde_json::to_vec(&changed).expect("changed manifest is JSON")
            )
            .is_err(),
            "unknown field at {path:?} must refuse"
        );
    }
}

#[test]
fn payloads_refuse_unknown_fields_at_every_structured_level() {
    let encoded = wire::encode(&payload(ReleaseTarget::Aarch64AppleDarwin, "darwin"))
        .expect("payload encodes");
    let original: serde_json::Value = serde_json::from_slice(&encoded).expect("payload is JSON");

    for path in [Vec::new(), vec!["pins"], vec!["inventory", "0"]] {
        let mut changed = original.clone();
        let mut object = &mut changed;
        for &segment in &path {
            object = if let Ok(index) = segment.parse::<usize>() {
                &mut object.as_array_mut().expect("fixture segment is an array")[index]
            } else {
                object
                    .as_object_mut()
                    .expect("fixture segment is an object")
                    .get_mut(segment)
                    .expect("fixture segment exists")
            };
        }
        object
            .as_object_mut()
            .expect("fixture target is an object")
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));

        let bytes = serde_json::to_vec(&changed).expect("changed payload encodes");
        assert!(
            wire::decode::<TargetPayload>(&bytes).is_err(),
            "unknown field at {path:?} must refuse"
        );
    }
}
