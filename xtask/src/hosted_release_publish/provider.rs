use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use super::PublishError;
use super::plan::{CHECKSUMS, PublicationAsset, PublicationPlan, sha256};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseView {
    tag_name: String,
    name: String,
    body: String,
    is_draft: bool,
    is_prerelease: bool,
    assets: Vec<AssetView>,
}

#[derive(Deserialize)]
struct AssetView {
    name: String,
    digest: Option<String>,
    state: String,
}

pub(super) fn publish_and_verify(
    root: &Path,
    operation: &Path,
    plan: &PublicationPlan,
) -> Result<(), PublishError> {
    converge_tag(root, plan)?;
    converge_release(root, operation, plan)?;
    verify_fresh_download(root, operation, plan)
}

fn converge_tag(root: &Path, plan: &PublicationPlan) -> Result<(), PublishError> {
    let reference = format!("refs/tags/{}", plan.tag);
    let peeled = format!("{reference}^{{}}");
    let observed = git(root, &["ls-remote", "origin", &reference, &peeled])?;
    match remote_tag(&observed, &reference, &peeled)? {
        Some(commit) if commit == plan.source_commit => return Ok(()),
        Some(commit) => {
            return Err(PublishError::Refused(format!(
                "remote tag {} names {commit}, not {}",
                plan.tag, plan.source_commit
            )));
        }
        None => {}
    }
    admit_or_create_local_tag(root, plan)?;
    git(
        root,
        &["push", "origin", &format!("{reference}:{reference}")],
    )?;
    let observed = git(root, &["ls-remote", "origin", &reference, &peeled])?;
    if remote_tag(&observed, &reference, &peeled)?.as_deref() == Some(&plan.source_commit) {
        Ok(())
    } else {
        Err(PublishError::Refused(format!(
            "remote tag {} did not converge to {}",
            plan.tag, plan.source_commit
        )))
    }
}

fn admit_or_create_local_tag(root: &Path, plan: &PublicationPlan) -> Result<(), PublishError> {
    let tag_type = command(root, "git", &["cat-file", "-t", &plan.tag], true)?;
    if tag_type.status.success() {
        if text(&tag_type)?.trim() != "tag"
            || git(root, &["rev-parse", &format!("{}^{{commit}}", plan.tag)])? != plan.source_commit
        {
            return Err(PublishError::Refused(format!(
                "local tag {} is not the expected annotated tag",
                plan.tag
            )));
        }
        return Ok(());
    }
    let timestamp = git(root, &["show", "-s", "--format=%ct", &plan.source_commit])?;
    let mut tag = Command::new("git");
    tag.current_dir(root)
        .env("GIT_COMMITTER_NAME", "Spice Release")
        .env("GIT_COMMITTER_EMAIL", "release@infimalabs.com")
        .env("GIT_COMMITTER_DATE", format!("@{timestamp} +0000"))
        .args([
            "tag",
            "-a",
            plan.tag.as_str(),
            plan.source_commit.as_str(),
            "-m",
            &format!("Spice {}", plan.tag),
        ]);
    successful(
        "git",
        tag.output().map_err(|source| PublishError::Launch {
            program: "git".into(),
            source,
        })?,
    )?;
    Ok(())
}

fn remote_tag(output: &str, reference: &str, peeled: &str) -> Result<Option<String>, PublishError> {
    let rows = output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(object, name)| (name, object))
        .collect::<BTreeMap<_, _>>();
    match (rows.get(reference), rows.get(peeled)) {
        (None, None) => Ok(None),
        (Some(_), Some(commit)) => Ok(Some((*commit).to_owned())),
        _ => Err(PublishError::Refused(
            "release tag exists but is not one annotated tag".into(),
        )),
    }
}

