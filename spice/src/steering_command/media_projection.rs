use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sill::authority;
use wire::{AgentMediaReference, CanonicalUuid, Digest};

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_PROJECTION_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug)]
pub(crate) struct Projection {
    lane: CanonicalUuid,
    reference: AgentMediaReference,
    path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    directive: CanonicalUuid,
    ordinal: u8,
    digest: Digest,
    bytes: u64,
    content_type: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RenderedProjection<'a> {
    lane: CanonicalUuid,
    directive: CanonicalUuid,
    ordinal: u8,
    digest: &'a Digest,
    bytes: u64,
    content_type: &'a str,
    standing: &'static str,
    path: &'a str,
}

impl Projection {
    pub(crate) const fn reference(&self) -> &AgentMediaReference {
        &self.reference
    }

    pub(crate) fn encoded(&self) -> Result<Vec<u8>, String> {
        wire::encode(&RenderedProjection {
            lane: self.lane,
            directive: self.reference.directive(),
            ordinal: self.reference.ordinal(),
            digest: self.reference.digest(),
            bytes: self.reference.bytes(),
            content_type: self.reference.content_type(),
            standing: "live",
            path: &self.path,
        })
        .map_err(|error| error.to_string())
    }
}

/// Each directive's published attachments and the records now advertised.
type Published = (BTreeMap<CanonicalUuid, Vec<Projection>>, Vec<Record>);

/// Publishes every pending attachment into `occupant`'s private tree under
/// `lane`, naming where each one is readable and the records now advertised.
///
/// Nothing outside that occupant's tree is touched, so two occupants of one
/// lane can never publish over or remove each other's paths.
pub(crate) fn publish(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    pending: &[wire::intent::PendingDirective],
    root: &Path,
) -> Result<Published, String> {
    let mut desired = Vec::new();
    let mut projected = BTreeMap::new();
    for directive in pending {
        let references =
            wire::media::agent_media_references(directive.directive, &directive.attachments)
                .map_err(|error| {
                    format!(
                        "directive {} media is invalid: {error}",
                        directive.directive
                    )
                })?;
        let mut items = Vec::with_capacity(references.len());
        for (reference, attachment) in references.into_iter().zip(&directive.attachments) {
            let bytes = attachment.decoded_bytes().map_err(|error| {
                format!(
                    "directive {} attachment {} is undecodable: {error}",
                    directive.directive,
                    reference.ordinal(),
                )
            })?;
            let record = Record::from_reference(&reference);
            let path = projection_path(root, lane, occupant, &record)?;
            publish_verified(&path, &bytes)?;
            let path = path
                .to_str()
                .ok_or_else(|| "steering media projection path is not valid UTF-8".to_owned())?;
            if path.len() > MAX_PROJECTION_PATH_BYTES {
                return Err(format!(
                    "steering media projection path is {} bytes over the {} byte limit",
                    path.len(),
                    MAX_PROJECTION_PATH_BYTES,
                ));
            }
            desired.push(record);
            items.push(Projection {
                lane,
                reference,
                path: path.to_owned(),
            });
        }
        projected.insert(directive.directive, items);
    }
    Ok((projected, desired))
}

/// Removes the projection of every record in `records` from `occupant`'s tree.
pub(crate) fn retire<'a>(
    root: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    records: impl IntoIterator<Item = &'a Record>,
) -> Result<(), String> {
    for record in records {
        remove_projection(&record_path(root, lane, occupant, record)?)?;
    }
    Ok(())
}

