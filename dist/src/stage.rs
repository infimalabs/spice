//! Ordered builds and manifest-last atomic staging.

use std::path::PathBuf;

use atomic::RootedPublish;
use wire::manifest::{
    BuildPins, InventoryEntry, MacSymbolArtifact, MacSymbolMetadata, ProductVersion, ReleaseTarget,
    SYMBOL_RECIPE_VERSION, SymbolRecipe, TARGET_PAYLOAD_VERSION, TargetPayload,
};
use wire::{Digest, Domain};

use crate::canvas::Canvas;
use crate::pins::{PinError, RequiredPins};

const DATA_MODE: u32 = 0o644;
const EXECUTABLE_MODE: u32 = 0o755;
const MAC_SYMBOL_PATH: &str = "symbols/spice.symbols";
const WINDOWS_DEBUG_PATH: &str = "symbols/spice.pdb";
const LINUX_DEBUG_PATH: &str = "symbols/spice.debug";
const SYMBOL_RECIPE_PATH: &str = "symbols/recipe.json";

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
    /// Derive the platform build identity from the final executable.
    fn build_id(&mut self, _target: ReleaseTarget, native: &[u8]) -> Result<String, BuildError> {
        Ok(Digest::compute(Domain::DistArtifact, native).to_string())
    }

    fn take_symbol_artifact(&mut self) -> Option<Vec<u8>> {
        None
    }
    /// Take the detached unstripped Linux executable, when explicitly built.
    fn take_linux_debug_artifact(&mut self) -> Option<Vec<u8>> {
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
    /// Full lowercase Git object name carried by the selected source record.
    pub source_commit: String,
    /// Domain-separated identity of the selected `Cargo.lock`.
    pub cargo_lock: Digest,
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
        let mut built = build_artifacts(target, &self.version, builder)?;
        let recipe = symbol_recipe(
            target,
            &source,
            self.pins.get(),
            &built.build_id,
            &built.artifacts,
        )?;
        built.artifacts.push(artifact(
            SYMBOL_RECIPE_PATH,
            DATA_MODE,
            wire::encode(&recipe)?,
        ));
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
    build_id: String,
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
    let build_id = builder
        .build_id(target, &native)
        .map_err(|source| StageError::Build {
            step: BuildStep::Native,
            source,
        })?;
    let symbols = builder.take_symbol_artifact();
    let linux_debug = builder.take_linux_debug_artifact();
    let windows_debug = builder.take_windows_debug_artifact();
    let mut artifacts = Vec::new();
    let native = artifact(target.executable_path(), EXECUTABLE_MODE, native);
    let symbols = match (target, symbols) {
        (ReleaseTarget::Aarch64AppleDarwin, Some(bytes)) => {
            let (artifact, metadata) = symbol_artifact(bytes, &native.digest)?;
            artifacts.push(artifact);
            Some(metadata)
        }
        (ReleaseTarget::Aarch64AppleDarwin, None)
        | (ReleaseTarget::X86_64UnknownLinuxMusl | ReleaseTarget::X86_64PcWindowsMsvc, None) => {
            None
        }
        (ReleaseTarget::X86_64UnknownLinuxMusl, Some(_)) => {
            return Err(native_build_error(
                "Linux build produced a macOS symbol artifact",
            ));
        }
        (ReleaseTarget::X86_64PcWindowsMsvc, Some(_)) => {
            return Err(native_build_error(
                "Windows build produced a macOS symbol artifact",
            ));
        }
    };
    match (target, linux_debug) {
        (ReleaseTarget::X86_64UnknownLinuxMusl, Some(bytes)) if !bytes.is_empty() => {
            artifacts.push(artifact(LINUX_DEBUG_PATH, DATA_MODE, bytes));
        }
        (ReleaseTarget::X86_64UnknownLinuxMusl, Some(_)) => {
            return Err(native_build_error(
                "Linux build produced an empty debug artifact",
            ));
        }
        (_, None) => {}
        (_, Some(_)) => {
            return Err(native_build_error(
                "non-Linux build produced a Linux debug artifact",
            ));
        }
    }
    match (target, windows_debug) {
        (ReleaseTarget::X86_64PcWindowsMsvc, Some(bytes)) if !bytes.is_empty() => {
            artifacts.push(artifact(WINDOWS_DEBUG_PATH, DATA_MODE, bytes));
        }
        (ReleaseTarget::X86_64PcWindowsMsvc, Some(_)) => {
            return Err(native_build_error("Windows build produced an empty PDB"));
        }
        (_, None) => {}
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
        build_id,
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
fn symbol_recipe(
    target: ReleaseTarget,
    source: &TargetSource,
    pins: &BuildPins,
    build_id: &str,
    artifacts: &[Artifact],
) -> Result<SymbolRecipe, serde_json::Error> {
    let released_binary = artifacts
        .iter()
        .find(|artifact| artifact.path == target.executable_path())
        .expect("the built inventory carries the native executable")
        .digest
        .clone();
    let vendor_authority = Digest::compute(Domain::DistPins, &wire::encode(pins)?);
    let build_id = build_id.to_owned();
    let mut flags = vec![
        "rustc".to_owned(),
        "--locked".to_owned(),
        "--release".to_owned(),
        "-p".to_owned(),
        "spice".to_owned(),
        "--bin".to_owned(),
        "spice".to_owned(),
        "--target".to_owned(),
        target.to_string(),
        "--config".to_owned(),
        "profile.release.debug=\"line-tables-only\"".to_owned(),
    ];
    flags.push(
        match target {
            ReleaseTarget::Aarch64AppleDarwin => "post-link=strip",
            ReleaseTarget::X86_64UnknownLinuxMusl => "post-link=strip --strip-all",
            ReleaseTarget::X86_64PcWindowsMsvc => "debug-sidecar=spice.pdb",
        }
        .to_owned(),
    );
    Ok(SymbolRecipe {
        version: SYMBOL_RECIPE_VERSION,
        target,
        source_commit: source.source_commit.clone(),
        source: source.commit.clone(),
        source_archive: source.archive.clone(),
        cargo_lock: source.cargo_lock.clone(),
        vendor_authority,
        proof_container: pins.proof_container.clone(),
        toolchain: pins.rust.clone(),
        flags,
        released_binary,
        build_id,
    })
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

/// The inventoried objects of one Darwin stage: its executable, its compact
/// symbol recipe, and the dSYM artifact when symbols were captured.
#[derive(Clone, Copy, Debug)]
pub struct DarwinObjects<'a> {
    pub native: &'a [u8],
    pub recipe: &'a [u8],
    pub symbols: Option<&'a [u8]>,
}

/// The executable-bound documents one admitted Darwin stage carries.
struct AdmittedDarwin {
    recipe: SymbolRecipe,
    symbols: Option<MacSymbolArtifact>,
}

/// Replace one admitted unsigned Darwin executable with its signed and
/// notarized bytes, then rebind the symbol recipe, any dSYM artifact, and the
/// payload to those final bytes without running build code.
///
/// The caller owns Developer ID and notarization verification. This function
/// admits the complete unsigned tuple, changes only the executable-dependent
/// identities, and returns exact content-addressed files for atomic
/// manifest-last publication.
///
/// # Errors
///
/// Refuses a noncanonical or non-Darwin payload, stale input objects, a symbol
/// recipe or symbol evidence that does not bind the executable, or pin and
/// inventory digests that do not bind their documents.
pub fn restage_signed_darwin(
    payload_bytes: &[u8],
    unsigned: DarwinObjects<'_>,
    signed_native: Vec<u8>,
) -> Result<FinalizedDarwinStage, StageError> {
    let payload = admit_darwin_payload(payload_bytes)?;
    let admitted = admit_darwin_artifacts(&payload, unsigned)?;
    finalize_signed_darwin(payload, admitted, signed_native)
}

/// Admit one complete Darwin payload and every object it inventories.
///
/// # Errors
///
/// Refuses noncanonical payloads, an inventory other than the executable,
/// symbol recipe, and optional dSYM artifact, stale digests, a recipe naming
/// another executable, malformed or unbound dSYM evidence, and executables
/// whose linked canvas differs from the payload.
pub fn admit_finalized_darwin(
    payload_bytes: &[u8],
    objects: DarwinObjects<'_>,
) -> Result<TargetPayload, StageError> {
    let payload = admit_darwin_payload(payload_bytes)?;
    admit_darwin_artifacts(&payload, objects)?;
    let linked = rack::link::extract(objects.native)
        .map_err(|error| native_build_error(format!("Darwin canvas does not decode: {error}")))?;
    let observed = Digest::compute(Domain::DistArtifact, linked);
    if observed != payload.canvas {
        return Err(native_build_error(format!(
            "Darwin canvas differs: expected {}, observed {observed}",
            payload.canvas
        )));
    }
    Ok(payload)
}

fn admit_darwin_payload(payload_bytes: &[u8]) -> Result<TargetPayload, StageError> {
    let payload: TargetPayload = wire::decode(payload_bytes)
        .map_err(|error| native_build_error(format!("Darwin payload does not decode: {error}")))?;
    let canonical = wire::encode(&payload)?;
    if canonical != payload_bytes {
        return Err(native_build_error("Darwin payload is not canonical"));
    }
    if payload.version != TARGET_PAYLOAD_VERSION
        || payload.target != ReleaseTarget::Aarch64AppleDarwin
    {
        return Err(native_build_error(
            "Darwin admission requires the current aarch64-apple-darwin payload",
        ));
    }
    let expected_pins = Digest::compute(Domain::DistPins, &wire::encode(&payload.pins)?);
    if payload.pins_digest != expected_pins {
        return Err(native_build_error("Darwin payload has a stale pins digest"));
    }
    let expected_inventory =
        Digest::compute(Domain::DistInventory, &wire::encode(&payload.inventory)?);
    if payload.inventory_digest != expected_inventory {
        return Err(native_build_error(
            "Darwin payload has a stale inventory digest",
        ));
    }
    Ok(payload)
}

fn admit_darwin_artifacts(
    payload: &TargetPayload,
    objects: DarwinObjects<'_>,
) -> Result<AdmittedDarwin, StageError> {
    let native_path = ReleaseTarget::Aarch64AppleDarwin.executable_path();
    let mut expected = vec![native_path, SYMBOL_RECIPE_PATH];
    if objects.symbols.is_some() {
        expected.push(MAC_SYMBOL_PATH);
    }
    expected.sort_unstable();
    if !payload
        .inventory
        .iter()
        .map(|entry| entry.path.as_str())
        .eq(expected)
    {
        return Err(native_build_error(
            "Darwin payload does not inventory exactly its executable, symbol recipe, and supplied dSYM artifact",
        ));
    }
    let native_digest = admit_darwin_object(payload, native_path, EXECUTABLE_MODE, objects.native)?;
    if payload.native != native_digest {
        return Err(native_build_error(
            "Darwin executable does not match its payload",
        ));
    }
    admit_darwin_object(payload, SYMBOL_RECIPE_PATH, DATA_MODE, objects.recipe)?;
    let recipe = admit_darwin_recipe(objects.recipe, &native_digest)?;
    let symbols = match (objects.symbols, payload.symbols.as_ref()) {
        (Some(bytes), Some(metadata)) => Some(admit_darwin_symbols(
            payload,
            bytes,
            metadata,
            &native_digest,
        )?),
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => {
            return Err(native_build_error(
                "Darwin dSYM metadata and artifact are not both present",
            ));
        }
    };
    Ok(AdmittedDarwin { recipe, symbols })
}

fn admit_darwin_object(
    payload: &TargetPayload,
    path: &str,
    mode: u32,
    bytes: &[u8],
) -> Result<Digest, StageError> {
    let digest = Digest::compute(Domain::DistArtifact, bytes);
    let matches = payload.inventory.iter().any(|entry| {
        entry.path == path
            && entry.mode == mode
            && entry.bytes == bytes.len() as u64
            && entry.digest == digest
    });
    if matches {
        Ok(digest)
    } else {
        Err(native_build_error(format!(
            "Darwin {path} does not match its inventory"
        )))
    }
}

fn admit_darwin_recipe(bytes: &[u8], native: &Digest) -> Result<SymbolRecipe, StageError> {
    let recipe: SymbolRecipe = wire::decode(bytes).map_err(|error| {
        native_build_error(format!("Darwin symbol recipe does not decode: {error}"))
    })?;
    if wire::encode(&recipe)? != bytes {
        return Err(native_build_error("Darwin symbol recipe is not canonical"));
    }
    if recipe.version != SYMBOL_RECIPE_VERSION
        || recipe.target != ReleaseTarget::Aarch64AppleDarwin
        || recipe.released_binary != *native
    {
        return Err(native_build_error(
            "Darwin symbol recipe does not bind its executable",
        ));
    }
    Ok(recipe)
}

fn admit_darwin_symbols(
    payload: &TargetPayload,
    bytes: &[u8],
    metadata: &MacSymbolMetadata,
    native: &Digest,
) -> Result<MacSymbolArtifact, StageError> {
    let digest = admit_darwin_object(payload, MAC_SYMBOL_PATH, DATA_MODE, bytes)?;
    let document = MacSymbolArtifact::decode(bytes)
        .map_err(|error| native_build_error(format!("dSYM artifact does not decode: {error}")))?;
    if metadata.path != MAC_SYMBOL_PATH
        || metadata.mode != DATA_MODE
        || metadata.bytes != bytes.len() as u64
        || metadata.digest != digest
        || metadata.uuid != document.uuid
        || metadata.executable != *native
        || document.executable != *native
    {
        return Err(native_build_error(
            "Darwin dSYM metadata does not bind its artifact and executable",
        ));
    }
    Ok(document)
}

fn finalize_signed_darwin(
    mut payload: TargetPayload,
    admitted: AdmittedDarwin,
    signed_native: Vec<u8>,
) -> Result<FinalizedDarwinStage, StageError> {
    let native = artifact(
        ReleaseTarget::Aarch64AppleDarwin.executable_path(),
        EXECUTABLE_MODE,
        signed_native,
    );
    let mut recipe = admitted.recipe;
    recipe.released_binary = native.digest.clone();
    let mut artifacts = vec![artifact(
        SYMBOL_RECIPE_PATH,
        DATA_MODE,
        wire::encode(&recipe)?,
    )];
    payload.symbols = match admitted.symbols {
        Some(mut document) => {
            document.executable = native.digest.clone();
            let rebound = document.encode().map_err(|error| {
                native_build_error(format!("dSYM artifact cannot be rebound: {error}"))
            })?;
            let (symbols, metadata) = symbol_artifact(rebound, &native.digest)?;
            artifacts.push(symbols);
            Some(metadata)
        }
        None => None,
    };
    payload.native = native.digest.clone();
    artifacts.push(native);
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
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

    struct UnsignedStage {
        payload: Vec<u8>,
        native: Vec<u8>,
        recipe: Vec<u8>,
        symbols: Option<Vec<u8>>,
    }

    impl UnsignedStage {
        fn objects(&self) -> DarwinObjects<'_> {
            DarwinObjects {
                native: &self.native,
                recipe: &self.recipe,
                symbols: self.symbols.as_deref(),
            }
        }
    }

    fn recipe(released_binary: Digest, pins: &BuildPins) -> SymbolRecipe {
        SymbolRecipe {
            version: SYMBOL_RECIPE_VERSION,
            target: ReleaseTarget::Aarch64AppleDarwin,
            source_commit: "1111111111111111111111111111111111111111".to_owned(),
            source: digest(Domain::DistSourceCommit, b"source"),
            source_archive: digest(Domain::DistSourceArchive, b"archive"),
            cargo_lock: digest(Domain::DistArtifact, b"Cargo.lock"),
            vendor_authority: digest(Domain::DistPins, &wire::encode(pins).expect("pins encode")),
            proof_container: pins.proof_container.clone(),
            toolchain: pins.rust.clone(),
            flags: vec!["rustc".to_owned()],
            released_binary,
            build_id: "01234567-89AB-CDEF-0123-456789ABCDEF".to_owned(),
        }
    }

    fn entry(path: &str, mode: u32, bytes: &[u8]) -> InventoryEntry {
        InventoryEntry {
            path: path.to_owned(),
            mode,
            bytes: bytes.len() as u64,
            digest: digest(Domain::DistArtifact, bytes),
        }
    }

    fn unsigned_stage(with_symbols: bool, recipe_binary: Option<Digest>) -> UnsignedStage {
        let native = b"unsigned executable".to_vec();
        let native_digest = digest(Domain::DistArtifact, &native);
        let pins = BuildPins {
            rust: "rustc test".to_owned(),
            wasm_target: "wasm32-unknown-unknown".to_owned(),
            trunk: "trunk test".to_owned(),
            proof_container:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        };
        let recipe = wire::encode(&recipe(
            recipe_binary.unwrap_or_else(|| native_digest.clone()),
            &pins,
        ))
        .expect("recipe encodes");
        let mut inventory = vec![
            entry("bin/spice", EXECUTABLE_MODE, &native),
            entry(SYMBOL_RECIPE_PATH, DATA_MODE, &recipe),
        ];
        let symbols = with_symbols.then(|| {
            MacSymbolArtifact {
                version: wire::manifest::MAC_SYMBOL_ARTIFACT_VERSION,
                uuid: "01234567-89AB-CDEF-0123-456789ABCDEF".to_owned(),
                executable: native_digest.clone(),
                commit: "1111111111111111111111111111111111111111".to_owned(),
                dirty: false,
                info: b"Info.plist".to_vec(),
                dwarf: b"debug symbols".to_vec(),
            }
            .encode()
            .expect("symbols encode")
        });
        let metadata = symbols.as_ref().map(|symbols| {
            inventory.push(entry(MAC_SYMBOL_PATH, DATA_MODE, symbols));
            MacSymbolMetadata {
                path: MAC_SYMBOL_PATH.to_owned(),
                mode: DATA_MODE,
                bytes: symbols.len() as u64,
                digest: digest(Domain::DistArtifact, symbols),
                uuid: "01234567-89AB-CDEF-0123-456789ABCDEF".to_owned(),
                executable: native_digest.clone(),
            }
        });
        let payload = TargetPayload {
            version: TARGET_PAYLOAD_VERSION,
            target: ReleaseTarget::Aarch64AppleDarwin,
            source: digest(Domain::DistSourceCommit, b"source"),
            archive: digest(Domain::DistSourceArchive, b"archive"),
            app: digest(Domain::DistArtifact, b"app"),
            canvas: digest(Domain::DistArtifact, b"canvas"),
            native: native_digest,
            symbols: metadata,
            pins: pins.clone(),
            pins_digest: digest(Domain::DistPins, &wire::encode(&pins).expect("pins encode")),
            inventory_digest: digest(
                Domain::DistInventory,
                &wire::encode(&inventory).expect("inventory encodes"),
            ),
            inventory,
        };
        UnsignedStage {
            payload: wire::encode(&payload).expect("payload encodes"),
            native,
            recipe,
            symbols,
        }
    }

    fn finalized_recipe(staged: &FinalizedDarwinStage) -> SymbolRecipe {
        let recipe = staged
            .artifacts
            .iter()
            .find(|artifact| artifact.path == SYMBOL_RECIPE_PATH)
            .expect("recipe artifact");
        wire::decode(&recipe.bytes).expect("recipe decodes")
    }

    /// The finalized stage admits on its own, as `verify-staged-darwin` reads it.
    fn readmit(staged: &FinalizedDarwinStage) {
        let object = |path: &str| {
            staged
                .artifacts
                .iter()
                .find(|artifact| artifact.path == path)
                .map(|artifact| artifact.bytes.as_slice())
        };
        let payload = admit_darwin_payload(&staged.payload_bytes).expect("finalized payload");
        admit_darwin_artifacts(
            &payload,
            DarwinObjects {
                native: object("bin/spice").expect("finalized executable"),
                recipe: object(SYMBOL_RECIPE_PATH).expect("finalized recipe"),
                symbols: object(MAC_SYMBOL_PATH),
            },
        )
        .expect("finalized stage admits");
    }

    #[test]
    fn signed_darwin_restage_rebinds_the_executable_recipe_and_symbols() {
        let unsigned = unsigned_stage(true, None);
        let signed = b"signed executable".to_vec();
        let staged = restage_signed_darwin(&unsigned.payload, unsigned.objects(), signed.clone())
            .expect("restage");

        let native_digest = digest(Domain::DistArtifact, &signed);
        assert_eq!(staged.payload.native, native_digest);
        assert_eq!(finalized_recipe(&staged).released_binary, native_digest);
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
        readmit(&staged);
    }

    #[test]
    fn signed_darwin_restage_admits_a_default_stage_without_a_dsym() {
        let unsigned = unsigned_stage(false, None);
        let signed = b"signed executable".to_vec();
        let staged = restage_signed_darwin(&unsigned.payload, unsigned.objects(), signed.clone())
            .expect("restage");

        let native_digest = digest(Domain::DistArtifact, &signed);
        assert_eq!(staged.payload.symbols, None);
        assert_eq!(finalized_recipe(&staged).released_binary, native_digest);
        assert!(
            staged
                .payload
                .inventory
                .iter()
                .all(|entry| entry.path != MAC_SYMBOL_PATH)
        );
        readmit(&staged);
    }

    #[test]
    fn signed_darwin_restage_refuses_a_stale_unsigned_executable() {
        let unsigned = unsigned_stage(true, None);
        let stale = DarwinObjects {
            native: b"different unsigned executable",
            ..unsigned.objects()
        };
        let error = restage_signed_darwin(&unsigned.payload, stale, b"signed".to_vec())
            .expect_err("stale executable");
        assert!(error.to_string().contains("bin/spice does not match"));
    }

    #[test]
    fn darwin_admission_refuses_a_recipe_naming_another_executable() {
        let other = digest(Domain::DistArtifact, b"other executable");
        for with_symbols in [false, true] {
            let unsigned = unsigned_stage(with_symbols, Some(other.clone()));
            let error =
                restage_signed_darwin(&unsigned.payload, unsigned.objects(), b"signed".to_vec())
                    .expect_err("recipe names another executable");
            assert!(error.to_string().contains("recipe does not bind"));
        }
    }

    #[test]
    fn darwin_admission_refuses_a_dsym_the_payload_does_not_inventory() {
        let with = unsigned_stage(true, None);
        let without = unsigned_stage(false, None);
        let extra = DarwinObjects {
            symbols: with.symbols.as_deref(),
            ..without.objects()
        };
        restage_signed_darwin(&without.payload, extra, b"signed".to_vec())
            .expect_err("uninventoried dSYM");
        let missing = DarwinObjects {
            symbols: None,
            ..with.objects()
        };
        restage_signed_darwin(&with.payload, missing, b"signed".to_vec())
            .expect_err("inventoried dSYM withheld");
    }
}
