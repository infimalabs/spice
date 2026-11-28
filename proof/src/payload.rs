//! Admission of one canonical distribution target before proof execution.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use wire::manifest::{InventoryEntry, ProofImage, TargetPayload};
use wire::{Digest, Domain};

const PAYLOAD_VERSION: u16 = wire::manifest::TARGET_PAYLOAD_VERSION;

/// The last release that wrote each retired payload schema. No retired
/// payload can be migrated: each predecessor lacks identities only its own
/// build observed.
const RETIRED_PAYLOADS: &[(u16, &str)] = &[];

/// Only the schema version, read before the current shape is assumed.
#[derive(serde::Deserialize)]
struct Versioned {
    version: u16,
}

/// A canonical, internally coherent target payload admitted for proof work.
///
/// Construction is private so runner and evidence APIs can require this type
/// without retaining another payload-validation path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedPayload {
    value: TargetPayload,
    digest: Digest,
}

impl AdmittedPayload {
    /// The exact typed payload admitted from canonical bytes.
    #[must_use]
    pub const fn value(&self) -> &TargetPayload {
        &self.value
    }

    /// Domain-tagged identity of the exact admitted payload bytes.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// Admit one target payload before any sealed runner or receipt publisher.
///
/// # Errors
///
/// Refuses malformed or noncanonical bytes, a non-current schema (naming the
/// release that last wrote it), an unpinned proof environment, inconsistent
/// subordinate digests, or an inventory outside the canonical executable,
/// compact symbol recipe, and optional target debug artifact.
pub fn admit(bytes: &[u8]) -> Result<AdmittedPayload, PayloadError> {
    let Versioned { version } =
        wire::decode(bytes).map_err(|source| PayloadError::Decode(source.to_string()))?;
    if version != PAYLOAD_VERSION {
        let release = RETIRED_PAYLOADS
            .iter()
            .find_map(|&(retired, release)| (retired == version).then_some(release));
        return Err(PayloadError::Version { version, release });
    }
    let payload: TargetPayload =
        wire::decode(bytes).map_err(|source| PayloadError::Decode(source.to_string()))?;
    let canonical =
        wire::encode(&payload).map_err(|source| PayloadError::Encode(source.to_string()))?;
    if canonical != bytes {
        return Err(PayloadError::NonCanonical);
    }
    if !ProofImage::admits(&payload.pins.proof_container) {
        return Err(PayloadError::UnpinnedEnvironment(
            payload.pins.proof_container.clone(),
        ));
    }

    let pins =
        wire::encode(&payload.pins).map_err(|source| PayloadError::Encode(source.to_string()))?;
    if payload.pins_digest != Digest::compute(Domain::DistPins, &pins) {
        return Err(PayloadError::PinsDigest);
    }
    let inventory = wire::encode(&payload.inventory)
        .map_err(|source| PayloadError::Encode(source.to_string()))?;
    if payload.inventory_digest != Digest::compute(Domain::DistInventory, &inventory) {
        return Err(PayloadError::InventoryDigest);
    }

    verify_symbol_metadata(&payload)?;
    verify_inventory(&payload)?;

    Ok(AdmittedPayload {
        value: payload,
        digest: Digest::compute(Domain::DistTargetPayload, &canonical),
    })
}

fn verify_symbol_metadata(payload: &TargetPayload) -> Result<(), PayloadError> {
    let symbols = match (payload.target, payload.symbols.as_ref()) {
        (wire::manifest::ReleaseTarget::Aarch64AppleDarwin, Some(symbols)) => symbols,
        (wire::manifest::ReleaseTarget::Aarch64AppleDarwin, None)
        | (
            wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl
            | wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc,
            None,
        ) => return Ok(()),
        (wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl, Some(_)) => {
            return Err(PayloadError::SymbolMetadata("present for Linux"));
        }
        (wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc, Some(_)) => {
            return Err(PayloadError::SymbolMetadata("present for Windows"));
        }
    };
    let entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == symbols.path)
        .ok_or(PayloadError::SymbolMetadata("absent from inventory"))?;
    if symbols.path != "symbols/spice.symbols"
        || symbols.mode != 0o644
        || !symbols.uuid_is_canonical()
        || symbols.executable != payload.native
        || (entry.mode, entry.bytes, &entry.digest)
            != (symbols.mode, symbols.bytes, &symbols.digest)
    {
        return Err(PayloadError::SymbolMetadata(
            "disagrees with executable or inventory",
        ));
    }
    Ok(())
}

