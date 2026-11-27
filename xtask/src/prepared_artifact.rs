//! One digest-addressed OCI contract for prepared release inputs.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use flate2::Compression;
use flate2::GzBuilder;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tar::Archive;

const SCHEMA: u16 = 1;
const ORAS_VERSION: &str = "1.3.0";
const CONFIG_MEDIA_TYPE: &str = "application/vnd.infimalabs.spice.prepared.v1+json";
const ARTIFACT_MEDIA_TYPE: &str = "application/vnd.infimalabs.spice.prepared.v1";
const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const DIGEST_PREFIX: &str = "sha256:";
const DIGEST_HEX_LENGTH: usize = 64;
const BUILD_KEY_NAME_MAX: usize = 64;

/// Run one `prepared-artifact` command.
///
/// # Errors
///
/// Returns usage, manifest admission, filesystem, or exact ORAS failures.
pub fn prepared_artifact(
    arguments: Vec<OsString>,
    output: &mut impl io::Write,
) -> Result<(), PreparedArtifactError> {
    match arguments.as_slice() {
        [
            command,
            kind,
            platform,
            build_key,
            media_type,
            payload,
            inventory,
            runtime,
            manifest,
        ] if command == "manifest" => write_manifest(ManifestRequest {
            kind,
            platform,
            build_key: Path::new(build_key),
            media_type,
            payload: Path::new(payload),
            inventory_root: Path::new(inventory),
            runtime,
            output: Path::new(manifest),
        }),
        [command, build_key] if command == "build-key" => {
            let digest = build_key_digest(Path::new(build_key))?;
            writeln!(output, "build key {digest}").map_err(PreparedArtifactError::Output)
        }
        [command, repository, tag, manifest, payload] if command == "publish" => {
            let digest = publish(repository, tag, Path::new(manifest), Path::new(payload))?;
            writeln!(output, "published {digest}").map_err(PreparedArtifactError::Output)
        }
        [command, reference, build_key] if command == "resolve" => {
            let (digest, manifest) = resolve(reference, build_key)?;
            writeln!(
                output,
                "resolved {digest} {} {}",
                manifest.payload.digest,
                manifest.runtime.as_deref().unwrap_or("-")
            )
            .map_err(PreparedArtifactError::Output)
        }
        [command, reference, build_key, destination] if command == "fetch" => {
            let manifest = fetch(reference, build_key, Path::new(destination))?;
            writeln!(
                output,
                "fetched {} {}",
                manifest.payload.digest,
                Path::new(destination)
                    .join(&manifest.payload.path)
                    .display()
            )
            .map_err(PreparedArtifactError::Output)
        }
        [command, root, archive] if command == "pack" => {
            pack_bundle(Path::new(root), Path::new(archive))?;
            writeln!(output, "packed {}", Path::new(archive).display())
                .map_err(PreparedArtifactError::Output)
        }
        [command, manifest, root] if command == "admit-tree" => {
            let manifest = read_manifest(Path::new(manifest))?;
            admit_tree(&manifest, Path::new(root))?;
            writeln!(output, "admitted {}", manifest.build_key.digest)
                .map_err(PreparedArtifactError::Output)
        }
        _ => Err(PreparedArtifactError::Usage),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ArtifactKind {
    Image,
    Bundle,
}

impl ArtifactKind {
    fn parse(value: &OsStr) -> Result<Self, PreparedArtifactError> {
        match value.to_str() {
            Some("image") => Ok(Self::Image),
            Some("bundle") => Ok(Self::Bundle),
            _ => Err(PreparedArtifactError::Usage),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Platform {
    LinuxAmd64,
    MacosArm64,
    WindowsAmd64,
}

impl Platform {
    fn parse(value: &OsStr) -> Result<Self, PreparedArtifactError> {
        match value.to_str() {
            Some("linux-amd64") => Ok(Self::LinuxAmd64),
            Some("macos-arm64") => Ok(Self::MacosArm64),
            Some("windows-amd64") => Ok(Self::WindowsAmd64),
            _ => Err(PreparedArtifactError::Usage),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PreparedManifest {
    schema: u16,
    kind: ArtifactKind,
    platform: Platform,
    build_key: BuildKey,
    payload: Payload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    files: Vec<FileEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BuildKey {
    digest: String,
    inputs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    path: String,
    media_type: String,
    bytes: u64,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    bytes: u64,
    digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OciDescriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(rename = "artifactType", default)]
    artifact_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OciManifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u16,
    #[serde(rename = "mediaType")]
    media_type: String,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(default)]
    artifact_type: Option<String>,
}

struct ManifestRequest<'a> {
    kind: &'a OsStr,
    platform: &'a OsStr,
    build_key: &'a Path,
    media_type: &'a OsStr,
    payload: &'a Path,
    inventory_root: &'a Path,
    runtime: &'a OsStr,
    output: &'a Path,
}

fn write_manifest(request: ManifestRequest<'_>) -> Result<(), PreparedArtifactError> {
    let ManifestRequest {
        kind,
        platform,
        build_key,
        media_type,
        payload,
        inventory_root,
        runtime,
        output,
    } = request;
    let inputs: BTreeMap<String, String> = decode_file(build_key)?;
    let build_key = BuildKey {
        digest: build_key_digest(build_key)?,
        inputs,
    };
    let payload_name = file_name(payload)?;
    let metadata = fs::metadata(payload).map_err(|source| PreparedArtifactError::Io {
        path: payload.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(PreparedArtifactError::Invalid(
            "payload must be one nonempty regular file".to_owned(),
        ));
    }
    let media_type = media_type
        .to_str()
        .filter(|value| admitted_media_type(value))
        .ok_or(PreparedArtifactError::Usage)?
        .to_owned();
    let kind = ArtifactKind::parse(kind)?;
    let runtime = match runtime.to_str() {
        Some("-") => None,
        Some(value) => Some(value.to_owned()),
        None => return Err(PreparedArtifactError::Usage),
    };
    let manifest = PreparedManifest {
        schema: SCHEMA,
        kind,
        platform: Platform::parse(platform)?,
        build_key,
        payload: Payload {
            path: payload_name,
            media_type,
            bytes: metadata.len(),
            digest: sha256_file(payload)?,
        },
        runtime,
        files: inventory(inventory_root)?,
    };
    manifest.admit()?;
    let bytes = serde_json::to_vec(&manifest).map_err(PreparedArtifactError::Encode)?;
    fs::write(output, bytes).map_err(|source| PreparedArtifactError::Io {
        path: output.to_path_buf(),
        source,
    })
}

fn build_key_digest(path: &Path) -> Result<String, PreparedArtifactError> {
    let inputs: BTreeMap<String, String> = decode_file(path)?;
    admit_inputs(&inputs)?;
    let encoded = serde_json::to_vec(&inputs).map_err(PreparedArtifactError::Encode)?;
    Ok(sha256_bytes(&encoded))
}

fn pack_bundle(root: &Path, output: &Path) -> Result<(), PreparedArtifactError> {
    let files = inventory(root)?;
    let destination = fs::File::create(output).map_err(|source| PreparedArtifactError::Io {
        path: output.to_path_buf(),
        source,
    })?;
    let encoder: GzEncoder<_> = GzBuilder::new()
        .mtime(0)
        .write(destination, Compression::best());
    let mut archive = tar::Builder::new(encoder);
    for entry in files {
        let path = root.join(&entry.path);
        let mut source = fs::File::open(&path).map_err(|source| PreparedArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        let mut header = tar::Header::new_gnu();
        header.set_size(entry.bytes);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mode(file_mode(&path)?);
        header.set_cksum();
        archive
            .append_data(&mut header, &entry.path, &mut source)
            .map_err(|source| PreparedArtifactError::Io {
                path: output.to_path_buf(),
                source,
            })?;
    }
    let encoder = archive
        .into_inner()
        .map_err(|source| PreparedArtifactError::Io {
            path: output.to_path_buf(),
            source,
        })?;
    encoder
        .finish()
        .map(|_| ())
        .map_err(|source| PreparedArtifactError::Io {
            path: output.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Result<u32, PreparedArtifactError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .map_err(|source| PreparedArtifactError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn file_mode(path: &Path) -> Result<u32, PreparedArtifactError> {
    fs::metadata(path)
        .map(|metadata| {
            if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            }
        })
        .map_err(|source| PreparedArtifactError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Resolve one deterministic discovery tag, fetch its immutable bundle, unpack
/// it into a fresh destination, and admit the complete unpacked inventory.
///
/// # Errors
///
/// Returns exact registry, archive, filesystem, or inventory admission errors.
pub(crate) fn fetch_bundle(
    repository: &str,
    tag: &str,
    expected_build_key: &str,
    destination: &Path,
) -> Result<(), PreparedArtifactError> {
    let tagged = OsString::from(format!("{repository}:{tag}"));
    let expected = OsString::from(expected_build_key);
    let (digest, _) = resolve(&tagged, &expected)?;
    let exact = OsString::from(format!("{repository}@{digest}"));
    let scratch = Scratch::new()?;
    let pulled = scratch.path.join("pulled");
    let manifest = fetch(&exact, &expected, &pulled)?;
    fs::create_dir(destination).map_err(|source| PreparedArtifactError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    let payload = pulled.join(&manifest.payload.path);
    let file = fs::File::open(&payload).map_err(|source| PreparedArtifactError::Io {
        path: payload,
        source,
    })?;
    Archive::new(GzDecoder::new(file))
        .unpack(destination)
        .map_err(|source| PreparedArtifactError::Io {
            path: destination.to_path_buf(),
            source,
        })?;
    admit_tree(&manifest, destination)
}

impl PreparedManifest {
    fn admit(&self) -> Result<(), PreparedArtifactError> {
        if self.schema != SCHEMA {
            return Err(PreparedArtifactError::Invalid(format!(
                "prepared schema must be {SCHEMA}"
            )));
        }
        admit_inputs(&self.build_key.inputs)?;
        let encoded =
            serde_json::to_vec(&self.build_key.inputs).map_err(PreparedArtifactError::Encode)?;
        if self.build_key.digest != sha256_bytes(&encoded) {
            return Err(PreparedArtifactError::Invalid(
                "prepared build-key digest does not cover its canonical inputs".to_owned(),
            ));
        }
        admit_digest("build_key.digest", &self.build_key.digest)?;
        admit_relative_path("payload.path", &self.payload.path)?;
        if !admitted_media_type(&self.payload.media_type) {
            return Err(PreparedArtifactError::Invalid(
                "payload media type is not canonical".to_owned(),
            ));
        }
        if self.payload.bytes == 0 {
            return Err(PreparedArtifactError::Invalid(
                "payload is empty".to_owned(),
            ));
        }
        admit_digest("payload.digest", &self.payload.digest)?;
        match (self.kind, self.runtime.as_deref()) {
            (ArtifactKind::Image, Some(runtime)) => {
                admit_digest("runtime", runtime)?;
            }
            (ArtifactKind::Bundle, None) => {}
            (ArtifactKind::Image, None) => {
                return Err(PreparedArtifactError::Invalid(
                    "image preparation must bind its runtime identity".to_owned(),
                ));
            }
            (ArtifactKind::Bundle, Some(_)) => {
                return Err(PreparedArtifactError::Invalid(
                    "bundle preparation must not declare an image runtime identity".to_owned(),
                ));
            }
        }
        if self.files.is_empty() {
            return Err(PreparedArtifactError::Invalid(
                "file inventory is empty".to_owned(),
            ));
        }
        let mut previous = None;
        for file in &self.files {
            admit_relative_path("file.path", &file.path)?;
            if file.bytes == 0 {
                return Err(PreparedArtifactError::Invalid(format!(
                    "inventoried file {} is empty",
                    file.path
                )));
            }
            admit_digest("file.digest", &file.digest)?;
            if previous.is_some_and(|path: &str| path >= file.path.as_str()) {
                return Err(PreparedArtifactError::Invalid(
                    "file inventory must be strictly ordered by path".to_owned(),
                ));
            }
            previous = Some(file.path.as_str());
        }
        Ok(())
    }
}

fn admit_inputs(inputs: &BTreeMap<String, String>) -> Result<(), PreparedArtifactError> {
    if inputs.is_empty() {
        return Err(PreparedArtifactError::Invalid(
            "build key has no inputs".to_owned(),
        ));
    }
    for (name, digest) in inputs {
        if name.is_empty()
            || name.len() > BUILD_KEY_NAME_MAX
            || !name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(PreparedArtifactError::Invalid(format!(
                "build-key input name is not canonical: {name}"
            )));
        }
        admit_digest("build-key input", digest)?;
    }
    Ok(())
}

fn admitted_media_type(value: &str) -> bool {
    !value.is_empty()
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        && value.contains('/')
}

fn admit_digest(field: &str, value: &str) -> Result<(), PreparedArtifactError> {
    let digest = value.strip_prefix(DIGEST_PREFIX).unwrap_or(value);
    if digest.len() != DIGEST_HEX_LENGTH
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PreparedArtifactError::Invalid(format!(
            "{field} must be one lowercase SHA-256 digest"
        )));
    }
    Ok(())
}

fn admit_relative_path(field: &str, value: &str) -> Result<(), PreparedArtifactError> {
    let path = Path::new(value);
    if value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PreparedArtifactError::Invalid(format!(
            "{field} must be one canonical relative path"
        )));
    }
    Ok(())
}

fn inventory(root: &Path) -> Result<Vec<FileEntry>, PreparedArtifactError> {
    if !root.is_dir() {
        return Err(PreparedArtifactError::Invalid(
            "inventory root must be a directory".to_owned(),
        ));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| PreparedArtifactError::Io {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| PreparedArtifactError::Io {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|source| PreparedArtifactError::Io {
                    path: path.clone(),
                    source,
                })?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let relative = path.strip_prefix(root).map_err(|_| {
                    PreparedArtifactError::Invalid("inventory path escaped its root".to_owned())
                })?;
                let path_text = slash_path(relative)?;
                let bytes = entry
                    .metadata()
                    .map_err(|source| PreparedArtifactError::Io {
                        path: path.clone(),
                        source,
                    })?
                    .len();
                if bytes == 0 {
                    return Err(PreparedArtifactError::Invalid(format!(
                        "inventoried file {path_text} is empty"
                    )));
                }
                files.push(FileEntry {
                    path: path_text,
                    bytes,
                    digest: sha256_file(&path)?,
                });
            } else {
                return Err(PreparedArtifactError::Invalid(format!(
                    "inventory contains a non-regular member: {}",
                    path.display()
                )));
            }
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn admit_tree(manifest: &PreparedManifest, root: &Path) -> Result<(), PreparedArtifactError> {
    manifest.admit()?;
    let observed = inventory(root)?;
    if observed != manifest.files {
        return Err(PreparedArtifactError::Invalid(
            "unpacked file inventory differs from the prepared manifest".to_owned(),
        ));
    }
    Ok(())
}

fn publish(
    repository: &OsStr,
    tag: &OsStr,
    manifest_path: &Path,
    payload_path: &Path,
) -> Result<String, PreparedArtifactError> {
    admit_oras_version()?;
    let repository = utf8("repository", repository)?;
    let tag = utf8("tag", tag)?;
    admit_repository(repository)?;
    admit_tag(tag)?;
    let manifest = read_manifest(manifest_path)?;
    admit_payload(&manifest, payload_path)?;
    let scratch = Scratch::new()?;
    let layout = scratch.path.join("layout");
    let staged_manifest = scratch.path.join("prepared.json");
    let staged_payload = scratch.path.join(&manifest.payload.path);
    copy_file(manifest_path, &staged_manifest)?;
    copy_file(payload_path, &staged_payload)?;
    let local = format!("prepared:{tag}");
    let payload_argument = format!("{}:{}", manifest.payload.path, manifest.payload.media_type);
    run_oras_in(
        &scratch.path,
        &[
            "push".into(),
            "--oci-layout-path".into(),
            layout.as_os_str().to_owned(),
            "--artifact-type".into(),
            ARTIFACT_MEDIA_TYPE.into(),
            "--config".into(),
            format!("prepared.json:{CONFIG_MEDIA_TYPE}").into(),
            local.clone().into(),
            payload_argument.into(),
        ],
    )?;
    let expected = descriptor(&local, Some(&layout))?.digest;
    let remote = format!("{repository}:{tag}");
    match descriptor(&remote, None) {
        Ok(existing) if existing.digest == expected => {
            admit_remote(&remote, &existing.digest, &manifest.build_key.digest)?;
            return Ok(existing.digest);
        }
        Ok(existing) => {
            return Err(PreparedArtifactError::Conflict {
                reference: remote,
                expected,
                observed: existing.digest,
            });
        }
        Err(PreparedArtifactError::OrasMissing(_)) => {}
        Err(error) => return Err(error),
    }
    run_oras(&[
        "cp".into(),
        "--from-oci-layout-path".into(),
        layout.as_os_str().to_owned(),
        local.into(),
        remote.clone().into(),
    ])?;
    let observed = descriptor(&remote, None)?.digest;
    if observed != expected {
        return Err(PreparedArtifactError::Conflict {
            reference: remote,
            expected,
            observed,
        });
    }
    admit_remote(&remote, &observed, &manifest.build_key.digest)?;
    Ok(observed)
}

fn resolve(
    reference: &OsStr,
    expected_build_key: &OsStr,
) -> Result<(String, PreparedManifest), PreparedArtifactError> {
    admit_oras_version()?;
    let reference = utf8("reference", reference)?;
    admit_tag_reference(reference)?;
    let expected_build_key = utf8("build key", expected_build_key)?;
    admit_digest("expected build key", expected_build_key)?;
    let descriptor = descriptor(reference, None)?;
    let manifest = admit_remote(reference, &descriptor.digest, expected_build_key)?;
    Ok((descriptor.digest, manifest))
}

fn fetch(
    reference: &OsStr,
    expected_build_key: &OsStr,
    destination: &Path,
) -> Result<PreparedManifest, PreparedArtifactError> {
    admit_oras_version()?;
    let reference = utf8("reference", reference)?;
    let expected_build_key = utf8("build key", expected_build_key)?;
    let (_, digest) = reference
        .rsplit_once('@')
        .ok_or(PreparedArtifactError::Usage)?;
    admit_digest("manifest digest", digest)?;
    admit_digest("expected build key", expected_build_key)?;
    let manifest = admit_remote(reference, digest, expected_build_key)?;
    fs::create_dir(destination).map_err(|source| PreparedArtifactError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    let config = destination.join("prepared.json");
    run_oras(&[
        "pull".into(),
        "--output".into(),
        destination.as_os_str().to_owned(),
        "--config".into(),
        config.as_os_str().to_owned(),
        reference.into(),
    ])?;
    let pulled = read_manifest(&config)?;
    if pulled != manifest {
        return Err(PreparedArtifactError::Invalid(
            "pulled prepared config differs from the admitted registry config".to_owned(),
        ));
    }
    admit_payload(&pulled, &destination.join(&pulled.payload.path))?;
    Ok(pulled)
}

fn admit_remote(
    reference: &str,
    digest: &str,
    expected_build_key: &str,
) -> Result<PreparedManifest, PreparedArtifactError> {
    admit_digest("manifest digest", digest)?;
    let repository = reference
        .split([':', '@'])
        .next()
        .ok_or(PreparedArtifactError::Usage)?;
    let exact = format!("{repository}@{digest}");
    let bytes = run_oras_stdout(&["manifest".into(), "fetch".into(), exact.clone().into()])?;
    let oci: OciManifest = serde_json::from_slice(&bytes).map_err(PreparedArtifactError::Decode)?;
    if oci.schema_version != 2
        || oci.media_type != OCI_MANIFEST_MEDIA_TYPE
        || oci.config.media_type != CONFIG_MEDIA_TYPE
        || oci.artifact_type.as_deref() != Some(ARTIFACT_MEDIA_TYPE)
        || oci.layers.len() != 1
        || oci.annotations.iter().any(|(key, value)| {
            key.is_empty()
                || value
                    .bytes()
                    .any(|byte| byte.is_ascii_control() && !byte.is_ascii_whitespace())
        })
    {
        return Err(PreparedArtifactError::Invalid(
            "OCI manifest is not one prepared Spice artifact".to_owned(),
        ));
    }
    let config = run_oras_stdout(&[
        "blob".into(),
        "fetch".into(),
        "--output".into(),
        "-".into(),
        format!("{repository}@{}", oci.config.digest).into(),
    ])?;
    if oci.config.size != u64::try_from(config.len()).unwrap_or(u64::MAX)
        || sha256_bytes(&config) != oci.config.digest
    {
        return Err(PreparedArtifactError::Invalid(
            "prepared config descriptor differs from its bytes".to_owned(),
        ));
    }
    let manifest: PreparedManifest =
        serde_json::from_slice(&config).map_err(PreparedArtifactError::Decode)?;
    manifest.admit()?;
    if manifest.build_key.digest != expected_build_key {
        return Err(PreparedArtifactError::Invalid(
            "prepared build key differs from the requested key".to_owned(),
        ));
    }
    let layer = &oci.layers[0];
    if layer.media_type != manifest.payload.media_type
        || layer.digest != manifest.payload.digest
        || layer.size != manifest.payload.bytes
        || layer.annotations.get("org.opencontainers.image.title") != Some(&manifest.payload.path)
    {
        return Err(PreparedArtifactError::Invalid(
            "OCI payload descriptor differs from the prepared manifest".to_owned(),
        ));
    }
    Ok(manifest)
}

fn read_manifest(path: &Path) -> Result<PreparedManifest, PreparedArtifactError> {
    let manifest: PreparedManifest = decode_file(path)?;
    manifest.admit()?;
    Ok(manifest)
}

fn decode_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, PreparedArtifactError> {
    let bytes = fs::read(path).map_err(|source| PreparedArtifactError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(PreparedArtifactError::Decode)
}

fn admit_payload(manifest: &PreparedManifest, payload: &Path) -> Result<(), PreparedArtifactError> {
    manifest.admit()?;
    if file_name(payload)? != manifest.payload.path {
        return Err(PreparedArtifactError::Invalid(
            "payload basename differs from the prepared manifest".to_owned(),
        ));
    }
    let bytes = fs::metadata(payload)
        .map_err(|source| PreparedArtifactError::Io {
            path: payload.to_path_buf(),
            source,
        })?
        .len();
    if bytes != manifest.payload.bytes || sha256_file(payload)? != manifest.payload.digest {
        return Err(PreparedArtifactError::Invalid(
            "payload bytes differ from the prepared manifest".to_owned(),
        ));
    }
    Ok(())
}

fn descriptor(
    reference: &str,
    layout: Option<&Path>,
) -> Result<OciDescriptor, PreparedArtifactError> {
    let mut arguments = vec!["manifest".into(), "fetch".into(), "--descriptor".into()];
    if let Some(layout) = layout {
        arguments.push("--oci-layout-path".into());
        arguments.push(layout.as_os_str().to_owned());
    }
    arguments.push(reference.into());
    let bytes = run_oras_stdout(&arguments)?;
    admit_manifest_descriptor(&bytes)
}

fn admit_manifest_descriptor(bytes: &[u8]) -> Result<OciDescriptor, PreparedArtifactError> {
    let descriptor: OciDescriptor =
        serde_json::from_slice(bytes).map_err(PreparedArtifactError::Decode)?;
    if descriptor.media_type != OCI_MANIFEST_MEDIA_TYPE
        || descriptor.artifact_type.as_deref() != Some(ARTIFACT_MEDIA_TYPE)
    {
        return Err(PreparedArtifactError::Invalid(
            "OCI descriptor does not name one prepared Spice artifact".to_owned(),
        ));
    }
    admit_digest("OCI manifest digest", &descriptor.digest)?;
    Ok(descriptor)
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), PreparedArtifactError> {
    fs::copy(source, destination)
        .map(|_| ())
        .map_err(|source| PreparedArtifactError::Io {
            path: destination.to_path_buf(),
            source,
        })
}

fn run_oras(arguments: &[OsString]) -> Result<(), PreparedArtifactError> {
    let output = oras(arguments)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(classify_oras(arguments, output))
    }
}

fn run_oras_in(directory: &Path, arguments: &[OsString]) -> Result<(), PreparedArtifactError> {
    let output = oras_command(arguments)?
        .current_dir(directory)
        .output()
        .map_err(PreparedArtifactError::Launch)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(classify_oras(arguments, output))
    }
}

fn run_oras_stdout(arguments: &[OsString]) -> Result<Vec<u8>, PreparedArtifactError> {
    let output = oras(arguments)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(classify_oras(arguments, output))
    }
}

fn oras(arguments: &[OsString]) -> Result<Output, PreparedArtifactError> {
    oras_command(arguments)?
        .output()
        .map_err(PreparedArtifactError::Launch)
}

fn oras_command(arguments: &[OsString]) -> Result<Command, PreparedArtifactError> {
    let mut command = Command::new("oras");
    command.args(arguments).env_remove("ORAS_CACHE");
    Ok(command)
}

fn admit_oras_version() -> Result<(), PreparedArtifactError> {
    let output = Command::new("oras")
        .arg("version")
        .output()
        .map_err(PreparedArtifactError::Launch)?;
    if !output.status.success() {
        return Err(PreparedArtifactError::Oras {
            command: "oras version".to_owned(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let admitted = stdout.lines().any(|line| {
        line.trim() == ORAS_VERSION
            || line
                .trim()
                .strip_prefix("Version:")
                .is_some_and(|version| version.trim() == ORAS_VERSION)
    });
    if !admitted {
        return Err(PreparedArtifactError::Invalid(format!(
            "oras must be exactly {ORAS_VERSION}, observed {}",
            stdout.trim()
        )));
    }
    Ok(())
}

fn classify_oras(arguments: &[OsString], output: Output) -> PreparedArtifactError {
    let command = format!(
        "oras {}",
        arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.to_ascii_lowercase().contains("not found") {
        PreparedArtifactError::OrasMissing(command)
    } else {
        PreparedArtifactError::Oras {
            command,
            status: output.status.code(),
            stderr,
        }
    }
}

fn sha256_file(path: &Path) -> Result<String, PreparedArtifactError> {
    let bytes = fs::read(path).map_err(|source| PreparedArtifactError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn file_name(path: &Path) -> Result<String, PreparedArtifactError> {
    path.file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            PreparedArtifactError::Invalid("payload has no canonical UTF-8 basename".to_owned())
        })
}

fn slash_path(path: &Path) -> Result<String, PreparedArtifactError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(PreparedArtifactError::Invalid(
                "inventory path is not canonical".to_owned(),
            ));
        };
        parts.push(part.to_str().ok_or_else(|| {
            PreparedArtifactError::Invalid("inventory path is not UTF-8".to_owned())
        })?);
    }
    Ok(parts.join("/"))
}

fn admit_repository(value: &str) -> Result<(), PreparedArtifactError> {
    if !value.starts_with("ghcr.io/")
        || value.contains(['@', ':'])
        || value.split('/').any(|part| part.is_empty())
    {
        return Err(PreparedArtifactError::Invalid(
            "prepared repository must be one canonical ghcr.io path".to_owned(),
        ));
    }
    Ok(())
}

fn admit_tag(value: &str) -> Result<(), PreparedArtifactError> {
    if value.len() != DIGEST_HEX_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PreparedArtifactError::Invalid(
            "prepared discovery tag must be one lowercase build-key digest".to_owned(),
        ));
    }
    Ok(())
}

fn admit_tag_reference(value: &str) -> Result<(), PreparedArtifactError> {
    let (repository, tag) = value.rsplit_once(':').ok_or(PreparedArtifactError::Usage)?;
    admit_repository(repository)?;
    admit_tag(tag)
}

fn utf8<'a>(field: &str, value: &'a OsStr) -> Result<&'a str, PreparedArtifactError> {
    value
        .to_str()
        .ok_or_else(|| PreparedArtifactError::Invalid(format!("{field} must be UTF-8")))
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, PreparedArtifactError> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(PreparedArtifactError::Random)?;
        let path = std::env::temp_dir().join(format!("spice-prepared-{}", hex(&random)));
        fs::create_dir(&path).map_err(|source| PreparedArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

/// Prepared-artifact command failure.
#[derive(Debug)]
pub enum PreparedArtifactError {
    /// The command shape was not admitted.
    Usage,
    /// A manifest or reference violated the common contract.
    Invalid(String),
    /// A deterministic build-key locator already names different bytes.
    Conflict {
        /// Mutable discovery reference that conflicted.
        reference: String,
        /// Digest this publication would produce.
        expected: String,
        /// Digest already selected by the locator.
        observed: String,
    },
    /// One required artifact was absent from GHCR.
    OrasMissing(String),
    /// ORAS refused the requested registry operation.
    Oras {
        /// Exact command.
        command: String,
        /// Exit status, absent on signal termination.
        status: Option<i32>,
        /// Bounded diagnostic text.
        stderr: String,
    },
    /// ORAS was unavailable.
    Launch(io::Error),
    /// A local path could not be read or written.
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// JSON input could not be decoded.
    Decode(serde_json::Error),
    /// Canonical JSON output could not be encoded.
    Encode(serde_json::Error),
    /// Scratch-name entropy was unavailable.
    Random(getrandom::Error),
    /// Command output could not be written.
    Output(io::Error),
}

impl fmt::Display for PreparedArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => formatter.write_str("usage: prepared-artifact <build-key BUILD_KEY.json|manifest KIND PLATFORM BUILD_KEY.json MEDIA_TYPE PAYLOAD INVENTORY_ROOT RUNTIME|- MANIFEST.json|publish REPOSITORY TAG MANIFEST.json PAYLOAD|resolve REPOSITORY:TAG BUILD_KEY|fetch REPOSITORY@DIGEST BUILD_KEY OUT|pack ROOT ARCHIVE|admit-tree MANIFEST.json ROOT>"),
            Self::Invalid(message) => formatter.write_str(message),
            Self::Conflict { reference, expected, observed } => write!(formatter, "prepared locator {reference} conflicts: expected {expected}, observed {observed}"),
            Self::OrasMissing(command) => write!(formatter, "prepared artifact is absent: `{command}`"),
            Self::Oras { command, status, stderr } => write!(formatter, "`{command}` failed with {status:?}: {stderr}"),
            Self::Launch(error) => write!(formatter, "could not launch pinned oras: {error}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Decode(error) => write!(formatter, "prepared JSON is invalid: {error}"),
            Self::Encode(error) => write!(formatter, "could not encode prepared JSON: {error}"),
            Self::Random(error) => write!(formatter, "could not name prepared scratch directory: {error}"),
            Self::Output(error) => write!(formatter, "could not write prepared output: {error}"),
        }
    }
}

impl std::error::Error for PreparedArtifactError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn manifest() -> PreparedManifest {
        let inputs = BTreeMap::from([
            ("cargo_lock".to_owned(), digest('1')),
            ("source".to_owned(), digest('2')),
        ]);
        let build_key_digest = sha256_bytes(&serde_json::to_vec(&inputs).unwrap());
        PreparedManifest {
            schema: SCHEMA,
            kind: ArtifactKind::Bundle,
            platform: Platform::MacosArm64,
            build_key: BuildKey {
                digest: build_key_digest,
                inputs,
            },
            payload: Payload {
                path: "candidate.tar.gz".to_owned(),
                media_type: "application/gzip".to_owned(),
                bytes: 3,
                digest: digest('3'),
            },
            runtime: None,
            files: vec![FileEntry {
                path: "bin/spice".to_owned(),
                bytes: 4,
                digest: digest('4'),
            }],
        }
    }

    #[test]
    fn common_manifest_admits_bundle_and_image_kinds() {
        let bundle = manifest();
        bundle.admit().unwrap();
        let mut image = bundle.clone();
        image.kind = ArtifactKind::Image;
        image.platform = Platform::LinuxAmd64;
        image.runtime = Some(digest('5'));
        image.admit().unwrap();
    }

    #[test]
    fn build_key_covers_complete_named_input_set() {
        let mut changed = manifest();
        changed
            .build_key
            .inputs
            .insert("vendor".to_owned(), digest('5'));
        assert!(changed.admit().is_err());
    }

    #[test]
    fn inventory_order_and_paths_are_canonical() {
        let mut changed = manifest();
        changed.files.push(FileEntry {
            path: "../escape".to_owned(),
            bytes: 1,
            digest: digest('5'),
        });
        assert!(changed.admit().is_err());
    }

    #[test]
    fn discovery_accepts_only_build_key_tags() {
        assert!(admit_tag(&"a".repeat(64)).is_ok());
        assert!(admit_tag("latest").is_err());
        assert!(
            admit_tag_reference(&format!(
                "ghcr.io/infimalabs/spice/prepared:{}",
                "a".repeat(64)
            ))
            .is_ok()
        );
        assert!(admit_tag_reference("ghcr.io/infimalabs/spice/prepared:latest").is_err());
    }

    #[test]
    fn top_level_descriptor_requires_the_prepared_artifact_type() {
        let descriptor = serde_json::json!({
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "artifactType": ARTIFACT_MEDIA_TYPE,
            "digest": digest('a'),
            "size": 17,
            "annotations": {},
        });
        let bytes = serde_json::to_vec(&descriptor).unwrap();
        assert_eq!(
            admit_manifest_descriptor(&bytes).unwrap().digest,
            digest('a')
        );

        for field in ["artifactType", "mediaType"] {
            let mut changed = descriptor.clone();
            changed.as_object_mut().unwrap().remove(field);
            assert!(admit_manifest_descriptor(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
        let mut wrong_type = descriptor.clone();
        wrong_type["artifactType"] = serde_json::json!("application/vnd.example.wrong");
        assert!(admit_manifest_descriptor(&serde_json::to_vec(&wrong_type).unwrap()).is_err());
        let mut changed = descriptor;
        changed["unknown"] = serde_json::json!("field");
        assert!(admit_manifest_descriptor(&serde_json::to_vec(&changed).unwrap()).is_err());
    }

    #[test]
    fn prepared_bundle_bytes_are_reproducible_and_unpack_to_the_inventory() {
        let scratch = Scratch::new().unwrap();
        let root = scratch.path.join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("candidate"), b"exact bytes").unwrap();
        let first = scratch.path.join("first.tar.gz");
        let second = scratch.path.join("second.tar.gz");

        pack_bundle(&root, &first).unwrap();
        pack_bundle(&root, &second).unwrap();
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());

        let unpacked = scratch.path.join("unpacked");
        fs::create_dir(&unpacked).unwrap();
        let archive = fs::File::open(first).unwrap();
        Archive::new(GzDecoder::new(archive))
            .unpack(&unpacked)
            .unwrap();
        assert_eq!(inventory(&unpacked).unwrap(), inventory(&root).unwrap());
    }
}