/// Retires everything under `lane` except `occupant`'s own tree.
///
/// A lane has one occupant at a time, so a seated occupant owns no path any
/// other occupant advertised.
pub(crate) fn retire_other_occupants(
    root: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> Result<(), String> {
    let path = spice::repository::lane_media(root, lane);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "steering media lane {} is not a directory",
                path.display(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("steering media lane is unreadable: {error}")),
    }
    let own = occupant.to_string();
    let entries = fs::read_dir(&path)
        .map_err(|error| format!("steering media lane is unreadable: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("steering media lane is unreadable: {error}"))?;
        if entry.file_name().to_str() == Some(own.as_str()) {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|error| format!("steering media lane is unreadable: {error}"))?;
        let removed = if kind.is_dir() {
            fs::remove_dir_all(entry.path())
        } else {
            fs::remove_file(entry.path())
        };
        removed.map_err(|error| {
            format!("another occupant's steering media could not be retired: {error}")
        })?;
    }
    Ok(())
}

impl Record {
    fn from_reference(reference: &AgentMediaReference) -> Self {
        Self {
            directive: reference.directive(),
            ordinal: reference.ordinal(),
            digest: reference.digest().clone(),
            bytes: reference.bytes(),
            content_type: reference.content_type().to_owned(),
        }
    }
}

/// File extension each projectable attachment type is published under.
const EXTENSIONS: [(&str, &str); 4] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

/// Whether `path` names a file in `lane`'s projection layout.
///
/// The layout is
/// `<root>/<lane>/<occupant>/<directive>/<ordinal>-<digest>.<extension>`, so a
/// lane reading one of these files is looking at an attachment the operator
/// sent it, not at media the lane produced.
pub(crate) fn names_projection(path: &str, lane: CanonicalUuid) -> bool {
    fn name(path: Option<&Path>) -> Option<&str> {
        path.and_then(Path::file_name)
            .and_then(|name| name.to_str())
    }
    fn identity(path: Option<&Path>) -> bool {
        name(path).is_some_and(|name| name.parse::<CanonicalUuid>().is_ok())
    }
    let path = Path::new(path);
    let directive = path.parent();
    let occupant = directive.and_then(Path::parent);
    let media = occupant.and_then(Path::parent);
    let owner = media.and_then(Path::parent);
    let file = name(Some(path)).and_then(|file| {
        let (ordinal, rest) = file.split_once('-')?;
        let (digest, extension) = rest.rsplit_once('.')?;
        Some((ordinal, digest, extension))
    });
    file.is_some_and(|(ordinal, digest, extension)| {
        ordinal.parse::<u8>().is_ok()
            && digest.parse::<Digest>().is_ok()
            && EXTENSIONS.iter().any(|(_, known)| *known == extension)
    }) && identity(directive)
        && identity(occupant)
        && name(media) == Some("media")
        && name(owner) == Some(lane.to_string().as_str())
}

/// Where `record` lives in `occupant`'s tree, its directories made private.
fn projection_path(
    root: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    record: &Record,
) -> Result<PathBuf, String> {
    let file = file_name(record)?;
    let lane = root.join(lane.to_string());
    let media = lane.join("media");
    let occupant = media.join(occupant.to_string());
    let directive = occupant.join(record.directive.to_string());
    for directory in [root, &lane] {
        ensure_owned_directory(directory)?;
    }
    for directory in [&media, &occupant, &directive] {
        ensure_private_directory(directory)?;
    }
    Ok(directive.join(file))
}

/// Where `record` lives in `occupant`'s tree, creating nothing.
fn record_path(
    root: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    record: &Record,
) -> Result<PathBuf, String> {
    Ok(spice::repository::lane_media(root, lane)
        .join(occupant.to_string())
        .join(record.directive.to_string())
        .join(file_name(record)?))
}

fn file_name(record: &Record) -> Result<String, String> {
    let Some((_, extension)) = EXTENSIONS
        .iter()
        .find(|(content_type, _)| *content_type == record.content_type)
    else {
        return Err(format!(
            "steering media projection type {:?} is unsupported",
            record.content_type
        ));
    };
    Ok(format!(
        "{}-{}.{}",
        record.ordinal, record.digest, extension
    ))
}

fn publish_verified(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !owned_file(&metadata) => {
            return Err(format!(
                "steering media projection {} is not a regular file",
                path.display(),
            ));
        }
        Ok(_) if private_mode(path, FILE_MODE)? => {
            let observed = fs::read(path).map_err(|error| {
                format!("published steering media bytes are unreadable: {error}")
            })?;
            if observed == bytes {
                return Ok(());
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("steering media projection is unreadable: {error}")),
    }
    atomic::Destination::new(path)
        .map_err(|error| format!("steering media projection is unavailable: {error}"))?
        .publish(|staged| staged.write_all(bytes))
        .map_err(|error| format!("steering media projection is unpublished: {error}"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("published steering media is unreadable: {error}"))?;
    if !owned_file(&metadata) || !private_mode(path, FILE_MODE)? {
        return Err(format!(
            "published steering media {} is not a private regular file",
            path.display(),
        ));
    }
    let observed = fs::read(path)
        .map_err(|error| format!("published steering media bytes are unreadable: {error}"))?;
    if observed != bytes {
        return Err(format!(
            "published steering media {} failed exact byte verification",
            path.display(),
        ));
    }
    Ok(())
}

fn remove_projection(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if owned_file(&metadata) => fs::remove_file(path)
            .map_err(|error| format!("obsolete steering media could not be removed: {error}")),
        Ok(_) => Err(format!(
            "obsolete steering media {} is not a regular file",
            path.display(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("obsolete steering media is unreadable: {error}")),
    }
}

fn ensure_owned_directory(path: &Path) -> Result<(), String> {
    match authority::create_private_directory(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "steering media directory {} is unavailable: {error}",
                path.display(),
            ));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("steering media directory is unreadable: {error}"))?;
    if !metadata.is_dir() || authority::is_link_like(&metadata) {
        return Err(format!(
            "steering media directory {} is not an owned directory",
            path.display(),
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match authority::create_private_directory(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "steering media directory {} is unavailable: {error}",
                path.display(),
            ));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("steering media directory is unreadable: {error}"))?;
    if !metadata.is_dir()
        || authority::is_link_like(&metadata)
        || !private_mode(path, DIRECTORY_MODE)?
    {
        return Err(format!(
            "steering media directory {} is not a private directory",
            path.display(),
        ));
    }
    Ok(())
}

/// Whether unfollowed `metadata` names a regular file this store owns rather
/// than a link or reparse point whose content lives elsewhere.
fn owned_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && !authority::is_link_like(metadata)
}

fn private_mode(path: &Path, expected: u32) -> Result<bool, String> {
    authority::owner_mode(path)
        .map(|mode| mode == expected)
        .map_err(|error| {
            format!(
                "steering media {} mode is unreadable: {error}",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANE: &str = "0a2f5b6c-1d3e-4f50-8a91-b2c3d4e5f607";
    const OCCUPANT: &str = "3d5b8e9f-4061-4273-9d24-e5f607182930";
    const DIRECTIVE: &str = "1b3f6c7d-2e4f-4051-9b02-c3d4e5f60718";
    const PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgo=";

    #[test]
    fn non_regular_projection_substitution_refuses_without_replacement() {
        let root = std::env::temp_dir().join(format!(
            "spice-steering-media-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        ));
        let lane: CanonicalUuid = LANE.parse().expect("fixture lane");
        let occupant: CanonicalUuid = OCCUPANT.parse().expect("fixture occupant");
        let pending = [directive()];
        let (projected, _) =
            publish(lane, occupant, &pending, &root).expect("initial projection publishes");
        let path =
            PathBuf::from(&projected[&DIRECTIVE.parse().expect("fixture directive")][0].path);
        fs::remove_file(&path).expect("remove initial projection");
        fs::create_dir(&path).expect("substitute a directory");

        publish(lane, occupant, &pending, &root).expect_err("non-regular projection must refuse");
        assert!(path.is_dir(), "refusal replaced the substituted path");
        fs::remove_dir_all(root).expect("remove projection fixture");
    }

    fn directive() -> wire::intent::PendingDirective {
        serde_json::from_value(serde_json::json!({
            "directive": DIRECTIVE,
            "key": "1kMdbx1xx",
            "text": "inspect the capture",
            "attachments": [{
                "name": "capture.png",
                "content_type": "image/png",
                "data_url": PNG_DATA_URL,
            }],
            "from": null,
            "sender": null,
            "authored": "2026-08-30T09:00:00.000Z",
            "stage": "published",
            "expires": "2026-08-30T10:00:00.000Z",
        }))
        .expect("projection directive fixture admits")
    }
}
