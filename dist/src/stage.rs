//! Ordered builds and manifest-last atomic staging.

use std::path::PathBuf;

use atomic::RootedPublish;
use wire::manifest::{
    BuildPins, InventoryEntry, MacSymbolArtifact, MacSymbolMetadata, ProductVersion, ReleaseTarget,
    TARGET_PAYLOAD_VERSION, TargetPayload,
};
use wire::{Digest, Domain};

use crate::canvas::Canvas;
use crate::pins::{PinError, RequiredPins};

const DATA_MODE: u32 = 0o644;
const EXECUTABLE_MODE: u32 = 0o755;
const MAC_SYMBOL_PATH: &str = "symbols/spice.symbols";
const WINDOWS_DEBUG_PATH: &str = "symbols/spice.pdb";

/// Build work occurs in this fixed order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildStep {
    App,
    Native,
}

/// A bounded failure from a concrete build adapter.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct BuildError {
    message: String,
}

impl BuildError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Heavyweight build implementation supplied by the executable rim.
pub trait Builder {
    /// Build the app and admit its staged canvas archive as generation G.
    fn build_app(&mut self) -> Result<Canvas, BuildError>;
    /// Link `canvas` into the executable, then derive symbols, strip, and sign
    /// it, and answer the final bytes.
    fn build_native(
        &mut self,
        target: ReleaseTarget,
        canvas: &Canvas,
    ) -> Result<Vec<u8>, BuildError>;
    /// Run the final executable once and answer its version report.
    fn report_version(&mut self) -> Result<String, BuildError>;

    fn take_symbol_artifact(&mut self) -> Option<Vec<u8>> {
        None
    }
    /// Take the raw PDB produced beside a Windows release executable.
    fn take_windows_debug_artifact(&mut self) -> Option<Vec<u8>> {
        None
    }
}

/// One completed, receipt-free target stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedTarget {
    pub payload: TargetPayload,
    pub payload_bytes: Vec<u8>,
    pub payload_digest: Digest,
    pub payload_path: PathBuf,
    /// The canvas archive linked into the staged executable.
    pub canvas: Canvas,
}

/// One finalized signed Darwin stage, ready for manifest-last publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedDarwinStage {
    pub payload: TargetPayload,
    pub payload_bytes: Vec<u8>,
    pub payload_digest: Digest,
    pub payload_path: PathBuf,
    pub artifacts: Vec<FinalizedArtifact>,
}

/// One content-addressed artifact in a finalized Darwin stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedArtifact {
    pub path: String,
    pub mode: u32,
    pub bytes: Vec<u8>,
    pub digest: Digest,
}

impl FinalizedArtifact {
    #[must_use]
    pub fn object_path(&self) -> PathBuf {
        PathBuf::from(format!("objects/{}-{:04o}", self.digest, self.mode))
    }
}

/// The source one target payload names: the commit it was built from and the
/// exact archive of that commit's tree every target of the release extracts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetSource {
    pub commit: Digest,
    pub archive: Digest,
}

/// Pin- and version-guarded deterministic stage assembly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pipeline {
    pins: RequiredPins,
    version: ProductVersion,
}

impl Pipeline {
    #[must_use]
    pub const fn new(pins: RequiredPins, version: ProductVersion) -> Self {
        Self { pins, version }
    }

