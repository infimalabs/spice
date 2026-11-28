//! Admission for a layered proof image stored directly in the prepared package.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;

use serde::Deserialize;

use super::{
    OciDescriptor, PreparedArtifactError, admit_digest, admit_oras_version, admit_repository,
    admit_tag, optional_descriptor, run_oras_stdout, sha256_bytes, utf8,
};

const OCI_IMAGE_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const DOCKER_IMAGE_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const OCI_IMAGE_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const DOCKER_IMAGE_CONFIG: &str = "application/vnd.docker.container.image.v1+json";

pub(super) fn resolve(
    reference: &OsStr,
    expected_build_key: &OsStr,
    optional: bool,
    output: &mut impl Write,
) -> Result<(), PreparedArtifactError> {
    admit_oras_version()?;
    let reference = utf8("reference", reference)?;
    let (repository, exact_digest) = admit_image_reference(reference)?;
    let expected_build_key = utf8("build key", expected_build_key)?;
    admit_digest("expected build key", expected_build_key)?;
    let descriptor = if optional {
        let Some(descriptor) = optional_descriptor(image_descriptor(reference))? else {
            return writeln!(output, "absent").map_err(PreparedArtifactError::Output);
        };
        descriptor
    } else {
        image_descriptor(reference)?
    };
    if exact_digest.is_some_and(|digest| descriptor.digest != digest) {
        return Err(PreparedArtifactError::Invalid(
            "resolved proof image manifest differs from the requested digest".to_owned(),
        ));
    }
    let resolution = resolve_descriptor(repository, expected_build_key, &descriptor)?;
    writeln!(
        output,
        "resolved {} {} {}",
        descriptor.digest, resolution.identity, resolution.identity
    )
    .map_err(PreparedArtifactError::Output)
}

struct ImageResolution {
    identity: String,
}

fn image_descriptor(reference: &str) -> Result<OciDescriptor, PreparedArtifactError> {
    let bytes = run_oras_stdout(&[
        "manifest".into(),
        "fetch".into(),
        "--descriptor".into(),
        reference.into(),
    ])?;
    let descriptor: OciDescriptor =
        serde_json::from_slice(&bytes).map_err(PreparedArtifactError::Decode)?;
    if !image_manifest_media_type(&descriptor.media_type) {
        return Err(PreparedArtifactError::Invalid(
            "OCI descriptor does not name one layered proof image".to_owned(),
        ));
    }
    admit_digest("proof image manifest", &descriptor.digest)?;
    Ok(descriptor)
}

fn resolve_descriptor(
    repository: &str,
    expected_build_key: &str,
    descriptor: &OciDescriptor,
) -> Result<ImageResolution, PreparedArtifactError> {
    let manifest = run_oras_stdout(&[
        "manifest".into(),
        "fetch".into(),
        format!("{repository}@{}", descriptor.digest).into(),
    ])?;
    let image: ImageManifest =
        serde_json::from_slice(&manifest).map_err(PreparedArtifactError::Decode)?;
    let config = run_oras_stdout(&[
        "blob".into(),
        "fetch".into(),
        "--output".into(),
        "-".into(),
        format!("{repository}@{}", image.config.digest).into(),
    ])?;
    admit_image(descriptor, expected_build_key, &manifest, &config)
}

fn admit_image_reference(value: &str) -> Result<(&str, Option<&str>), PreparedArtifactError> {
    if let Some((repository, digest)) = value.rsplit_once('@') {
        admit_repository(repository)?;
        admit_digest("proof image manifest", digest)?;
        return Ok((repository, Some(digest)));
    }
    let (repository, tag) = value.rsplit_once(':').ok_or(PreparedArtifactError::Usage)?;
    admit_repository(repository)?;
    let build_key = tag
        .strip_prefix("proof-")
        .ok_or(PreparedArtifactError::Usage)?;
    admit_tag(build_key)?;
    Ok((repository, None))
}

