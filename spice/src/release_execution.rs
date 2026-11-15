//! Native effect adapter and durable operation journal for external releases.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest as ShaDigest, Sha256};
use spice::ambient::Ambient;
use wire::release::{Operation, RegistryEvidence, ReleaseObservations, ReleasePlan};
use wire::{Digest, Outcome, Refusal};

const JOURNAL_SCHEMA: u16 = 2;
const VISIBILITY_ATTEMPTS: usize = 8;

enum CorrectnessGateStanding {
    Admitted,
    Refused(String),
}

fn package_failure(error: crate::release_package::PackageFailure) -> dist::EffectError {
    match error {
        crate::release_package::PackageFailure::Moved(detail) => dist::EffectError::Moved(detail),
        crate::release_package::PackageFailure::Environment(detail) => {
            dist::EffectError::Environment(detail)
        }
    }
}

fn run_release_gate(
    root: &Path,
    ambient: &Ambient,
    source_commit: &str,
) -> Result<(), dist::EffectError> {
    run_release_gate_with(root, source_commit, |root, selector| {
        let report = spice::gate::run_strict_at(ambient, root, selector).map_err(|error| {
            if error.is_fault() {
                dist::EffectError::Environment(error.to_string())
            } else {
                dist::EffectError::GateRefused(error.to_string())
            }
        })?;
        if report.admits() {
            Ok(CorrectnessGateStanding::Admitted)
        } else {
            Ok(CorrectnessGateStanding::Refused(
                report.refusal_fold().to_string(),
            ))
        }
    })
}