fn verify_inventory(payload: &TargetPayload) -> Result<(), PayloadError> {
    let artifacts = expected_artifacts(payload);
    let mut paths = BTreeSet::new();
    for entry in &payload.inventory {
        if !paths.insert(entry.path.as_str()) {
            return Err(PayloadError::DuplicateArtifact(entry.path.clone()));
        }
    }

    for artifact in &artifacts {
        if !paths.contains(artifact.path.as_str()) {
            return Err(PayloadError::MissingArtifact(artifact.path.clone()));
        }
    }
    if let Some(entry) = payload
        .inventory
        .iter()
        .find(|entry| !artifacts.iter().any(|artifact| artifact.path == entry.path))
    {
        return Err(PayloadError::ExtraArtifact(entry.path.clone()));
    }

    for (index, (entry, artifact)) in payload.inventory.iter().zip(&artifacts).enumerate() {
        if entry.path != artifact.path {
            return Err(PayloadError::ReorderedArtifact {
                index,
                expected: artifact.path.clone(),
                observed: entry.path.clone(),
            });
        }
        verify_artifact(payload, entry, artifact)?;
    }
    Ok(())
}

fn verify_artifact(
    payload: &TargetPayload,
    entry: &InventoryEntry,
    artifact: &Artifact,
) -> Result<(), PayloadError> {
    if entry.mode != artifact.mode {
        return Err(PayloadError::ArtifactMode {
            path: artifact.path.clone(),
            expected: artifact.mode,
            observed: entry.mode,
        });
    }
    if entry.bytes == 0 {
        return Err(PayloadError::EmptyArtifact(artifact.path.clone()));
    }
    if artifact
        .digest
        .is_some_and(|field| entry.digest != *field.from(payload))
    {
        return Err(PayloadError::ArtifactDigest(artifact.path.clone()));
    }
    Ok(())
}

fn expected_artifacts(payload: &TargetPayload) -> Vec<Artifact> {
    let mut artifacts = vec![Artifact {
        path: payload.target.executable_path().to_owned(),
        mode: 0o755,
        digest: Some(DigestField::Native),
    }];
    artifacts.push(Artifact {
        path: "symbols/recipe.json".to_owned(),
        mode: 0o644,
        digest: None,
    });
    let debug_path = match payload.target {
        wire::manifest::ReleaseTarget::Aarch64AppleDarwin => "symbols/spice.symbols",
        wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl => "symbols/spice.debug",
        wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc => "symbols/spice.pdb",
    };
    if payload
        .inventory
        .iter()
        .any(|entry| entry.path == debug_path)
    {
        artifacts.push(Artifact {
            path: debug_path.to_owned(),
            mode: 0o644,
            digest: (payload.target == wire::manifest::ReleaseTarget::Aarch64AppleDarwin)
                .then_some(DigestField::Symbols),
        });
    }
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    artifacts
}

struct Artifact {
    path: String,
    mode: u32,
    digest: Option<DigestField>,
}

#[derive(Clone, Copy)]
enum DigestField {
    Native,
    Symbols,
}

impl DigestField {
    const fn from(self, payload: &TargetPayload) -> &Digest {
        match self {
            Self::Native => &payload.native,
            Self::Symbols => {
                &payload
                    .symbols
                    .as_ref()
                    .expect("Darwin expected inventory requires symbol metadata")
                    .digest
            }
        }
    }
}