fn admit_image(
    descriptor: &OciDescriptor,
    expected_build_key: &str,
    manifest_bytes: &[u8],
    config_bytes: &[u8],
) -> Result<ImageResolution, PreparedArtifactError> {
    if !image_manifest_media_type(&descriptor.media_type)
        || descriptor.size != u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX)
        || descriptor.digest != sha256_bytes(manifest_bytes)
    {
        return Err(PreparedArtifactError::Invalid(
            "proof image manifest descriptor differs from its bytes".to_owned(),
        ));
    }
    let manifest: ImageManifest =
        serde_json::from_slice(manifest_bytes).map_err(PreparedArtifactError::Decode)?;
    if manifest.schema_version != 2
        || manifest.media_type != descriptor.media_type
        || !image_config_media_type(&manifest.config.media_type)
        || manifest.layers.is_empty()
    {
        return Err(PreparedArtifactError::Invalid(
            "prepared proof image is not one layered OCI image".to_owned(),
        ));
    }
    admit_descriptor("proof image config", &manifest.config)?;
    for layer in &manifest.layers {
        admit_descriptor("proof image layer", layer)?;
    }
    if manifest.config.size != u64::try_from(config_bytes.len()).unwrap_or(u64::MAX)
        || manifest.config.digest != sha256_bytes(config_bytes)
    {
        return Err(PreparedArtifactError::Invalid(
            "proof image config descriptor differs from its bytes".to_owned(),
        ));
    }
    let config: ImageConfiguration =
        serde_json::from_slice(config_bytes).map_err(PreparedArtifactError::Decode)?;
    if config.architecture != "amd64" || config.os != "linux" {
        return Err(PreparedArtifactError::Invalid(format!(
            "prepared proof image platform is {}/{}, expected linux/amd64",
            config.os, config.architecture
        )));
    }
    let observed_build_key = config
        .config
        .labels
        .get(vendor_set::PROOF_IMAGE_BUILD_KEY_LABEL)
        .ok_or_else(|| {
            PreparedArtifactError::Invalid("proof image omits its prepared build key".to_owned())
        })?;
    if observed_build_key != expected_build_key {
        return Err(PreparedArtifactError::Invalid(format!(
            "proof image build key is {observed_build_key}, expected {expected_build_key}"
        )));
    }
    let vendor_set = config
        .config
        .labels
        .get(vendor_set::VENDOR_SET_LABEL)
        .ok_or_else(|| {
            PreparedArtifactError::Invalid("proof image omits its vendor set".to_owned())
        })?;
    admit_digest("proof image vendor set", vendor_set)?;
    Ok(ImageResolution {
        identity: manifest.config.digest,
    })
}

fn admit_descriptor(field: &str, descriptor: &OciDescriptor) -> Result<(), PreparedArtifactError> {
    admit_digest(field, &descriptor.digest)?;
    if descriptor.size == 0 || descriptor.media_type.is_empty() {
        return Err(PreparedArtifactError::Invalid(format!(
            "{field} descriptor must name nonempty typed bytes"
        )));
    }
    Ok(())
}

fn image_manifest_media_type(value: &str) -> bool {
    matches!(value, OCI_IMAGE_MANIFEST | DOCKER_IMAGE_MANIFEST)
}

fn image_config_media_type(value: &str) -> bool {
    matches!(value, OCI_IMAGE_CONFIG | DOCKER_IMAGE_CONFIG)
}

#[derive(Deserialize)]
struct ImageManifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u16,
    #[serde(rename = "mediaType")]
    media_type: String,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[derive(Deserialize)]
struct ImageConfiguration {
    architecture: String,
    os: String,
    config: RuntimeConfiguration,
}

#[derive(Default, Deserialize)]
struct RuntimeConfiguration {
    #[serde(default, rename = "Labels")]
    labels: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const BUILD_KEY: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const VENDOR_SET: &str =
        "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[test]
    fn image_references_admit_only_proof_tags_or_exact_manifests() {
        let tag = format!(
            "ghcr.io/infimalabs/spice/prepared:proof-{}",
            BUILD_KEY.strip_prefix("sha256:").unwrap()
        );
        let (repository, digest) = admit_image_reference(&tag).unwrap();
        assert_eq!(repository, "ghcr.io/infimalabs/spice/prepared");
        assert_eq!(digest, None);

        let exact = format!("ghcr.io/infimalabs/spice/prepared@{VENDOR_SET}");
        let (repository, digest) = admit_image_reference(&exact).unwrap();
        assert_eq!(repository, "ghcr.io/infimalabs/spice/prepared");
        assert_eq!(digest, Some(VENDOR_SET));
        assert!(
            admit_image_reference(
                "ghcr.io/infimalabs/spice/prepared:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .is_err()
        );
    }

    #[test]
    fn layered_image_admission_binds_build_key_platform_and_identity() {
        let config = serde_json::to_vec(&json!({
            "architecture": "amd64",
            "os": "linux",
            "config": {"Labels": {
                vendor_set::PROOF_IMAGE_BUILD_KEY_LABEL: BUILD_KEY,
                vendor_set::VENDOR_SET_LABEL: VENDOR_SET,
            }},
        }))
        .unwrap();
        let config_digest = sha256_bytes(&config);
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_IMAGE_MANIFEST,
            "config": {
                "mediaType": OCI_IMAGE_CONFIG,
                "digest": config_digest,
                "size": config.len(),
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": VENDOR_SET,
                "size": 1,
            }],
        }))
        .unwrap();
        let descriptor = OciDescriptor {
            media_type: OCI_IMAGE_MANIFEST.to_owned(),
            digest: sha256_bytes(&manifest),
            size: u64::try_from(manifest.len()).unwrap(),
            annotations: BTreeMap::new(),
            artifact_type: None,
        };

        let resolution = admit_image(&descriptor, BUILD_KEY, &manifest, &config).unwrap();
        assert_eq!(resolution.identity, config_digest);
        let error = admit_image(
            &descriptor,
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            &manifest,
            &config,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("build key"));
    }
}