    /// Build and publish one target. The payload pointer is always last.
    ///
    /// Pin verification is a release-path check, not a hot-path one, and it is
    /// ordered first deliberately: it compares the observed pins against the
    /// required ones in memory, with no I/O, so the cost of catching drift is
    /// nothing next to the build it refuses to start.
    ///
    /// # Errors
    ///
    /// Refuses pin drift before calling the builder, names the failed build
    /// step, and preserves a previously published payload when object
    /// publication is interrupted.
    pub fn stage<B: Builder, P: RootedPublish>(
        &self,
        target: ReleaseTarget,
        source: TargetSource,
        observed: &BuildPins,
        builder: &mut B,
        publisher: &mut P,
    ) -> Result<StagedTarget, StageError> {
        self.pins.verify(observed)?;
        let built = build_artifacts(target, &self.version, builder)?;
        let payload = make_payload(
            target,
            source,
            &built.canvas,
            built.symbols,
            self.pins.get(),
            &built.artifacts,
        )?;
        let payload_bytes = wire::encode(&payload)?;
        let payload_digest = Digest::compute(Domain::DistTargetPayload, &payload_bytes);
        publish_artifacts(publisher, &built.artifacts)?;
        let payload_path = PathBuf::from(format!("targets/{target}/payload.json"));
        publisher
            .publish(&payload_path, DATA_MODE, &payload_bytes)
            .map_err(|source| StageError::Publish {
                path: payload_path.clone(),
                source,
            })?;
        Ok(StagedTarget {
            payload,
            payload_bytes,
            payload_digest,
            payload_path,
            canvas: built.canvas,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error(transparent)]
    Pin(#[from] PinError),
    #[error("{step:?} build failed: {source}")]
    Build { step: BuildStep, source: BuildError },
    #[error("failed to publish {path}: {source}")]
    Publish {
        path: PathBuf,
        source: atomic::Error,
    },
    #[error("target payload encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
struct Artifact {
    path: String,
    mode: u32,
    bytes: Vec<u8>,
    digest: Digest,
}

struct BuiltArtifacts {
    canvas: Canvas,
    symbols: Option<MacSymbolMetadata>,
    artifacts: Vec<Artifact>,
}

fn build_artifacts<B: Builder>(
    target: ReleaseTarget,
    version: &ProductVersion,
    builder: &mut B,
) -> Result<BuiltArtifacts, StageError> {
    let canvas = builder.build_app().map_err(|source| StageError::Build {
        step: BuildStep::App,
        source,
    })?;
    let native = builder
        .build_native(target, &canvas)
        .map_err(|source| StageError::Build {
            step: BuildStep::Native,
            source,
        })?;
    canvas.verify_linked(&native).map_err(native_build_error)?;
    if target == ReleaseTarget::X86_64PcWindowsMsvc {
        canvas
            .verify_windows_authenticode(&native)
            .map_err(native_build_error)?;
    }
    let report = builder
        .report_version()
        .map_err(|source| StageError::Build {
            step: BuildStep::Native,
            source,
        })?;
    canvas
        .verify_release_report(version, &report)
        .map_err(native_build_error)?;
    let symbols = builder.take_symbol_artifact();
    let windows_debug = builder.take_windows_debug_artifact();
    let mut artifacts = Vec::new();
    let native = artifact(target.executable_path(), EXECUTABLE_MODE, native);
    let symbols = match (target, symbols) {
        (ReleaseTarget::Aarch64AppleDarwin, Some(bytes)) => {
            let (artifact, metadata) = symbol_artifact(bytes, &native.digest)?;
            artifacts.push(artifact);
            Some(metadata)
        }
        (ReleaseTarget::Aarch64AppleDarwin, None) => {
            return Err(native_build_error(
                "macOS build produced no symbol artifact",
            ));
        }
        (ReleaseTarget::X86_64UnknownLinuxMusl | ReleaseTarget::X86_64PcWindowsMsvc, None) => None,
        (ReleaseTarget::X86_64UnknownLinuxMusl, Some(_)) => {
            return Err(native_build_error("Linux build produced a symbol artifact"));
        }
        (ReleaseTarget::X86_64PcWindowsMsvc, Some(_)) => {
            return Err(native_build_error(
                "Windows build produced a macOS symbol artifact",
            ));
        }
    };
    match (target, windows_debug) {
        (ReleaseTarget::X86_64PcWindowsMsvc, Some(bytes)) if !bytes.is_empty() => {
            artifacts.push(artifact(WINDOWS_DEBUG_PATH, DATA_MODE, bytes));
        }
        (ReleaseTarget::X86_64PcWindowsMsvc, Some(_)) => {
            return Err(native_build_error("Windows build produced an empty PDB"));
        }
        (ReleaseTarget::X86_64PcWindowsMsvc, None) => {
            return Err(native_build_error("Windows build produced no PDB"));
        }
        (ReleaseTarget::Aarch64AppleDarwin | ReleaseTarget::X86_64UnknownLinuxMusl, None) => {}
        (_, Some(_)) => {
            return Err(native_build_error(
                "non-Windows build produced a Windows debug artifact",
            ));
        }
    }
    artifacts.push(native);
    Ok(BuiltArtifacts {
        canvas,
        symbols,
        artifacts,
    })
}

fn symbol_artifact(
    bytes: Vec<u8>,
    native: &Digest,
) -> Result<(Artifact, MacSymbolMetadata), StageError> {
    let document = MacSymbolArtifact::decode(&bytes)
        .map_err(|error| native_build_error(format!("symbol artifact does not decode: {error}")))?;
    if document.executable != *native {
        return Err(native_build_error(
            "symbol artifact has stale executable binding",
        ));
    }
    let artifact = artifact(MAC_SYMBOL_PATH, DATA_MODE, bytes);
    let metadata = MacSymbolMetadata {
        path: artifact.path.clone(),
        mode: artifact.mode,
        bytes: artifact.bytes.len() as u64,
        digest: artifact.digest.clone(),
        uuid: document.uuid,
        executable: document.executable,
    };
    if !metadata.uuid_is_canonical() {
        return Err(native_build_error("symbol artifact UUID is not canonical"));
    }
    Ok((artifact, metadata))
}

fn native_build_error(message: impl ToString) -> StageError {
    StageError::Build {
        step: BuildStep::Native,
        source: BuildError::new(message.to_string()),
    }
}

fn artifact(path: impl Into<String>, mode: u32, bytes: Vec<u8>) -> Artifact {
    let digest = Digest::compute(Domain::DistArtifact, &bytes);
    Artifact {
        path: path.into(),
        mode,
        bytes,
        digest,
    }
}

fn make_payload(
    target: ReleaseTarget,
    source: TargetSource,
    canvas: &Canvas,
    symbols: Option<MacSymbolMetadata>,
    pins: &BuildPins,
    artifacts: &[Artifact],
) -> Result<TargetPayload, serde_json::Error> {
    let mut inventory = artifacts.iter().map(inventory_entry).collect::<Vec<_>>();
    inventory.sort_by(|left, right| left.path.cmp(&right.path));
    let pins_digest = Digest::compute(Domain::DistPins, &wire::encode(pins)?);
    let inventory_digest = Digest::compute(Domain::DistInventory, &wire::encode(&inventory)?);
    Ok(TargetPayload {
        version: TARGET_PAYLOAD_VERSION,
        target,
        source: source.commit,
        archive: source.archive,
        app: canvas.generation().clone(),
        canvas: canvas.archive().clone(),
        native: artifacts
            .iter()
            .find(|artifact| artifact.path == target.executable_path())
            .expect("the built inventory carries the native executable")
            .digest
            .clone(),
        symbols,
        pins: pins.clone(),
        pins_digest,
        inventory,
        inventory_digest,
    })
}

fn inventory_entry(artifact: &Artifact) -> InventoryEntry {
    InventoryEntry {
        path: artifact.path.to_owned(),
        mode: artifact.mode,
        bytes: artifact.bytes.len() as u64,
        digest: artifact.digest.clone(),
    }
}

fn publish_artifacts<P: RootedPublish>(
    publisher: &mut P,
    artifacts: &[Artifact],
) -> Result<(), StageError> {
    let mut ordered = artifacts.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));
    for artifact in ordered {
        let path = object_path(artifact);
        publisher
            .publish(&path, artifact.mode, &artifact.bytes)
            .map_err(|source| StageError::Publish { path, source })?;
    }
    Ok(())
}

/// Replace one admitted unsigned Darwin executable with its signed and
/// notarized bytes, then rebind the dSYM artifact and payload to those final
/// bytes without running build code.
///
/// The caller owns Developer ID and notarization verification. This function
/// admits the complete unsigned tuple, changes only the executable-dependent
/// identities, and returns exact content-addressed files for atomic
/// manifest-last publication.
///
/// # Errors
///
/// Refuses a noncanonical or non-Darwin payload, stale input objects, malformed
/// symbol evidence, or pin and inventory digests that do not bind their
/// documents.
pub fn restage_signed_darwin(
    payload_bytes: &[u8],
    unsigned_native: &[u8],
    symbol_bytes: &[u8],
    signed_native: Vec<u8>,
) -> Result<FinalizedDarwinStage, StageError> {
    let payload = admit_unsigned_darwin_payload(payload_bytes)?;
    let symbol_document =
        admit_unsigned_darwin_artifacts(&payload, unsigned_native, symbol_bytes)?;
    finalize_signed_darwin(payload, symbol_document, signed_native)
}

fn admit_unsigned_darwin_payload(payload_bytes: &[u8]) -> Result<TargetPayload, StageError> {
    let payload: TargetPayload = wire::decode(payload_bytes).map_err(|error| {
        native_build_error(format!("unsigned payload does not decode: {error}"))
    })?;
    let canonical = wire::encode(&payload)?;
    if canonical != payload_bytes {
        return Err(native_build_error("unsigned payload is not canonical"));
    }
    if payload.version != TARGET_PAYLOAD_VERSION
        || payload.target != ReleaseTarget::Aarch64AppleDarwin
    {
        return Err(native_build_error(
            "signed Darwin restaging requires the current aarch64-apple-darwin payload",
        ));
    }
    let expected_pins = Digest::compute(Domain::DistPins, &wire::encode(&payload.pins)?);
    if payload.pins_digest != expected_pins {
        return Err(native_build_error(
            "unsigned payload has a stale pins digest",
        ));
    }
    let expected_inventory =
        Digest::compute(Domain::DistInventory, &wire::encode(&payload.inventory)?);
    if payload.inventory_digest != expected_inventory {
        return Err(native_build_error(
            "unsigned payload has a stale inventory digest",
        ));
    }
    if payload.inventory.len() != 2 {
        return Err(native_build_error(
            "unsigned Darwin payload does not contain exactly its executable and dSYM artifact",
        ));
    }
    Ok(payload)
}

fn admit_unsigned_darwin_artifacts(
    payload: &TargetPayload,
    unsigned_native: &[u8],
    symbol_bytes: &[u8],
) -> Result<MacSymbolArtifact, StageError> {
    let native_path = ReleaseTarget::Aarch64AppleDarwin.executable_path();
    let native_entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == native_path)
        .ok_or_else(|| native_build_error("unsigned Darwin payload has no executable"))?;
    let symbol_entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == MAC_SYMBOL_PATH)
        .ok_or_else(|| native_build_error("unsigned Darwin payload has no dSYM artifact"))?;
    let unsigned_digest = Digest::compute(Domain::DistArtifact, unsigned_native);
    if native_entry.mode != EXECUTABLE_MODE
        || native_entry.bytes != unsigned_native.len() as u64
        || native_entry.digest != unsigned_digest
        || payload.native != unsigned_digest
    {
        return Err(native_build_error(
            "unsigned Darwin executable does not match its payload",
        ));
    }
    let symbol_digest = Digest::compute(Domain::DistArtifact, symbol_bytes);
    if symbol_entry.mode != DATA_MODE
        || symbol_entry.bytes != symbol_bytes.len() as u64
        || symbol_entry.digest != symbol_digest
    {
        return Err(native_build_error(
            "unsigned Darwin dSYM artifact does not match its inventory",
        ));
    }
    let symbol_document = MacSymbolArtifact::decode(symbol_bytes)
        .map_err(|error| native_build_error(format!("dSYM artifact does not decode: {error}")))?;
    let symbol_metadata = payload
        .symbols
        .as_ref()
        .ok_or_else(|| native_build_error("unsigned Darwin payload has no dSYM metadata"))?;
    if symbol_metadata.path != symbol_entry.path
        || symbol_metadata.mode != symbol_entry.mode
        || symbol_metadata.bytes != symbol_entry.bytes
        || symbol_metadata.digest != symbol_entry.digest
        || symbol_metadata.uuid != symbol_document.uuid
        || symbol_metadata.executable != unsigned_digest
        || symbol_document.executable != unsigned_digest
    {
        return Err(native_build_error(
            "unsigned Darwin dSYM metadata does not bind its artifact and executable",
        ));
    }
    Ok(symbol_document)
}

