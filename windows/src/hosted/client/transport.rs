//! Challenge-bound direct delivery of one canonical source archive.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::source::PreparedSource;
use super::{WindowsHostedError, command, command_with_input, successful, successful_text};

const RELEASE_PREFIX: &str = "hosted-archive-";
const ASSET_NAME: &str = "source.tar";

/// Provider state carrying one immutable source archive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Transport {
    pub(super) release_id: u64,
    pub(super) asset_id: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Release {
    pub(super) id: u64,
    pub(super) tag_name: String,
    draft: bool,
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Serialize)]
struct CreateRelease<'a> {
    tag_name: &'a str,
    name: &'a str,
    draft: bool,
    prerelease: bool,
    generate_release_notes: bool,
}

pub(super) fn release_name(challenge: &str) -> String {
    format!("{RELEASE_PREFIX}{challenge}")
}

/// Upload the exact archive as one draft-release asset. Draft releases do not
/// create their named Git tag; the name is only the stable provider lookup key
/// attachment re-derives from the challenge.
pub(super) fn create(
    root: &Path,
    repository: &str,
    challenge: &str,
    source: &PreparedSource,
) -> Result<Transport, WindowsHostedError> {
    let name = release_name(challenge);
    let release = match command_with_input(
        root,
        "gh",
        [
            OsString::from("api"),
            OsString::from("--method=POST"),
            OsString::from(format!("repos/{repository}/releases")),
            OsString::from("--input=-"),
        ],
        serde_json::to_vec(&CreateRelease {
            tag_name: &name,
            name: &name,
            draft: true,
            prerelease: true,
            generate_release_notes: false,
        })
        .map_err(WindowsHostedError::Encode)?,
    )
    .and_then(|output| decode::<Release>("source transport creation", output))
    {
        Ok(release) => release,
        Err(operation) => {
            return cleanup_named_failure(root, repository, &name, operation);
        }
    };
    if release.id == 0 || !release.draft || release.tag_name != name {
        return cleanup_named_failure(
            root,
            repository,
            &name,
            WindowsHostedError::ProviderResponse(
                "provider created a changed source transport".to_owned(),
            ),
        );
    }
    let upload = command(
        root,
        "gh",
        [
            OsString::from("api"),
            OsString::from("--method=POST"),
            OsString::from(format!(
                "https://uploads.github.com/repos/{repository}/releases/{}/assets?name={ASSET_NAME}",
                release.id
            )),
            OsString::from("--header=Content-Type: application/x-tar"),
            OsString::from("--input"),
            source.archive_path.as_os_str().to_owned(),
        ],
    )
    .and_then(|output| decode::<Asset>("source archive upload", output));
    let asset = match upload {
        Ok(asset) => asset,
        Err(operation) => {
            let cleanup = delete(root, repository, release.id);
            return Err(match cleanup {
                Ok(()) => operation,
                Err(cleanup) => WindowsHostedError::OperationAndCleanup {
                    operation: Box::new(operation),
                    cleanup: Box::new(cleanup),
                },
            });
        }
    };
    if let Err(operation) = admit_asset(source, &asset) {
        let cleanup = delete(root, repository, release.id);
        return Err(match cleanup {
            Ok(()) => operation,
            Err(cleanup) => WindowsHostedError::OperationAndCleanup {
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            },
        });
    }
    Ok(Transport {
        release_id: release.id,
        asset_id: asset.id,
    })
}

/// Find and admit the retained source transport for `challenge`.
pub(super) fn find(
    root: &Path,
    repository: &str,
    challenge: &str,
    source: &PreparedSource,
) -> Result<Transport, WindowsHostedError> {
    let name = release_name(challenge);
    let mut matching = releases(root, repository)?
        .into_iter()
        .filter(|release| release.draft && release.tag_name == name)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(WindowsHostedError::AttachRefused(format!(
            "challenge {challenge} has {} retained source transports",
            matching.len()
        )));
    }
    let release = matching.remove(0);
    if release.assets.len() != 1 {
        return Err(WindowsHostedError::AttachRefused(format!(
            "source transport {} has {} assets",
            release.id,
            release.assets.len()
        )));
    }
    let asset = &release.assets[0];
    admit_asset(source, asset).map_err(|error| {
        WindowsHostedError::AttachRefused(format!(
            "source transport {} differs: {error}",
            release.id
        ))
    })?;
    Ok(Transport {
        release_id: release.id,
        asset_id: asset.id,
    })
}