fn converge_release(
    root: &Path,
    operation: &Path,
    plan: &PublicationPlan,
) -> Result<(), PublishError> {
    let notes = operation.join("notes.md");
    fs::write(&notes, &plan.notes).map_err(|source| PublishError::Io {
        path: notes.clone(),
        source,
    })?;
    let observed = release_view(root, &plan.tag)?;
    let missing = match observed {
        None => {
            let mut arguments = vec![
                "release".to_owned(),
                "create".to_owned(),
                plan.tag.clone(),
                "--verify-tag".to_owned(),
                "--title".to_owned(),
                plan.tag.clone(),
                "--notes-file".to_owned(),
                notes.to_string_lossy().into_owned(),
            ];
            arguments.extend(plan.assets.iter().map(|asset| {
                plan.asset_path(operation, asset)
                    .to_string_lossy()
                    .into_owned()
            }));
            gh(
                root,
                &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
            )?;
            Vec::new()
        }
        Some(view) => admit_release(view, plan)?,
    };
    if !missing.is_empty() {
        let mut arguments = vec!["release", "upload", plan.tag.as_str()];
        let paths = missing
            .iter()
            .map(|asset| {
                plan.asset_path(operation, asset)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        arguments.extend(paths.iter().map(String::as_str));
        gh(root, &arguments)?;
    }
    let view = release_view(root, &plan.tag)?
        .ok_or_else(|| PublishError::Refused("GitHub Release remained absent".into()))?;
    if admit_release(view, plan)?.is_empty() {
        Ok(())
    } else {
        Err(PublishError::Refused(
            "GitHub Release remained incomplete after upload".into(),
        ))
    }
}

fn admit_release(
    view: ReleaseView,
    plan: &PublicationPlan,
) -> Result<Vec<PublicationAsset>, PublishError> {
    if view.tag_name != plan.tag
        || view.name != plan.tag
        || view.body != plan.notes
        || view.is_draft
        || view.is_prerelease
    {
        return Err(PublishError::Refused(
            "GitHub Release metadata moved from the publication plan".into(),
        ));
    }
    let mut remote = view
        .assets
        .into_iter()
        .map(|asset| (asset.name.clone(), asset))
        .collect::<BTreeMap<_, _>>();
    let mut missing = Vec::new();
    for expected in &plan.assets {
        let Some(observed) = remote.remove(&expected.name) else {
            missing.push(expected.clone());
            continue;
        };
        if observed.state != "uploaded"
            || observed.digest.as_deref() != Some(&format!("sha256:{}", expected.sha256))
        {
            return Err(PublishError::Refused(format!(
                "GitHub Release asset {} moved from the admitted bytes",
                expected.name
            )));
        }
    }
    if let Some(name) = remote.keys().next() {
        return Err(PublishError::Refused(format!(
            "GitHub Release has unexpected asset {name}"
        )));
    }
    Ok(missing)
}

fn release_view(root: &Path, tag: &str) -> Result<Option<ReleaseView>, PublishError> {
    let output = command(
        root,
        "gh",
        &[
            "release",
            "view",
            tag,
            "--json",
            "tagName,name,body,isDraft,isPrerelease,assets",
        ],
        true,
    )?;
    if output.status.success() {
        serde_json::from_slice(&output.stdout)
            .map(Some)
            .map_err(PublishError::Decode)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        if detail.to_ascii_lowercase().contains("release not found") {
            Ok(None)
        } else {
            Err(PublishError::Command {
                program: "gh".into(),
                detail: detail.trim().to_owned(),
            })
        }
    }
}

fn verify_fresh_download(
    root: &Path,
    operation: &Path,
    plan: &PublicationPlan,
) -> Result<(), PublishError> {
    let fresh = FreshDirectory::create(operation)?;
    gh(
        root,
        &[
            "release",
            "download",
            &plan.tag,
            "--dir",
            fresh.path().to_string_lossy().as_ref(),
            "--pattern",
            "*",
        ],
    )?;
    let names = fs::read_dir(fresh.path())
        .map_err(|source| PublishError::Io {
            path: fresh.path().to_path_buf(),
            source,
        })?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|source| PublishError::Io {
            path: fresh.path().to_path_buf(),
            source,
        })?;
    let expected = plan.assets.iter().map(|asset| asset.name.clone()).collect();
    if names != expected {
        return Err(PublishError::Refused(
            "fresh GitHub Release download has a different asset set".into(),
        ));
    }
    for asset in &plan.assets {
        let path = fresh.path().join(&asset.name);
        let bytes = fs::read(&path).map_err(|source| PublishError::Io {
            path: path.clone(),
            source,
        })?;
        if bytes.len() as u64 != asset.bytes || sha256(&bytes) != asset.sha256 {
            return Err(PublishError::Refused(format!(
                "freshly downloaded {} differs from its published checksum",
                asset.name
            )));
        }
    }
    let downloaded = fs::read(fresh.path().join(CHECKSUMS)).map_err(|source| PublishError::Io {
        path: fresh.path().join(CHECKSUMS),
        source,
    })?;
    let admitted = fs::read(
        plan.asset_path(
            operation,
            plan.assets
                .iter()
                .find(|asset| asset.name == CHECKSUMS)
                .expect("plan admits checksums"),
        ),
    )
    .map_err(|source| PublishError::Io {
        path: operation.join("assets").join(CHECKSUMS),
        source,
    })?;
    if downloaded != admitted {
        return Err(PublishError::Refused(
            "fresh checksum document differs from the admitted publication".into(),
        ));
    }
    Ok(())
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, PublishError> {
    text(&successful("git", command(root, "git", arguments, false)?)?)
        .map(|text| text.trim().to_owned())
}

fn gh(root: &Path, arguments: &[&str]) -> Result<String, PublishError> {
    text(&successful("gh", command(root, "gh", arguments, false)?)?)
        .map(|text| text.trim().to_owned())
}

fn command(
    root: &Path,
    program: &str,
    arguments: &[&str],
    allow_failure: bool,
) -> Result<Output, PublishError> {
    let output = Command::new(program)
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|source| PublishError::Launch {
            program: program.to_owned(),
            source,
        })?;
    if allow_failure || output.status.success() {
        Ok(output)
    } else {
        successful(program, output)
    }
}

fn successful(program: &str, output: Output) -> Result<Output, PublishError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(PublishError::Command {
            program: program.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn text(output: &Output) -> Result<String, PublishError> {
    String::from_utf8(output.stdout.clone())
        .map_err(|_| PublishError::Refused("provider output was not UTF-8".into()))
}

struct FreshDirectory(PathBuf);

impl FreshDirectory {
    fn create(operation: &Path) -> Result<Self, PublishError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = operation.join(format!(
            ".download-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).map_err(|source| PublishError::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self(root))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FreshDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::manifest::ProductVersion;

    fn plan() -> PublicationPlan {
        PublicationPlan {
            schema: 1,
            source_commit: "1".repeat(40),
            version: ProductVersion::parse("1.2.3").expect("version"),
            tag: "v1.2.3".to_owned(),
            notes: "# Spice v1.2.3\n".to_owned(),
            assets: vec![
                PublicationAsset {
                    name: "spice-aarch64-apple-darwin".to_owned(),
                    bytes: 3,
                    sha256: "1".repeat(64),
                },
                PublicationAsset {
                    name: CHECKSUMS.to_owned(),
                    bytes: 4,
                    sha256: "2".repeat(64),
                },
            ],
        }
    }

    fn view(plan: &PublicationPlan) -> ReleaseView {
        ReleaseView {
            tag_name: plan.tag.clone(),
            name: plan.tag.clone(),
            body: plan.notes.clone(),
            is_draft: false,
            is_prerelease: false,
            assets: plan
                .assets
                .iter()
                .map(|asset| AssetView {
                    name: asset.name.clone(),
                    digest: Some(format!("sha256:{}", asset.sha256)),
                    state: "uploaded".to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn provider_release_converges_only_with_the_exact_asset_inventory() {
        let plan = plan();
        assert!(
            admit_release(view(&plan), &plan)
                .expect("exact release")
                .is_empty()
        );

        let mut partial = view(&plan);
        partial.assets.pop();
        assert_eq!(
            admit_release(partial, &plan).expect("missing assets may resume"),
            vec![plan.assets[1].clone()]
        );

        let mut moved = view(&plan);
        moved.assets[0].digest = Some(format!("sha256:{}", "f".repeat(64)));
        assert!(matches!(
            admit_release(moved, &plan),
            Err(PublishError::Refused(_))
        ));
    }

    #[test]
    fn annotated_tag_observation_requires_one_peeled_commit() {
        let reference = "refs/tags/v1.2.3";
        let peeled = "refs/tags/v1.2.3^{}";
        let commit = "1".repeat(40);
        let tag = "2".repeat(40);
        let observed = format!("{tag}\t{reference}\n{commit}\t{peeled}\n");
        assert_eq!(
            remote_tag(&observed, reference, peeled).expect("annotated tag"),
            Some(commit)
        );
        assert!(remote_tag(&format!("{tag}\t{reference}\n"), reference, peeled).is_err());
    }
}