fn finalize_signed_darwin(
    mut payload: TargetPayload,
    mut symbol_document: MacSymbolArtifact,
    signed_native: Vec<u8>,
) -> Result<FinalizedDarwinStage, StageError> {
    let native_path = ReleaseTarget::Aarch64AppleDarwin.executable_path();
    let native = artifact(native_path, EXECUTABLE_MODE, signed_native);
    symbol_document.executable = native.digest.clone();
    let rebound_symbols = symbol_document
        .encode()
        .map_err(|error| native_build_error(format!("dSYM artifact cannot be rebound: {error}")))?;
    let (symbols, metadata) = symbol_artifact(rebound_symbols, &native.digest)?;
    let mut artifacts = vec![symbols, native];
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));

    payload.native = artifacts
        .iter()
        .find(|artifact| artifact.path == native_path)
        .expect("finalized Darwin artifacts include the executable")
        .digest
        .clone();
    payload.symbols = Some(metadata);
    payload.inventory = artifacts.iter().map(inventory_entry).collect();
    payload.inventory_digest =
        Digest::compute(Domain::DistInventory, &wire::encode(&payload.inventory)?);
    let payload_bytes = wire::encode(&payload)?;
    let payload_digest = Digest::compute(Domain::DistTargetPayload, &payload_bytes);
    Ok(FinalizedDarwinStage {
        payload,
        payload_bytes,
        payload_digest,
        payload_path: PathBuf::from(format!(
            "targets/{}/payload.json",
            ReleaseTarget::Aarch64AppleDarwin
        )),
        artifacts: artifacts
            .into_iter()
            .map(|artifact| FinalizedArtifact {
                path: artifact.path,
                mode: artifact.mode,
                bytes: artifact.bytes,
                digest: artifact.digest,
            })
            .collect(),
    })
}