/// Why target payload bytes were not admitted for proof work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PayloadError {
    /// Bytes do not decode as the current target payload shape.
    Decode(String),
    /// The decoded payload could not be encoded canonically.
    Encode(String),
    /// Bytes decode but differ from their canonical encoding.
    NonCanonical,
    /// The payload schema is not current; `release` last wrote a retired one.
    Version {
        version: u16,
        release: Option<&'static str>,
    },
    /// The proof environment is not pinned by an OCI digest.
    UnpinnedEnvironment(String),
    /// The declared pins digest disagrees with the pins document.
    PinsDigest,
    /// The declared inventory digest disagrees with the inventory document.
    InventoryDigest,
    /// Platform-conditional symbol metadata is absent or inconsistent.
    SymbolMetadata(&'static str),
    /// One canonical artifact is absent.
    MissingArtifact(String),
    /// An artifact outside the canonical inventory is present.
    ExtraArtifact(String),
    /// One artifact path occurs more than once.
    DuplicateArtifact(String),
    /// The canonical artifacts are not in strict order.
    ReorderedArtifact {
        /// Zero-based inventory position.
        index: usize,
        /// Canonical path required at that position.
        expected: String,
        /// Path observed at that position.
        observed: String,
    },
    /// One canonical artifact has the wrong published mode.
    ArtifactMode {
        /// Canonical artifact path.
        path: String,
        /// Required mode.
        expected: u32,
        /// Declared mode.
        observed: u32,
    },
    /// One inventory digest disagrees with the payload field it names.
    ArtifactDigest(String),
    /// A canonical artifact declares no bytes.
    EmptyArtifact(String),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(source) => write!(formatter, "target payload does not decode: {source}"),
            Self::Encode(source) => write!(formatter, "target payload does not encode: {source}"),
            Self::NonCanonical => formatter.write_str("target payload bytes are not canonical"),
            Self::Version {
                version,
                release: Some(release),
            } => write!(
                formatter,
                "target payload version {version} was last written by release {release} and is not current"
            ),
            Self::Version {
                version,
                release: None,
            } => write!(formatter, "target payload version {version} is not current"),
            Self::UnpinnedEnvironment(environment) => write!(
                formatter,
                "target payload proof environment is not digest-pinned: {environment:?}"
            ),
            Self::PinsDigest => {
                formatter.write_str("target payload pin digest disagrees with its pins")
            }
            Self::InventoryDigest => {
                formatter.write_str("target payload inventory digest disagrees with its inventory")
            }
            Self::SymbolMetadata(detail) => {
                write!(formatter, "target payload symbol metadata is {detail}")
            }
            Self::MissingArtifact(path) => {
                write!(formatter, "target payload inventory is missing {path}")
            }
            Self::ExtraArtifact(path) => {
                write!(
                    formatter,
                    "target payload inventory has extra artifact {path:?}"
                )
            }
            Self::DuplicateArtifact(path) => {
                write!(
                    formatter,
                    "target payload inventory repeats artifact {path:?}"
                )
            }
            Self::ReorderedArtifact {
                index,
                expected,
                observed,
            } => write!(
                formatter,
                "target payload artifact {index} is {observed:?}, expected {expected:?}"
            ),
            Self::ArtifactMode {
                path,
                expected,
                observed,
            } => write!(
                formatter,
                "target payload artifact {path} has mode {observed:04o}, expected {expected:04o}"
            ),
            Self::ArtifactDigest(path) => write!(
                formatter,
                "target payload artifact {path} disagrees with its named digest"
            ),
            Self::EmptyArtifact(path) => {
                write!(formatter, "target payload artifact {path} is empty")
            }
        }
    }
}

impl Error for PayloadError {}

#[cfg(test)]
mod tests {
    use wire::manifest::{BuildPins, InventoryEntry, ReleaseTarget, TargetPayload};
    use wire::{Digest, Domain};

    use super::{PAYLOAD_VERSION, PayloadError, admit};

    #[test]
    fn canonical_payload_admits_with_its_exact_domain_identity() {
        let payload = payload();
        let bytes = encode(&payload);

        let admitted = admit(&bytes).expect("canonical payload admits");

        assert_eq!(admitted.value(), &payload);
        assert_eq!(
            admitted.digest(),
            &Digest::compute(Domain::DistTargetPayload, &bytes)
        );
        assert_eq!(wire::encode(admitted.value()).expect("re-encode"), bytes);
    }