pub(super) fn releases(root: &Path, repository: &str) -> Result<Vec<Release>, WindowsHostedError> {
    let pages: Vec<Vec<Release>> = decode(
        "source transport inventory",
        command(
            root,
            "gh",
            [
                OsString::from("api"),
                OsString::from("--paginate"),
                OsString::from("--slurp"),
                OsString::from(format!("repos/{repository}/releases?per_page=100")),
            ],
        )?,
    )?;
    Ok(pages.into_iter().flatten().collect())
}

fn cleanup_named_failure<T>(
    root: &Path,
    repository: &str,
    name: &str,
    operation: WindowsHostedError,
) -> Result<T, WindowsHostedError> {
    let cleanup = releases(root, repository).and_then(|releases| {
        for release in releases
            .into_iter()
            .filter(|release| release.tag_name == name)
        {
            delete(root, repository, release.id)?;
        }
        Ok(())
    });
    Err(match cleanup {
        Ok(()) => operation,
        Err(cleanup) => WindowsHostedError::OperationAndCleanup {
            operation: Box::new(operation),
            cleanup: Box::new(cleanup),
        },
    })
}

/// Delete one source transport, accepting an already-absent release only after
/// a successful inventory proves its id is gone.
pub(super) fn delete(
    root: &Path,
    repository: &str,
    release_id: u64,
) -> Result<(), WindowsHostedError> {
    let deletion = command(
        root,
        "gh",
        [
            OsString::from("api"),
            OsString::from("--method=DELETE"),
            OsString::from(format!("repos/{repository}/releases/{release_id}")),
        ],
    )?;
    if deletion.status.success() {
        return Ok(());
    }
    if releases(root, repository)?
        .iter()
        .all(|release| release.id != release_id)
    {
        return Ok(());
    }
    successful("source transport retirement", deletion).map(|_| ())
}

fn admit_asset(source: &PreparedSource, asset: &Asset) -> Result<(), WindowsHostedError> {
    let bytes = fs::metadata(&source.archive_path)
        .map_err(|error| WindowsHostedError::Io {
            path: source.archive_path.clone(),
            source: error,
        })?
        .len();
    admit_asset_metadata(bytes, &source.archive_sha256, asset)
}

fn admit_asset_metadata(
    bytes: u64,
    archive_sha256: &str,
    asset: &Asset,
) -> Result<(), WindowsHostedError> {
    let expected_digest = format!("sha256:{archive_sha256}");
    if asset.id == 0
        || asset.name != ASSET_NAME
        || asset.size != bytes
        || asset.digest.as_deref() != Some(expected_digest.as_str())
    {
        return Err(WindowsHostedError::ProviderResponse(
            "provider source archive asset differs from its local bytes".to_owned(),
        ));
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(
    operation: &'static str,
    output: std::process::Output,
) -> Result<T, WindowsHostedError> {
    let response = successful_text(operation, output)?;
    serde_json::from_str(&response).map_err(WindowsHostedError::Decode)
}

#[cfg(test)]
mod tests {
    use super::{ASSET_NAME, Asset, admit_asset_metadata, release_name};

    const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn asset() -> Asset {
        Asset {
            id: 7,
            name: ASSET_NAME.to_owned(),
            size: 11,
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        }
    }

    #[test]
    fn transport_name_and_asset_bind_the_challenge_and_exact_archive() {
        assert_eq!(
            release_name(CHALLENGE),
            format!("hosted-archive-{CHALLENGE}")
        );
        assert!(admit_asset_metadata(11, &"a".repeat(64), &asset()).is_ok());

        let mut changed = asset();
        changed.size += 1;
        assert!(admit_asset_metadata(11, &"a".repeat(64), &changed).is_err());
        changed = asset();
        changed.digest = Some(format!("sha256:{}", "b".repeat(64)));
        assert!(admit_asset_metadata(11, &"a".repeat(64), &changed).is_err());
    }
}