fn run_release_gate_with(
    root: &Path,
    source_commit: &str,
    runner: impl FnOnce(&Path, gate::Selector) -> Result<CorrectnessGateStanding, dist::EffectError>,
) -> Result<(), dist::EffectError> {
    match runner(root, gate::Selector::revision(source_commit))? {
        CorrectnessGateStanding::Admitted => Ok(()),
        CorrectnessGateStanding::Refused(detail) => Err(dist::EffectError::GateRefused(detail)),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalDocument {
    schema: u16,
    plan: ReleasePlan,
    accepted: Vec<dist::AcceptedOperation>,
    receipts: Vec<dist::OperationReceipt>,
}

/// The schema-independent part of any release journal, read before the typed
/// decode so a predecessor is refused by name and never decoded for execution.
#[derive(Deserialize)]
struct JournalHeader {
    schema: u16,
    plan: JournalHeaderPlan,
}

#[derive(Deserialize)]
struct JournalHeaderPlan {
    requested_version: String,
    release_commit: String,
}

pub(super) struct FileReleaseJournal {
    path: PathBuf,
    document: JournalDocument,
}

impl FileReleaseJournal {
    fn open(root: &Path, plan: &ReleasePlan) -> Result<Self, String> {
        let path = root.join(format!("{}.json", plan.key));
        let document = match fs::read(&path) {
            Ok(bytes) => {
                let header: JournalHeader = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                if header.schema != JOURNAL_SCHEMA {
                    return Err(format!(
                        "release operation journal {} was last written by release v{} at {} \
                         under journal schema {}; this appliance executes only schema \
                         {JOURNAL_SCHEMA}",
                        path.display(),
                        header.plan.requested_version,
                        header.plan.release_commit,
                        header.schema,
                    ));
                }
                let document: JournalDocument =
                    wire::decode(&bytes).map_err(|error| error.to_string())?;
                let canonical = wire::encode(&document)
                    .map_err(|error| format!("journal encoding: {error}"))?;
                if canonical != bytes {
                    return Err("release operation journal is not canonical".to_owned());
                }
                if document.plan != *plan {
                    return Err("release operation journal identity moved".to_owned());
                }
                validate_document(&document)?;
                document
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => JournalDocument {
                schema: JOURNAL_SCHEMA,
                plan: plan.clone(),
                accepted: Vec::new(),
                receipts: Vec::new(),
            },
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        Ok(Self { path, document })
    }

    fn persist(&self) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "release journal has no parent".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        let bytes = wire::encode(&self.document).map_err(|error| error.to_string())?;
        let mut staged = atomic::Destination::new(&self.path)
            .map_err(|error| error.to_string())?
            .stage()
            .map_err(|error| error.to_string())?;
        staged
            .write_all(&bytes)
            .map_err(|error| format!("cannot write {}: {error}", self.path.display()))?;
        staged.commit().map_err(|error| error.to_string())
    }
}

fn validate_document(document: &JournalDocument) -> Result<(), String> {
    dist::verify_plan(&document.plan).map_err(|error| error.to_string())?;
    if document.accepted.len() > document.plan.operations.len()
        || document
            .accepted
            .iter()
            .zip(&document.plan.operations)
            .any(|(accepted, operation)| accepted.operation != *operation)
        || document.receipts.len() > document.accepted.len()
        || document.accepted.len() > document.receipts.len().saturating_add(1)
        || document
            .receipts
            .iter()
            .zip(&document.plan.operations)
            .any(|(receipt, operation)| receipt.operation != *operation)
    {
        return Err("release operation journal is incomplete or reordered".to_owned());
    }
    for (index, accepted) in document.accepted.iter().enumerate() {
        if accepted.plan != document.plan.key
            || document.accepted[..index]
                .iter()
                .any(|prior| prior.operation == accepted.operation)
        {
            return Err("release operation journal has a moved or duplicate acceptance".to_owned());
        }
    }
    for (index, receipt) in document.receipts.iter().enumerate() {
        let accepted = document
            .accepted
            .iter()
            .find(|accepted| accepted.operation == receipt.operation);
        if receipt.plan != document.plan.key
            || document.receipts[..index]
                .iter()
                .any(|prior| prior.operation == receipt.operation)
            || !matches!(accepted, Some(accepted) if accepted.prior == receipt.prior)
        {
            return Err("release operation journal has a moved or duplicate receipt".to_owned());
        }
    }
    Ok(())
}

impl dist::ReleaseJournal for FileReleaseJournal {
    type Error = String;

    fn accepted(
        &mut self,
        plan: &Digest,
        operation: Operation,
    ) -> Result<Option<dist::AcceptedOperation>, Self::Error> {
        Ok(self
            .document
            .accepted
            .iter()
            .find(|row| row.plan == *plan && row.operation == operation)
            .cloned())
    }

    fn receipt(
        &mut self,
        plan: &Digest,
        operation: Operation,
    ) -> Result<Option<dist::OperationReceipt>, Self::Error> {
        Ok(self
            .document
            .receipts
            .iter()
            .find(|row| row.plan == *plan && row.operation == operation)
            .cloned())
    }

    fn accept(&mut self, accepted: &dist::AcceptedOperation) -> Result<(), Self::Error> {
        if self
            .document
            .accepted
            .iter()
            .any(|row| row.operation == accepted.operation)
        {
            return Err(format!(
                "operation {:?} was accepted twice",
                accepted.operation
            ));
        }
        self.document.accepted.push(accepted.clone());
        self.persist()
    }

    fn complete(&mut self, receipt: &dist::OperationReceipt) -> Result<(), Self::Error> {
        if !self
            .document
            .accepted
            .iter()
            .any(|row| row.operation == receipt.operation && row.prior == receipt.prior)
        {
            return Err(format!(
                "operation {:?} completed without its exact accepted prior state",
                receipt.operation
            ));
        }
        if self
            .document
            .receipts
            .iter()
            .any(|row| row.operation == receipt.operation)
        {
            return Err(format!(
                "operation {:?} was receipted twice",
                receipt.operation
            ));
        }
        self.document.receipts.push(receipt.clone());
        self.persist()
    }
}

pub(super) struct SystemEffects<'a> {
    root: PathBuf,
    probe_root: PathBuf,
    authority: dist::ReleaseAuthority,
    package: Option<(Digest, crate::release_package::ReleaseIndex)>,
    ambient: &'a Ambient,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubReleaseView {
    tag_name: String,
    name: String,
    body: String,
    is_draft: bool,
    is_prerelease: bool,
    assets: Vec<GithubAssetView>,
}

#[derive(Deserialize)]
struct GithubAssetView {
    name: String,
    digest: Option<String>,
    state: String,
}

struct HashWriter(Sha256);

impl Default for HashWriter {
    fn default() -> Self {
        Self(Sha256::new())
    }
}

impl io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl HashWriter {
    fn finish(self) -> String {
        format!("sha256:{:x}", self.0.finalize())
    }
}

fn release_target(name: &str) -> Result<wire::manifest::ReleaseTarget, dist::EffectError> {
    use wire::manifest::ReleaseTarget;
    match name {
        "aarch64-apple-darwin" => Ok(ReleaseTarget::Aarch64AppleDarwin),
        "x86_64-unknown-linux-musl" => Ok(ReleaseTarget::X86_64UnknownLinuxMusl),
        "x86_64-pc-windows-msvc" => Ok(ReleaseTarget::X86_64PcWindowsMsvc),
        _ => Err(dist::EffectError::Moved(format!(
            "release plan contains unknown target {name}"
        ))),
    }
}

fn parse_github_release_view(
    tag: String,
    bytes: &[u8],
    expected: Vec<(dist::GithubReleaseAsset, String)>,
) -> Result<dist::OperationState, dist::EffectError> {
    let view: GithubReleaseView = serde_json::from_slice(bytes).map_err(|error| {
        dist::EffectError::RemoteRefused(format!(
            "GitHub Release observation was not canonical JSON: {error}"
        ))
    })?;
    if view.is_draft || view.is_prerelease {
        return Err(dist::EffectError::Moved(
            "GitHub Release is draft or prerelease rather than final".to_owned(),
        ));
    }
    if view.tag_name != tag {
        return Err(dist::EffectError::Moved(format!(
            "GitHub Release names tag {}, not {tag}",
            view.tag_name
        )));
    }
    let remote_count = view.assets.len();
    let mut remote = view
        .assets
        .into_iter()
        .map(|asset| (asset.name.clone(), asset))
        .collect::<std::collections::BTreeMap<_, _>>();
    if remote.len() != remote_count {
        return Err(dist::EffectError::Moved(
            "GitHub Release repeats an asset name".to_owned(),
        ));
    }
    let mut admitted = Vec::with_capacity(remote.len());
    for (asset, checksum) in expected {
        let Some(observed) = remote.remove(&asset.name) else {
            continue;
        };
        if observed.state != "uploaded" || observed.digest.as_deref() != Some(&checksum) {
            return Err(dist::EffectError::Moved(format!(
                "GitHub Release asset {} moved from its sealed bytes",
                asset.name
            )));
        }
        admitted.push(asset);
    }
    if let Some(name) = remote.keys().next() {
        return Err(dist::EffectError::Moved(format!(
            "GitHub Release has unexpected asset {name}"
        )));
    }
    Ok(dist::OperationState::GithubRelease {
        tag,
        title: Some(view.name),
        notes: Some(Digest::compute(
            wire::Domain::DistReleaseNotes,
            view.body.as_bytes(),
        )),
        assets: Some(admitted),
    })
}

impl<'a> SystemEffects<'a> {
    fn new(
        root: &Path,
        journal_root: &Path,
        ambient: &'a Ambient,
    ) -> Result<Self, dist::EffectError> {
        let authority = dist::current_authority(root)
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        let probe_root = journal_root.join("probes");
        Ok(Self {
            root: root.to_path_buf(),
            probe_root,
            authority,
            package: None,
            ambient,
        })
    }

    fn package_index(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<&crate::release_package::ReleaseIndex, dist::EffectError> {
        if let Some((key, _)) = &self.package
            && key != &plan.key
        {
            return Err(dist::EffectError::Moved(
                "release effect adapter was reused for another plan".to_owned(),
            ));
        }
        if self.package.is_none() {
            let index = crate::release_package::ReleaseIndex::admit(
                &self.root,
                &plan.requested_version,
                plan.notes.clone(),
                plan.notes_document.as_bytes(),
                &plan.manifest,
            )
            .map_err(package_failure)?;
            self.package = Some((plan.key.clone(), index));
        }
        Ok(&self.package.as_ref().expect("package index admitted").1)
    }

    fn github_asset_checksums(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<Vec<(dist::GithubReleaseAsset, String)>, dist::EffectError> {
        let expected = dist::github_release_assets(plan);
        let mut checksums = Vec::with_capacity(expected.len());
        for (target, asset) in plan.targets.iter().zip(expected) {
            let target = release_target(&target.target)?;
            let member = self
                .package_index(plan)?
                .target_executable(target)
                .map_err(package_failure)?;
            if member.digest() != &asset.identity {
                return Err(dist::EffectError::Moved(format!(
                    "release asset {} moved from its sealed target identity",
                    asset.name
                )));
            }
            let mut hash = HashWriter::default();
            member.copy_to(&mut hash).map_err(package_failure)?;
            checksums.push((asset, hash.finish()));
        }
        Ok(checksums)
    }

    fn github_release_state(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<dist::OperationState, dist::EffectError> {
        let tag = format!("v{}", plan.requested_version);
        let output = Command::new("gh")
            .args([
                "release",
                "view",
                &tag,
                "--json",
                "tagName,name,body,isDraft,isPrerelease,assets",
            ])
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| {
                dist::EffectError::Environment(format!("cannot launch gh: {error}"))
            })?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            if detail.to_ascii_lowercase().contains("release not found") {
                return Ok(dist::OperationState::GithubRelease {
                    tag,
                    title: None,
                    notes: None,
                    assets: None,
                });
            }
            return Err(dist::EffectError::RemoteRefused(format!(
                "gh release view exited {:?}: {}",
                output.status.code(),
                detail.trim()
            )));
        }
        let expected = self.github_asset_checksums(plan)?;
        parse_github_release_view(tag, &output.stdout, expected)
    }

    fn materialize_github_assets(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<Vec<PathBuf>, dist::EffectError> {
        let root = self
            .probe_root
            .join(plan.key.as_str())
            .join("github-release");
        fs::create_dir_all(&root)
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        let assets = dist::github_release_assets(plan);
        let mut paths = Vec::with_capacity(assets.len());
        for (target, asset) in plan.targets.iter().zip(assets) {
            let target = release_target(&target.target)?;
            let member = self
                .package_index(plan)?
                .target_executable(target)
                .map_err(package_failure)?;
            if member.digest() != &asset.identity {
                return Err(dist::EffectError::Moved(format!(
                    "release asset {} moved from its sealed target identity",
                    asset.name
                )));
            }
            let path = root.join(&asset.name);
            let mut staged = atomic::Destination::new(&path)
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?
                .stage()
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
            member.copy_to(&mut staged).map_err(package_failure)?;
            staged
                .commit()
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
            paths.push(path);
        }
        Ok(paths)
    }

    fn converge_github_release(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        let expected_assets = dist::github_release_assets(plan);
        let tag = format!("v{}", plan.requested_version);
        let expected = dist::OperationState::GithubRelease {
            tag: tag.clone(),
            title: Some(tag.clone()),
            notes: Some(plan.notes.clone()),
            assets: Some(expected_assets.clone()),
        };
        let current = self.github_release_state(plan)?;
        if current == expected {
            return Ok((current, dist::Convergence::AlreadyConverged));
        }
        let present_assets = match &current {
            dist::OperationState::GithubRelease {
                title: None,
                notes: None,
                assets: None,
                ..
            } => Vec::new(),
            dist::OperationState::GithubRelease {
                title: Some(title),
                notes: Some(notes),
                assets: Some(assets),
                ..
            } if title == &tag
                && notes == &plan.notes
                && assets.iter().all(|asset| expected_assets.contains(asset)) =>
            {
                assets.clone()
            }
            _ => {
                return Err(dist::EffectError::Moved(
                    "GitHub Release moved from the previewed title, notes, or assets".to_owned(),
                ));
            }
        };
        let paths = self.materialize_github_assets(plan)?;
        let notes_path = self
            .probe_root
            .join(plan.key.as_str())
            .join("github-release")
            .join("notes.md");
        fs::write(&notes_path, plan.notes_document.as_bytes())
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        if matches!(
            current,
            dist::OperationState::GithubRelease { title: None, .. }
        ) {
            run(
                &self.root,
                "gh",
                &[
                    "release",
                    "create",
                    &tag,
                    "--verify-tag",
                    "--title",
                    &tag,
                    "--notes-file",
                    notes_path.to_string_lossy().as_ref(),
                    "--latest=false",
                ],
            )
            .map_err(dist::EffectError::RemoteRefused)?;
        }
        for (asset, path) in expected_assets.iter().zip(paths) {
            if present_assets.contains(asset) {
                continue;
            }
            run(
                &self.root,
                "gh",
                &["release", "upload", &tag, path.to_string_lossy().as_ref()],
            )
            .map_err(dist::EffectError::RemoteRefused)?;
        }
        let result = self.github_release_state(plan)?;
        if result != expected {
            return Err(dist::EffectError::Moved(
                "GitHub Release did not converge to the sealed title, notes, and assets".to_owned(),
            ));
        }
        Ok((result, dist::Convergence::Applied))
    }

    fn remote_ref(&self, reference: &str) -> Result<Option<String>, dist::EffectError> {
        let command = self
            .authority
            .reference_visibility(reference)
            .map_err(|error| dist::EffectError::RemoteRefused(error.to_string()))?;
        let output = self.git_owned(&command)?;
        Ok(output.split_whitespace().next().map(str::to_owned))
    }

    fn git_local(&self, arguments: &[&str]) -> Result<String, dist::EffectError> {
        run(&self.root, "git", arguments).map_err(dist::EffectError::Environment)
    }

    fn git_owned(&self, command: &dist::GitCommand) -> Result<String, dist::EffectError> {
        let arguments = command
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        run(&self.root, command.program(), &arguments).map_err(dist::EffectError::RemoteRefused)
    }

    fn reference_for(&self, plan: &ReleasePlan, operation: Operation) -> Option<String> {
        let refs = self.authority.refs(&plan.requested_version);
        match operation {
            Operation::RehearseUpload | Operation::UploadPackage => Some(refs.package().to_owned()),
            Operation::CreateImmutableTag => Some(refs.tag().to_owned()),
            Operation::Announce => Some(refs.announcement().to_owned()),
            _ => None,
        }
    }

    fn registry_state(
        &self,
        plan: &ReleasePlan,
    ) -> Result<dist::OperationState, dist::EffectError> {
        let reference = self
            .authority
            .refs(&plan.requested_version)
            .package()
            .to_owned();
        let evidence = match self.remote_ref(&reference)? {
            Some(object) if object == plan.package_object => Some(RegistryEvidence {
                version: plan.requested_version.clone(),
                release_commit: plan.release_commit.clone(),
                package: plan.package.clone(),
            }),
            Some(object) => {
                return Err(dist::EffectError::RemoteRefused(format!(
                    "{reference} names {object}, not {}",
                    plan.package_object
                )));
            }
            None => None,
        };
        Ok(dist::OperationState::Registry { evidence })
    }

    fn probe_path(&self, plan: &ReleasePlan) -> PathBuf {
        self.probe_root.join(plan.key.as_str()).join("spice")
    }

    fn installed_state(
        &self,
        plan: &ReleasePlan,
    ) -> Result<dist::OperationState, dist::EffectError> {
        let path = self.probe_path(plan);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                let bytes = fs::read(path)
                    .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
                Ok(dist::OperationState::Artifact {
                    identity: Some(Digest::compute(wire::Domain::DistArtifact, &bytes)),
                })
            }
            Ok(_) => Err(dist::EffectError::Moved(
                "independent release probe is not a regular file".to_owned(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(dist::OperationState::Artifact { identity: None })
            }
            Err(error) => Err(dist::EffectError::Environment(error.to_string())),
        }
    }

    fn gate_state(&self, plan: &ReleasePlan) -> Result<dist::OperationState, dist::EffectError> {
        let path = self
            .probe_root
            .join(plan.key.as_str())
            .join("correctness-gate.receipt");
        let receipt = match fs::read(&path) {
            Ok(bytes) => Some(
                String::from_utf8(bytes)
                    .map_err(|error| dist::EffectError::Environment(error.to_string()))?
                    .trim()
                    .parse::<Digest>()
                    .map_err(|error| dist::EffectError::Environment(error.to_string()))?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(dist::EffectError::Environment(error.to_string())),
        };
        Ok(dist::OperationState::Gate { receipt })
    }

    fn install_and_probe(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        let target = proof::compiled_target()
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        let executable = self
            .package_index(plan)?
            .target_executable(target)
            .map_err(package_failure)?;
        let identity = executable.digest().clone();
        if identity != plan.installed_artifact {
            return Err(dist::EffectError::Moved(
                "sealed host executable identity moved".to_owned(),
            ));
        }
        let path = self.probe_path(plan);
        let prior = self.installed_state(plan)?;
        let convergence = if prior
            == (dist::OperationState::Artifact {
                identity: Some(identity.clone()),
            }) {
            dist::Convergence::AlreadyConverged
        } else {
            let parent = path
                .parent()
                .expect("release probe path has a fixed parent");
            fs::create_dir_all(parent)
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
            let mut staged = atomic::Destination::new(&path)
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?
                .stage()
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
            executable
                .copy_to(&mut staged)
                .map_err(|error| dist::EffectError::Moved(error.to_string()))?;
            make_executable(staged.temporary_path())?;
            staged
                .commit()
                .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
            dist::Convergence::Applied
        };
        run(
            &self.root,
            path.to_string_lossy().as_ref(),
            &["--json", "version"],
        )
        .map_err(dist::EffectError::GateRefused)?;
        Ok((
            dist::OperationState::Artifact {
                identity: Some(identity),
            },
            convergence,
        ))
    }
}

impl dist::EffectPort for SystemEffects<'_> {
    fn preflight(&mut self, plan: &ReleasePlan) -> Result<ReleaseObservations, dist::EffectError> {
        let mut observed = plan.observations.clone();
        let repository =
            crate::release_command::repository_identity(&self.root, self.authority.remote())
                .map_err(dist::EffectError::Environment)?;
        if repository != plan.repository {
            return Err(dist::EffectError::Moved(
                "release repository authority changed".to_owned(),
            ));
        }
        let head = self.git_local(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        let status = self.git_local(&["status", "--porcelain=v1", "--untracked-files=no"])?;
        let version = spice::version::source_version(&self.root, dist::Build::Staged)
            .map_err(dist::EffectError::Environment)?;
        if !admitted_local_progress(plan, &head, &version, &status) {
            observed.head = head.clone();
            observed.clean = status.is_empty();
            observed.release_version = version;
        }
        let refs = self.authority.refs(&plan.requested_version);
        observed.package_ref = stable_ref_observation(
            plan.observations.package_ref.as_ref(),
            self.remote_ref(refs.package())?,
            &plan.package_object,
        );
        observed.tag_ref = stable_ref_observation(
            plan.observations.tag_ref.as_ref(),
            self.remote_ref(refs.tag())?,
            &plan.release_commit,
        );
        observed.announcement_ref = stable_ref_observation(
            plan.observations.announcement_ref.as_ref(),
            self.remote_ref(refs.announcement())?,
            &plan.release_commit,
        );
        observed.credentials_valid = true;
        let package = self.package_index(plan)?.preview().clone();
        if package.identity != plan.package || package.git_tree != plan.package_object {
            return Err(dist::EffectError::Moved(
                "staged package identity or Git tree changed".to_owned(),
            ));
        }
        Ok(observed)
    }

    fn prior(
        &mut self,
        plan: &ReleasePlan,
        operation: Operation,
    ) -> Result<dist::OperationState, dist::EffectError> {
        use Operation as Op;
        match operation {
            Op::ResolvePreconditionsAndCredentials => {
                let identity = crate::release_command::repository_identity(
                    &self.root,
                    self.authority.remote(),
                )
                .map_err(dist::EffectError::Environment)?;
                Ok(dist::OperationState::Binding { identity })
            }
            Op::RunCorrectnessGate => self.gate_state(plan),
            Op::RehearseUpload => {
                let package = self.package_index(plan)?.preview().clone();
                Ok(dist::OperationState::Package {
                    identity: Some(package.identity),
                    object: object_exists(&self.root, &package.git_tree)?
                        .then_some(package.git_tree),
                })
            }
            Op::InstallAndProbeArtifacts => self.installed_state(plan),
            Op::AwaitRegistryVisibility => self.registry_state(plan),
            Op::PublishGithubReleaseAssets => self.github_release_state(plan),
            Op::UploadPackage | Op::CreateImmutableTag => {
                let reference = self
                    .reference_for(plan, operation)
                    .expect("remote-ref operation has one authority ref");
                Ok(dist::OperationState::RemoteRef {
                    object: self.remote_ref(&reference)?,
                    reference,
                })
            }
            Op::Announce => {
                let reference = self
                    .reference_for(plan, operation)
                    .expect("announcement has one authority ref");
                Ok(dist::OperationState::Announcement {
                    object: self.remote_ref(&reference)?,
                })
            }
        }
    }

    fn converge(
        &mut self,
        plan: &ReleasePlan,
        operation: Operation,
        prior: &dist::OperationState,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        use Operation as Op;
        match operation {
            Op::ResolvePreconditionsAndCredentials => self.converge_preconditions(plan),
            Op::RunCorrectnessGate => self.converge_gate(plan, prior),
            Op::InstallAndProbeArtifacts => self.install_and_probe(plan),
            Op::RehearseUpload => self.converge_rehearsal(plan),
            Op::PublishGithubReleaseAssets => self.converge_github_release(plan),
            Op::UploadPackage | Op::CreateImmutableTag | Op::Announce => {
                self.converge_immutable(plan, operation, prior)
            }
            Op::AwaitRegistryVisibility => self.await_registry(plan),
        }
    }
}

impl SystemEffects<'_> {
    fn converge_preconditions(
        &self,
        plan: &ReleasePlan,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        let identity =
            crate::release_command::repository_identity(&self.root, self.authority.remote())
                .map_err(dist::EffectError::Environment)?;
        if identity != plan.repository {
            return Err(dist::EffectError::Moved(
                "release repository authority changed".to_owned(),
            ));
        }
        Ok((
            dist::OperationState::Binding { identity },
            dist::Convergence::Applied,
        ))
    }

    fn converge_gate(
        &self,
        plan: &ReleasePlan,
        prior: &dist::OperationState,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        let current = self.gate_state(plan)?;
        if current
            == (dist::OperationState::Gate {
                receipt: Some(dist::gate_receipt(plan)),
            })
        {
            return Ok((current, dist::Convergence::AlreadyConverged));
        }
        if matches!(current, dist::OperationState::Gate { receipt: Some(_) }) {
            return Err(dist::EffectError::Moved(
                "correctness gate receipt identity changed".to_owned(),
            ));
        }
        if matches!(prior, dist::OperationState::Gate { receipt: Some(_) }) {
            return Ok((prior.clone(), dist::Convergence::AlreadyConverged));
        }
        if !matches!(prior, dist::OperationState::Gate { receipt: None }) {
            return Err(dist::EffectError::Environment(
                "correctness gate received the wrong prior-state shape".to_owned(),
            ));
        }
        run_release_gate(&self.root, self.ambient, &plan.source_commit)?;
        let head = self.git_local(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        let status = self.git_local(&["status", "--porcelain=v1", "--untracked-files=no"])?;
        if head != plan.source_commit || !status.is_empty() {
            return Err(dist::EffectError::GateRefused(format!(
                "correctness gate moved source state: HEAD={head} status={status:?}"
            )));
        }
        let receipt = dist::gate_receipt(plan);
        let path = self
            .probe_root
            .join(plan.key.as_str())
            .join("correctness-gate.receipt");
        let parent = path.parent().expect("gate receipt has one parent");
        fs::create_dir_all(parent)
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        let mut staged = atomic::Destination::new(&path)
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?
            .stage()
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        staged
            .write_all(format!("{receipt}\n").as_bytes())
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        staged
            .commit()
            .map_err(|error| dist::EffectError::Environment(error.to_string()))?;
        Ok((
            dist::OperationState::Gate {
                receipt: Some(receipt),
            },
            dist::Convergence::Applied,
        ))
    }

    fn converge_rehearsal(
        &mut self,
        plan: &ReleasePlan,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        let root = self.root.clone();
        let package = self
            .package_index(plan)?
            .materialize(&root)
            .map_err(package_failure)?;
        if package.identity != plan.package || package.git_tree != plan.package_object {
            return Err(dist::EffectError::Moved(
                "materialized package identity or Git tree changed".to_owned(),
            ));
        }
        let command = self
            .authority
            .package_rehearsal(&plan.requested_version, &plan.package_object)
            .map_err(|error| dist::EffectError::RemoteRefused(error.to_string()))?;
        self.git_owned(&command)?;
        Ok((
            package_state(package.identity, package.git_tree),
            dist::Convergence::Applied,
        ))
    }

    fn await_registry(
        &self,
        plan: &ReleasePlan,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        for _ in 0..VISIBILITY_ATTEMPTS {
            let observed = self.registry_state(plan)?;
            if matches!(
                observed,
                dist::OperationState::Registry { evidence: Some(_) }
            ) {
                return Ok((observed, dist::Convergence::AlreadyConverged));
            }
        }
        Err(dist::EffectError::RemoteRefused(format!(
            "package v{} did not become visible within {VISIBILITY_ATTEMPTS} responses",
            plan.requested_version
        )))
    }

    fn converge_immutable(
        &self,
        plan: &ReleasePlan,
        operation: Operation,
        prior: &dist::OperationState,
    ) -> Result<(dist::OperationState, dist::Convergence), dist::EffectError> {
        match prior {
            dist::OperationState::RemoteRef { .. } | dist::OperationState::Announcement { .. } => {}
            _ => {
                return Err(dist::EffectError::Environment(format!(
                    "{operation:?} received the wrong prior-state shape"
                )));
            }
        }
        let reference = self
            .reference_for(plan, operation)
            .expect("immutable operation has one authority ref");
        let current = self.remote_ref(&reference)?;
        let mutation = match operation {
            Operation::UploadPackage => self.authority.package_push(
                &plan.requested_version,
                &plan.package_object,
                ref_state(current.as_deref()),
            ),
            Operation::CreateImmutableTag => self.authority.tag_push(
                &plan.requested_version,
                &plan.release_commit,
                ref_state(current.as_deref()),
            ),
            Operation::Announce => self.authority.announcement_push(
                &plan.requested_version,
                &plan.release_commit,
                ref_state(current.as_deref()),
            ),
            _ => unreachable!("caller admits only immutable-ref operations"),
        }
        .map_err(|error| dist::EffectError::RemoteRefused(error.to_string()))?;
        let convergence = match mutation {
            dist::GitMutation::Converged => dist::Convergence::AlreadyConverged,
            dist::GitMutation::Command(command) => {
                self.git_owned(&command)?;
                dist::Convergence::Applied
            }
        };
        let target_object = match operation {
            Operation::UploadPackage => &plan.package_object,
            Operation::CreateImmutableTag | Operation::Announce => &plan.release_commit,
            _ => unreachable!("caller admits only immutable-ref operations"),
        };
        let result = match operation {
            Operation::Announce => dist::OperationState::Announcement {
                object: Some(target_object.to_owned()),
            },
            _ => dist::OperationState::RemoteRef {
                reference,
                object: Some(target_object.to_owned()),
            },
        };
        Ok((result, convergence))
    }
}

mod tail;
#[cfg(test)]
use tail::effect_outcome;
use tail::{
    admitted_local_progress, make_executable, object_exists, package_state, ref_state, run,
    stable_ref_observation,
};
pub(crate) use tail::{execute, verify_completed};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_effect_failures_keep_their_operator_visible_categories() {
        let refusals = [
            (
                dist::EffectError::Moved("binding".to_owned()),
                "release::external-plan-moved",
            ),
            (
                dist::EffectError::Interrupted,
                "release::operator-interrupted",
            ),
            (
                dist::EffectError::GateRefused("gate".to_owned()),
                "release::gate-refused",
            ),
            (
                dist::EffectError::RemoteRefused("remote".to_owned()),
                "release::remote-refused",
            ),
        ];
        for (error, expected) in refusals {
            let Outcome::Refusal(refusal) = effect_outcome(error) else {
                panic!("{expected} must remain an operator refusal")
            };
            assert_eq!(refusal.reason(), expected);
        }

        let Outcome::Fault(fault) =
            effect_outcome(dist::EffectError::Environment("filesystem".to_owned()))
        else {
            panic!("environment failure must remain a fault")
        };
        assert_eq!(fault.class, wire::FaultClass::Environment);
        assert_eq!(fault.reason, "release::environment-failed");
    }

    #[test]
    fn recognition_verification_cannot_bypass_a_missing_external_journal() {
        let journal = std::env::temp_dir().join(format!(
            "spice-absent-release-journal-{}",
            std::process::id()
        ));
        let key = Digest::compute(wire::Domain::DistReleasePlan, b"missing plan");
        let manifest = Digest::compute(wire::Domain::DistReleaseManifest, b"missing manifest");
        let notes = Digest::compute(wire::Domain::DistReleaseNotes, b"missing notes");

        verify_completed(&journal, &key, &manifest, &notes, &[])
            .expect_err("resident recognition requires the external receipt chain");
    }

    #[test]
    fn release_gate_binds_the_explicit_root_and_source_and_refuses_red() {
        let root = Path::new("/fixture/release-root");
        let source = "0123456789abcdef0123456789abcdef01234567";
        run_release_gate_with(root, source, |observed_root, selector| {
            assert_eq!(observed_root, root);
            assert_eq!(selector, gate::Selector::revision(source));
            Ok(CorrectnessGateStanding::Admitted)
        })
        .expect("an admitted canonical report crosses the release gate");

        let refused = run_release_gate_with(root, "source", |_, _| {
            Ok(CorrectnessGateStanding::Refused("red arm".to_owned()))
        })
        .expect_err("a red canonical report cannot mint a release receipt");
        assert!(matches!(
            refused,
            dist::EffectError::GateRefused(detail) if detail == "red arm"
        ));

        let unavailable = run_release_gate_with(root, "source", |_, _| {
            Err(dist::EffectError::Environment(
                "gate unavailable".to_owned(),
            ))
        })
        .expect_err("a gate acquisition fault stays an environment fault");
        assert!(matches!(
            unavailable,
            dist::EffectError::Environment(detail) if detail == "gate unavailable"
        ));
    }
    #[test]
    fn github_release_observation_binds_every_canonical_asset_digest() {
        let digest = |domain, bytes: &[u8]| Digest::compute(domain, bytes);
        let version: wire::manifest::ProductVersion = "1.2.3".parse().expect("version");
        let notes_document = "## Release\n".to_owned();
        let notes = digest(wire::Domain::DistReleaseNotes, notes_document.as_bytes());
        let targets = [
            ("aarch64-apple-darwin", b"darwin".as_slice()),
            ("x86_64-unknown-linux-musl", b"linux".as_slice()),
            ("x86_64-pc-windows-msvc", b"windows".as_slice()),
        ]
        .into_iter()
        .map(|(target, bytes)| wire::body::PublishedTarget {
            target: target.to_owned(),
            artifact: digest(wire::Domain::DistArtifact, bytes),
            receipt: digest(wire::Domain::ProofReceipt, bytes),
        })
        .collect::<Vec<_>>();
        let plan = ReleasePlan {
            key: digest(wire::Domain::DistReleasePlan, b"plan"),
            repository: digest(wire::Domain::DistSourceCommit, b"repository"),
            requested_version: version.clone(),
            source_commit: "1".repeat(40),
            source_archive: digest(wire::Domain::DistSourceArchive, b"archive"),
            release_commit: "1".repeat(40),
            manifest: digest(wire::Domain::DistReleaseManifest, b"manifest"),
            notes: notes.clone(),
            notes_document: notes_document.clone(),
            package: digest(wire::Domain::DistReleasePackage, b"package"),
            package_object: "2".repeat(40),
            installed_artifact: targets[0].artifact.clone(),
            targets,
            operations: Vec::new(),
            observations: ReleaseObservations {
                head: "1".repeat(40),
                clean: true,
                unrecorded_local_commits: 0,
                source_version: version.clone(),
                release_version: version,
                credentials_valid: true,
                package_ref: None,
                tag_ref: None,
                announcement_ref: None,
            },
        };
        let assets = dist::github_release_assets(&plan);
        let checksums = assets
            .iter()
            .cloned()
            .zip(["sha256:11", "sha256:22", "sha256:33"].map(str::to_owned))
            .collect::<Vec<_>>();
        let body = serde_json::json!({
            "tagName": "v1.2.3",
            "name": "v1.2.3",
            "body": notes_document,
            "isDraft": false,
            "isPrerelease": false,
            "assets": assets.iter().zip(&checksums).map(|(asset, (_, checksum))| {
                serde_json::json!({
                    "name": asset.name,
                    "digest": checksum,
                    "state": "uploaded"
                })
            }).collect::<Vec<_>>()
        });
        let observed = parse_github_release_view(
            "v1.2.3".to_owned(),
            &serde_json::to_vec(&body).expect("JSON"),
            checksums.clone(),
        )
        .expect("exact provider evidence");
        assert_eq!(
            observed,
            dist::OperationState::GithubRelease {
                tag: "v1.2.3".to_owned(),
                title: Some("v1.2.3".to_owned()),
                notes: Some(notes),
                assets: Some(assets),
            }
        );

        let mut moved = body;
        moved["assets"][1]["digest"] = serde_json::json!("sha256:bad");
        assert!(matches!(
            parse_github_release_view(
                "v1.2.3".to_owned(),
                &serde_json::to_vec(&moved).expect("JSON"),
                checksums,
            ),
            Err(dist::EffectError::Moved(_))
        ));
    }
}