    #[test]
    fn malformed_noncanonical_stale_and_unpinned_payloads_refuse_before_admission() {
        let mut runner_started = false;
        let mut publisher_reached = false;
        let malformed = admit(b"not-json").map(|_| {
            runner_started = true;
            publisher_reached = true;
        });
        assert!(matches!(malformed, Err(PayloadError::Decode(_))));
        assert!(!runner_started);
        assert!(!publisher_reached);

        let mut noncanonical = encode(&payload());
        noncanonical.push(b'\n');
        assert_refuses_before_proof(&noncanonical, PayloadError::NonCanonical);

        let mut stale = payload();
        stale.version = PAYLOAD_VERSION + 1;
        assert_refuses_before_proof(
            &encode(&stale),
            PayloadError::Version {
                version: PAYLOAD_VERSION + 1,
                release: None,
            },
        );

        let mut unpinned = payload();
        unpinned.pins.proof_container = "registry.example/proof:latest".to_owned();
        assert_refuses_before_proof(
            &encode(&unpinned),
            PayloadError::UnpinnedEnvironment("registry.example/proof:latest".to_owned()),
        );
    }

    #[test]
    fn subordinate_pin_and_inventory_identities_are_recomputed() {
        let mut wrong_pins = payload();
        wrong_pins.pins.rust = "rustc another".to_owned();
        assert_refuses_before_proof(&encode(&wrong_pins), PayloadError::PinsDigest);

        let mut wrong_inventory = payload();
        wrong_inventory.inventory[0].bytes += 1;
        assert_refuses_before_proof(&encode(&wrong_inventory), PayloadError::InventoryDigest);
    }

    #[test]
    fn missing_extra_duplicate_and_reordered_inventory_refuse_apart() {
        let mut missing = payload();
        missing.inventory.remove(0);
        refresh_inventory(&mut missing);
        assert_refuses_before_proof(
            &encode(&missing),
            PayloadError::MissingArtifact("bin/spice".to_owned()),
        );
        let mut missing_recipe = payload();
        missing_recipe.inventory.remove(1);
        refresh_inventory(&mut missing_recipe);
        assert_refuses_before_proof(
            &encode(&missing_recipe),
            PayloadError::MissingArtifact("symbols/recipe.json".to_owned()),
        );

        let mut extra = payload();
        extra.inventory.push(InventoryEntry {
            path: "app/app.html".to_owned(),
            mode: 0o644,
            bytes: 0,
            digest: Digest::compute(Domain::DistArtifact, b"extra"),
        });
        refresh_inventory(&mut extra);
        assert_refuses_before_proof(
            &encode(&extra),
            PayloadError::ExtraArtifact("app/app.html".to_owned()),
        );

        let mut duplicate = payload();
        duplicate.inventory.push(duplicate.inventory[0].clone());
        refresh_inventory(&mut duplicate);
        assert_refuses_before_proof(
            &encode(&duplicate),
            PayloadError::DuplicateArtifact("bin/spice".to_owned()),
        );

        let mut reordered = darwin();
        reordered.inventory.swap(0, 1);
        refresh_inventory(&mut reordered);
        assert_refuses_before_proof(
            &encode(&reordered),
            PayloadError::ReorderedArtifact {
                index: 0,
                expected: "bin/spice".to_owned(),
                observed: "symbols/recipe.json".to_owned(),
            },
        );
    }

    #[test]
    fn every_canonical_path_has_one_mode_and_the_native_digest_is_named() {
        for (index, entry) in payload().inventory.into_iter().enumerate() {
            let mut wrong_mode = payload();
            wrong_mode.inventory[index].mode = 0;
            refresh_inventory(&mut wrong_mode);
            assert_refuses_before_proof(
                &encode(&wrong_mode),
                PayloadError::ArtifactMode {
                    path: entry.path,
                    expected: entry.mode,
                    observed: 0,
                },
            );
        }
        let mut wrong_native = payload();
        wrong_native.native = Digest::compute(Domain::DistArtifact, b"other native");
        assert_refuses_before_proof(
            &encode(&wrong_native),
            PayloadError::ArtifactDigest("bin/spice".to_owned()),
        );

        let mut darwin_without_symbols = payload();
        darwin_without_symbols.target = ReleaseTarget::Aarch64AppleDarwin;
        admit(&encode(&darwin_without_symbols))
            .expect("canonical Darwin payload admits without detached symbols");
        let mut darwin = self::darwin();
        admit(&encode(&darwin)).expect("Darwin symbol metadata admits");
        darwin.symbols.as_mut().expect("Darwin symbols").bytes += 1;
        assert_refuses_before_proof(
            &encode(&darwin),
            PayloadError::SymbolMetadata("disagrees with executable or inventory"),
        );
    }

