//! Atomic replacement of one task's complete image citation set.

use super::*;

/// One image citation in an atomic task image-set replacement.
pub struct ImageBytes<'a> {
    pub bytes: &'a [u8],
    pub name: &'a str,
    pub media_type: &'a str,
    pub retention: Retention,
}

/// Inputs for replacing every image citation of one task while retaining its
/// non-image citations.
pub struct ReplaceImagesRequest<'a> {
    pub task: &'a TaskStamp,
    pub images: &'a [ImageBytes<'a>],
    pub cited: &'a BTreeSet<Digest>,
    pub created_at: Instant,
}

impl Store {
    /// Replace every image citation of one task through one manifest
    /// publication. The complete requested set and retained non-image set are
    /// admitted before objects are staged; a refusal or publication failure
    /// leaves the preceding manifest authoritative.
    pub fn replace_images(
        &self,
        request: ReplaceImagesRequest<'_>,
    ) -> Result<(Vec<Artifact>, ArtifactBudget), Error> {
        self.replace_images_with_hook(request, || Ok(()))
    }

    pub(super) fn replace_images_with_hook<F>(
        &self,
        request: ReplaceImagesRequest<'_>,
        before_manifest: F,
    ) -> Result<(Vec<Artifact>, ArtifactBudget), Error>
    where
        F: FnOnce() -> Result<(), Error>,
    {
        let _possession = self.acquire()?;
        let task_dir = self.task_dir(request.task);
        create_private_directories(&task_dir)?;
        let current = read_manifest(&task_dir, request.task)?;
        validate_manifest(&task_dir, request.task, &current, true)?;
        if let Some(images) = unchanged_images(&task_dir, &current, request.images) {
            return Ok((images, ArtifactBudget::of(current.artifacts.len())));
        }
        refuse_cited_images(request.task, &current, request.cited)?;
        let (replacement, images) = replacement_manifest(&task_dir, &current, &request)?;
        publish_replacement(
            &task_dir,
            &replacement,
            &images,
            request.images,
            before_manifest,
        )?;
        reclaim_removed_images(&task_dir, &current, &replacement);
        Ok((images, ArtifactBudget::of(replacement.artifacts.len())))
    }
}

fn image_artifacts(manifest: &Manifest) -> impl Iterator<Item = &Artifact> {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.media_type.is_image())
}

fn unchanged_images(
    task_dir: &Path,
    current: &Manifest,
    requested: &[ImageBytes<'_>],
) -> Option<Vec<Artifact>> {
    if image_artifacts(current).count() != requested.len() {
        return None;
    }
    image_artifacts(current)
        .zip(requested)
        .all(|(artifact, image)| {
            artifact.name == artifact_name(image.name)
                && artifact.media_type.as_str() == image.media_type
                && read_cited(
                    &payload_path(task_dir, &artifact.digest),
                    &artifact.id,
                    "payload",
                )
                .is_ok_and(|bytes| bytes == image.bytes)
        })
        .then(|| image_artifacts(current).cloned().collect())
}

fn refuse_cited_images(
    task: &TaskStamp,
    current: &Manifest,
    cited: &BTreeSet<Digest>,
) -> Result<(), Error> {
    let Some(artifact) = image_artifacts(current).find(|artifact| cited.contains(&artifact.digest))
    else {
        return Ok(());
    };
    Err(Error::refused(
        "artifact citation",
        format!(
            "artifact {} of task {} is cited by its room",
            artifact.id, task
        ),
    ))
}

fn replacement_manifest(
    task_dir: &Path,
    current: &Manifest,
    request: &ReplaceImagesRequest<'_>,
) -> Result<(Manifest, Vec<Artifact>), Error> {
    let high = high_water(current);
    let mut replacement = current.clone();
    replacement
        .artifacts
        .retain(|artifact| !artifact.media_type.is_image());
    if replacement
        .artifacts
        .len()
        .saturating_add(request.images.len())
        > MAX_ARTIFACTS_PER_TASK
    {
        return Err(Error::refused(
            "artifact count",
            format!(
                "task {} retains {} non-image artifacts and requests {} images; maximum is {}",
                request.task,
                replacement.artifacts.len(),
                request.images.len(),
                MAX_ARTIFACTS_PER_TASK,
            ),
        ));
    }
    replacement.issued = retained_issued(high, &replacement.artifacts);
    let mut images = Vec::with_capacity(request.images.len());
    for image in request.images {
        let artifact = replacement_artifact(&replacement, image, request.created_at)?;
        replacement.artifacts.push(artifact.clone());
        images.push(artifact);
    }
    replacement.issued = retained_issued(high, &replacement.artifacts);
    validate_manifest_shape(
        request.task,
        &replacement,
        &manifest_path(task_dir).display().to_string(),
    )?;
    Ok((replacement, images))
}

fn replacement_artifact(
    replacement: &Manifest,
    image: &ImageBytes<'_>,
    created_at: Instant,
) -> Result<Artifact, Error> {
    if image.bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(Error::refused(
            "artifact size",
            format!(
                "{} is {} bytes; maximum is {MAX_ARTIFACT_BYTES}",
                image.name,
                image.bytes.len()
            ),
        ));
    }
    let media_type = image
        .media_type
        .parse::<MediaType>()
        .map_err(|error| Error::refused("artifact media type", error.to_string()))?;
    if !media_type.is_image() {
        return Err(Error::refused(
            "artifact media type",
            format!("{} is not an image media type", image.media_type),
        ));
    }
    Ok(Artifact {
        id: next_id(replacement)?,
        name: artifact_name(image.name),
        digest: Digest::compute(Domain::TaskArtifactPayload, image.bytes),
        media_type,
        bytes: count(image.bytes.len(), "artifact byte")?,
        created_at,
        source: SOURCE.to_owned(),
        retention: image.retention,
    })
}

fn publish_replacement<F>(
    task_dir: &Path,
    replacement: &Manifest,
    images: &[Artifact],
    requested: &[ImageBytes<'_>],
    before_manifest: F,
) -> Result<(), Error>
where
    F: FnOnce() -> Result<(), Error>,
{
    let mut staged = Vec::new();
    for (artifact, image) in images.iter().zip(requested) {
        let object = object_dir(task_dir, &artifact.digest);
        let absent = match fs::symlink_metadata(&object) {
            Ok(_) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => {
                cleanup_objects(task_dir, &staged);
                return Err(Error::io("inspect artifact object", object, error));
            }
        };
        if absent {
            staged.push(artifact.digest.clone());
        }
        if let Err(error) = create_private_directories(&object)
            .and_then(|()| publish_object(&object, &artifact.digest, image.bytes))
        {
            cleanup_objects(task_dir, &staged);
            return Err(error);
        }
    }
    if let Err(error) = before_manifest().and_then(|()| publish_manifest(task_dir, replacement)) {
        cleanup_objects(task_dir, &staged);
        return Err(error);
    }
    Ok(())
}

fn reclaim_removed_images(task_dir: &Path, current: &Manifest, replacement: &Manifest) {
    // The manifest is authoritative. Reclamation is best-effort: an error may
    // retain an unreachable object but cannot make the committed set partial.
    for artifact in image_artifacts(current) {
        if !replacement
            .artifacts
            .iter()
            .any(|retained| retained.digest == artifact.digest)
        {
            let _ = remove_object(task_dir, &artifact.digest);
        }
    }
}