fn object_path(artifact: &Artifact) -> PathBuf {
    PathBuf::from(format!("objects/{}-{:04o}", artifact.digest, artifact.mode))
}

#[cfg(test)]
mod signed_darwin_tests {
    use super::*;

    fn digest(domain: Domain, bytes: &[u8]) -> Digest {
        Digest::compute(domain, bytes)
    }

    fn unsigned_stage() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let native = b"unsigned executable".to_vec();
        let native_digest = digest(Domain::DistArtifact, &native);
        let symbols = MacSymbolArtifact {
            version: wire::manifest::MAC_SYMBOL_ARTIFACT_VERSION,
            uuid: "01234567-89AB-CDEF-0123-456789ABCDEF".to_owned(),
            executable: native_digest.clone(),
            commit: "1111111111111111111111111111111111111111".to_owned(),
            dirty: false,
            info: b"Info.plist".to_vec(),
            dwarf: b"debug symbols".to_vec(),
        }
        .encode()
        .expect("symbols encode");
        let symbols_digest = digest(Domain::DistArtifact, &symbols);
        let pins = BuildPins {
            rust: "rustc test".to_owned(),
            wasm_target: "wasm32-unknown-unknown".to_owned(),
            trunk: "trunk test".to_owned(),
            proof_container:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        };
        let inventory = vec![
            InventoryEntry {
                path: "symbols/spice.symbols".to_owned(),
                mode: DATA_MODE,
                bytes: symbols.len() as u64,
                digest: symbols_digest.clone(),
            },
            InventoryEntry {
                path: "bin/spice".to_owned(),
                mode: EXECUTABLE_MODE,
                bytes: native.len() as u64,
                digest: native_digest.clone(),
            },
        ];
        let payload = TargetPayload {
            version: TARGET_PAYLOAD_VERSION,
            target: ReleaseTarget::Aarch64AppleDarwin,
            source: digest(Domain::DistSourceCommit, b"source"),
            archive: digest(Domain::DistSourceArchive, b"archive"),
            app: digest(Domain::DistArtifact, b"app"),
            canvas: digest(Domain::DistArtifact, b"canvas"),
            native: native_digest.clone(),
            symbols: Some(MacSymbolMetadata {
                path: "symbols/spice.symbols".to_owned(),
                mode: DATA_MODE,
                bytes: symbols.len() as u64,
                digest: symbols_digest,
                uuid: "01234567-89AB-CDEF-0123-456789ABCDEF".to_owned(),
                executable: native_digest,
            }),
            pins: pins.clone(),
            pins_digest: digest(Domain::DistPins, &wire::encode(&pins).expect("pins encode")),
            inventory_digest: digest(
                Domain::DistInventory,
                &wire::encode(&inventory).expect("inventory encodes"),
            ),
            inventory,
        };
        (
            wire::encode(&payload).expect("payload encodes"),
            native,
            symbols,
        )
    }

    #[test]
    fn signed_darwin_restage_rebinds_the_executable_and_symbols() {
        let (payload, native, symbols) = unsigned_stage();
        let signed = b"signed executable".to_vec();
        let staged =
            restage_signed_darwin(&payload, &native, &symbols, signed.clone()).expect("restage");

        let native_digest = digest(Domain::DistArtifact, &signed);
        assert_eq!(staged.payload.native, native_digest);
        assert_eq!(
            staged
                .payload
                .symbols
                .as_ref()
                .expect("symbol metadata")
                .executable,
            native_digest
        );
        let symbol = staged
            .artifacts
            .iter()
            .find(|artifact| artifact.path == MAC_SYMBOL_PATH)
            .expect("symbol artifact");
        assert_eq!(
            MacSymbolArtifact::decode(&symbol.bytes)
                .expect("symbols decode")
                .executable,
            native_digest
        );
        assert_eq!(
            staged.payload.inventory_digest,
            digest(
                Domain::DistInventory,
                &wire::encode(&staged.payload.inventory).expect("inventory encodes")
            )
        );
    }

    #[test]
    fn signed_darwin_restage_refuses_a_stale_unsigned_executable() {
        let (payload, _, symbols) = unsigned_stage();
        let error = restage_signed_darwin(
            &payload,
            b"different unsigned executable",
            &symbols,
            b"signed".to_vec(),
        )
        .expect_err("stale executable");
        assert!(
            error
                .to_string()
                .contains("executable does not match its payload")
        );
    }
}