    #[test]
    fn windows_payload_admits_without_a_pdb_and_validates_one_when_present() {
        let windows = windows();
        admit(&encode(&windows)).expect("canonical Windows payload admits");

        let mut without_pdb = windows.clone();
        without_pdb
            .inventory
            .retain(|entry| entry.path != "symbols/spice.pdb");
        refresh_inventory(&mut without_pdb);
        admit(&encode(&without_pdb)).expect("canonical publication carries no PDB");

        let mut empty_pdb = windows;
        let pdb = empty_pdb
            .inventory
            .iter_mut()
            .find(|entry| entry.path == "symbols/spice.pdb")
            .expect("Windows PDB entry");
        pdb.bytes = 0;
        refresh_inventory(&mut empty_pdb);
        assert_refuses_before_proof(
            &encode(&empty_pdb),
            PayloadError::EmptyArtifact("symbols/spice.pdb".to_owned()),
        );
    }

    fn darwin() -> TargetPayload {
        let mut darwin = payload();
        darwin.target = ReleaseTarget::Aarch64AppleDarwin;
        darwin.symbols =
            crate::test_support::symbols(darwin.target, &darwin.native, &mut darwin.inventory);
        darwin
            .inventory
            .sort_by(|left, right| left.path.cmp(&right.path));
        refresh_inventory(&mut darwin);
        darwin
    }

    fn windows() -> TargetPayload {
        let mut windows = payload();
        windows.target = ReleaseTarget::X86_64PcWindowsMsvc;
        windows.inventory[0].path = windows.target.executable_path().to_owned();
        crate::test_support::symbols(windows.target, &windows.native, &mut windows.inventory);
        windows
            .inventory
            .sort_by(|left, right| left.path.cmp(&right.path));
        refresh_inventory(&mut windows);
        windows
    }

    fn payload() -> TargetPayload {
        let pins = BuildPins {
            rust: "rustc 1.97.1".to_owned(),
            wasm_target: "wasm32-unknown-unknown".to_owned(),
            trunk: "trunk 0.22.1".to_owned(),
            proof_container: format!("registry.example/proof@sha256:{}", "4".repeat(64)),
        };
        let app = Digest::compute(Domain::DistArtifact, b"app generation");
        let native = Digest::compute(Domain::DistArtifact, b"native");
        let mut inventory = vec![InventoryEntry {
            path: "bin/spice".to_owned(),
            mode: 0o755,
            bytes: 6,
            digest: native.clone(),
        }];
        crate::test_support::symbol_recipe(&mut inventory);
        TargetPayload {
            version: wire::manifest::TARGET_PAYLOAD_VERSION,
            target: ReleaseTarget::X86_64UnknownLinuxMusl,
            source: Digest::compute(Domain::DistArtifact, b"source"),
            archive: Digest::compute(Domain::DistSourceArchive, b"source archive"),
            app,
            canvas: Digest::compute(Domain::DistArtifact, b"canvas archive"),
            native,
            symbols: None,
            pins_digest: Digest::compute(
                Domain::DistPins,
                &wire::encode(&pins).expect("pins encode"),
            ),
            inventory_digest: Digest::compute(
                Domain::DistInventory,
                &wire::encode(&inventory).expect("inventory encodes"),
            ),
            pins,
            inventory,
        }
    }

    fn encode(payload: &TargetPayload) -> Vec<u8> {
        wire::encode(payload).expect("payload encodes")
    }

    fn refresh_inventory(payload: &mut TargetPayload) {
        payload.inventory_digest = Digest::compute(
            Domain::DistInventory,
            &wire::encode(&payload.inventory).expect("inventory encodes"),
        );
    }

    fn assert_refuses_before_proof(bytes: &[u8], expected: PayloadError) {
        let mut runner_started = false;
        let mut publisher_reached = false;
        let refused = admit(bytes).map(|_| {
            runner_started = true;
            publisher_reached = true;
        });

        assert_eq!(refused, Err(expected));
        assert!(!runner_started, "a refused payload reached the runner");
        assert!(
            !publisher_reached,
            "a refused payload reached receipt publication"
        );
    }
}
