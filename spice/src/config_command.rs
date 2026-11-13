use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{command_refusal as refusal, environment_fault, protocol_fault};
use clap::ArgMatches;
use serde::Serialize;
use sill::authority::{self, Directory, Entry, FileIdentity, FileObject};
use sill::{Acquisition, LockFile, LockName};
use spice::config::{
    self, Builtin, Document, Inputs, PathSegment, PolicyClass, Resolved, ValueKind,
};
use spice::repository::Repository;
use wire::{ConfigSource, Digest, Domain, Outcome, Refusal, Reply, ReplyPosition};

const CONFIG_PLAN_VERSION: u16 = 1;
const PUBLICATION_ATTEMPTS: usize = 128;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ConfigReport {
    mode: ConfigMode,
    effective: config::Root,
    provenance: Vec<ProvenanceRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected: Option<Selection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<MutationPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation: Option<MutationReceipt>,
}

impl ConfigReport {
    /// Text answers a keyed read or change with only its selection, plan, and
    /// receipt; a bare read prints the full effective document. JSON always
    /// carries the complete report.
    pub(crate) fn text(&self) -> Result<Vec<u8>, toml::ser::Error> {
        #[derive(Serialize)]
        struct Focus<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            selected: Option<&'a Selection>,
            #[serde(skip_serializing_if = "Option::is_none")]
            plan: Option<&'a MutationPlan>,
            #[serde(skip_serializing_if = "Option::is_none")]
            mutation: Option<&'a MutationReceipt>,
        }
        let document = if self.selected.is_some() || self.plan.is_some() || self.mutation.is_some()
        {
            toml::to_string_pretty(&Focus {
                selected: self.selected.as_ref(),
                plan: self.plan.as_ref(),
                mutation: self.mutation.as_ref(),
            })?
        } else {
            toml::to_string_pretty(self)?
        };
        let mut document = document.into_bytes();
        if document.last() != Some(&b'\n') {
            document.push(b'\n');
        }
        Ok(document)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ConfigMode {
    Current,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProvenanceRow {
    path: String,
    source: ConfigSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin: Option<Builtin>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Selection {
    path: String,
    value: toml::Value,
    source: ConfigSource,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct MutationReceipt {
    plan: Digest,
    path: String,
    destination: PathBuf,
    before: Digest,
    after: Digest,
    changed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct MutationPlan {
    version: u16,
    digest: Digest,
    path: String,
    destination: PathBuf,
    before: Digest,
    after: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
enum ParentWitness {
    Absent,
    Directory {
        canonical: PathBuf,
        object: FileObject,
    },
}

pub(crate) fn run(matches: &ArgMatches) -> Outcome<ConfigReport> {
    let current = match crate::working_directory() {
        Ok(current) => current,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let key = matches.get_one::<String>("KEY");
    let value = matches.get_one::<String>("VALUE");
    let apply = matches.get_one::<String>("apply");
    let result = match (key, value) {
        (Some(key), Some(value)) => set(&repository, key, value, apply.map(String::as_str)),
        (_, None) if apply.is_some() => Err(refusal(
            "config::apply-without-mutation",
            None,
            "supply one mutable KEY VALUE pair with the plan digest",
        )),
        (key, None) => read(&repository, key.map(String::as_str)),
        (None, Some(_)) => unreachable!("clap cannot supply VALUE without KEY"),
    };
    match result {
        Ok(report) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: report,
        }),
        Err(outcome) => crate::typed_outcome(outcome),
    }
}

fn read(repository: &Repository, key: Option<&str>) -> Result<ConfigReport, Outcome<()>> {
    let documents = Documents::read(repository)?;
    let resolved = documents.resolve_current()?;
    report(resolved, key, None, None)
}

fn set(
    repository: &Repository,
    key: &str,
    raw: &str,
    apply: Option<&str>,
) -> Result<ConfigReport, Outcome<()>> {
    let keys = parse_key(key)?;
    let borrowed = borrowed(&keys);
    let descriptor = config::descriptor(&borrowed).ok_or_else(|| {
        refusal(
            "config::key-unknown",
            Some(key.to_owned()),
            "select one concrete path in the generated configuration registry",
        )
    })?;
    let target = match descriptor.policy_class() {
        PolicyClass::Policy => {
            return Err(refusal(
                "config::tracked-policy",
                Some(key.to_owned()),
                format!(
                    "edit tracked {} in an operator-reviewed commit",
                    wire::TRACKED_POLICY_FILE
                ),
            ));
        }
        PolicyClass::WorktreeLocal => MutationTarget::Worktree,
        PolicyClass::MachineLocal => MutationTarget::Local,
    };
    let value = parse_value(raw, descriptor.kind())?;
    let documents = Documents::read_for_mutation(repository, target)?;
    let mut document = documents.value(target)?;
    set_value(&mut document, &keys, value)?;
    let candidate = toml::to_string_pretty(&document)
        .map_err(|error| protocol_fault("config::candidate-unencodable", error.to_string()))?;
    let resolved = documents.resolve(target, &candidate)?;
    let plan = MutationPlan::new(&documents, target, key, candidate.as_bytes())?;
    let Some(spelling) = apply else {
        return report(resolved, Some(key), Some(plan), None);
    };
    let applied = spelling.parse::<Digest>().map_err(|error| {
        refusal(
            "config::apply-digest-refused",
            Some(error.to_string()),
            "preview the current mutation and apply its exact digest",
        )
    })?;
    if applied != plan.digest {
        return Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "config::plan-moved".to_owned(),
                Some("config-plan".to_owned()),
                Some(plan.digest.to_string()),
                Some(applied.to_string()),
            ),
        )));
    }
    let receipt = publish(&documents, target, &plan, candidate.as_bytes())?;
    report(resolved, Some(key), Some(plan), Some(receipt))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationTarget {
    Worktree,
    Local,
}

impl MutationTarget {
    fn path(self, documents: &Documents) -> &Path {
        match self {
            Self::Worktree => &documents.worktree_path,
            Self::Local => &documents.local_path,
        }
    }

    fn current(self, documents: &Documents) -> Option<&[u8]> {
        match self {
            Self::Worktree => documents.worktree.as_deref(),
            Self::Local => documents.local.as_deref(),
        }
    }

    fn parent(self, documents: &Documents) -> &ParentWitness {
        match self {
            Self::Worktree => &documents.worktree_parent,
            Self::Local => &documents.local_parent,
        }
    }

    fn malformed_reason(self) -> &'static str {
        match self {
            Self::Worktree => "config::worktree-malformed",
            Self::Local => "config::local-malformed",
        }
    }

    fn non_utf8_reason(self) -> &'static str {
        match self {
            Self::Worktree => "config::worktree-non-utf8",
            Self::Local => "config::local-non-utf8",
        }
    }
}

impl MutationPlan {
    fn new(
        documents: &Documents,
        target: MutationTarget,
        key: &str,
        candidate: &[u8],
    ) -> Result<Self, Outcome<()>> {
        #[derive(Serialize)]
        struct Basis<'a> {
            version: u16,
            path: &'a str,
            destination: &'a Path,
            parent: &'a ParentWitness,
            tracked: Option<&'a [u8]>,
            worktree: Option<&'a [u8]>,
            local: Option<&'a [u8]>,
            candidate: &'a [u8],
        }

        let before = Digest::compute(
            Domain::ConfigLocal,
            target.current(documents).unwrap_or(&[]),
        );
        let after = Digest::compute(Domain::ConfigLocal, candidate);
        let basis = Basis {
            version: CONFIG_PLAN_VERSION,
            path: key,
            destination: target.path(documents),
            parent: target.parent(documents),
            tracked: documents.tracked.as_deref(),
            worktree: documents.worktree.as_deref(),
            local: documents.local.as_deref(),
            candidate,
        };
        let encoded = wire::encode(&basis)
            .map_err(|error| protocol_fault("config::plan-unencodable", error.to_string()))?;
        Ok(Self {
            version: CONFIG_PLAN_VERSION,
            digest: Digest::compute(Domain::ConfigPlan, &encoded),
            path: key.to_owned(),
            destination: target.path(documents).to_path_buf(),
            before,
            after,
        })
    }
}

struct Documents {
    tracked: Option<Vec<u8>>,
    worktree: Option<Vec<u8>>,
    local: Option<Vec<u8>>,
    tracked_path: PathBuf,
    worktree_path: PathBuf,
    local_path: PathBuf,
    worktree_parent: ParentWitness,
    local_parent: ParentWitness,
}

impl Documents {
    fn read(repository: &Repository) -> Result<Self, Outcome<()>> {
        admit_repository_owner(repository)?;
        let locations = config::locations(repository);
        let mut remaining = config::GENERATION_BYTE_LIMIT;
        Ok(Self {
            tracked: read_optional(locations.tracked(), &mut remaining)?,
            worktree: read_optional(locations.worktree(), &mut remaining)?,
            local: read_optional(locations.local(), &mut remaining)?,
            tracked_path: locations.tracked().to_path_buf(),
            worktree_path: locations.worktree().to_path_buf(),
            local_path: locations.local().to_path_buf(),
            worktree_parent: ParentWitness::Absent,
            local_parent: ParentWitness::Absent,
        })
    }

    fn read_for_mutation(
        repository: &Repository,
        target: MutationTarget,
    ) -> Result<Self, Outcome<()>> {
        let mut documents = Self::read(repository)?;
        let parent = admit_parent(target.path(&documents))?;
        match target {
            MutationTarget::Worktree => documents.worktree_parent = parent,
            MutationTarget::Local => documents.local_parent = parent,
        }
        Ok(documents)
    }

    fn value(&self, target: MutationTarget) -> Result<toml::Value, Outcome<()>> {
        let Some(bytes) = target.current(self) else {
            return Ok(toml::Value::Table(toml::Table::new()));
        };
        let text = std::str::from_utf8(bytes).map_err(|error| {
            refusal(
                target.non_utf8_reason(),
                Some(target.path(self).display().to_string()),
                error.to_string(),
            )
        })?;
        toml::from_str(text).map_err(|error| {
            refusal(
                target.malformed_reason(),
                Some(target.path(self).display().to_string()),
                error.message(),
            )
        })
    }

    fn resolve(&self, target: MutationTarget, candidate: &str) -> Result<Resolved, Outcome<()>> {
        let worktree = if target == MutationTarget::Worktree {
            Some(candidate)
        } else {
            optional_utf8(self.worktree.as_deref(), &self.worktree_path)?
        };
        let local = if target == MutationTarget::Local {
            Some(candidate)
        } else {
            optional_utf8(self.local.as_deref(), &self.local_path)?
        };
        self.resolve_with(worktree, local)
    }

    fn resolve_current(&self) -> Result<Resolved, Outcome<()>> {
        let worktree = optional_utf8(self.worktree.as_deref(), &self.worktree_path)?;
        let local = optional_utf8(self.local.as_deref(), &self.local_path)?;
        self.resolve_with(worktree, local)
    }

    fn resolve_with(
        &self,
        worktree: Option<&str>,
        local: Option<&str>,
    ) -> Result<Resolved, Outcome<()>> {
        let tracked_text = optional_utf8(self.tracked.as_deref(), &self.tracked_path)?;
        let tracked_location = self.tracked_path.display().to_string();
        let worktree_location = self.worktree_path.display().to_string();
        let local_location = self.local_path.display().to_string();
        config::resolve(Inputs::with_worktree(
            tracked_text.map(|text| Document::new(&tracked_location, text)),
            worktree.map(|text| Document::new(&worktree_location, text)),
            local.map(|text| Document::new(&local_location, text)),
            &[],
        ))
        .map_err(|error| resolution_refusal(&error))
    }
}

fn admit_repository_owner(repository: &Repository) -> Result<(), Outcome<()>> {
    let marker = repository.worktree_root().join(".git");
    let metadata = fs::symlink_metadata(&marker).map_err(|error| {
        environment_fault(
            "config::local-owner-unavailable",
            format!("{}: {error}", marker.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(refusal(
            "config::parent-refused",
            Some(marker.display().to_string()),
            "replace the repository owner marker with one Git directory or pointer file",
        ));
    }
    Ok(())
}

fn publish(
    documents: &Documents,
    target: MutationTarget,
    plan: &MutationPlan,
    candidate: &[u8],
) -> Result<MutationReceipt, Outcome<()>> {
    let parent = materialize_parent(documents, target)?;
    let admitted = AdmittedParent::open(target.path(documents), &parent)?;
    let changed = admitted.commit(documents, plan, candidate)?;
    Ok(receipt(plan, changed))
}

/// The configuration parent, held so that every lock, read, and publication
/// reaches the admitted directory by entry name alone.
struct AdmittedParent {
    directory: Directory,
    object: FileObject,
    target_path: PathBuf,
    target_name: OsString,
    lock: PathBuf,
}

impl AdmittedParent {
    fn open(target: &Path, witness: &ParentWitness) -> Result<Self, Outcome<()>> {
        let ParentWitness::Directory { canonical, object } = witness else {
            unreachable!("publication materializes one real directory")
        };
        let parent = config_parent(target)?;
        if parent != canonical {
            return Err(source_moved(parent));
        }
        let target_name = target.file_name().ok_or_else(|| {
            environment_fault("config::target-path-invalid", target.display().to_string())
        })?;
        let lock = wire::state_locks(canonical).join("config");
        let directory = Directory::open(canonical).map_err(|_| source_moved(canonical))?;
        let held = directory.identity().map_err(|error| {
            environment_fault(
                "config::local-parent-unavailable",
                format!("{}: {error}", canonical.display()),
            )
        })?;
        if held.object() != *object {
            return Err(source_moved(canonical));
        }
        let admitted = Self {
            directory,
            object: *object,
            target_path: target.to_path_buf(),
            target_name: target_name.to_os_string(),
            lock,
        };
        admitted.verify_named()?;
        Ok(admitted)
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn commit(
        &self,
        documents: &Documents,
        plan: &MutationPlan,
        candidate: &[u8],
    ) -> Result<bool, Outcome<()>> {
        self.verify_named()?;
        let lock_parent = self.lock.parent().expect("configuration lock has a parent");
        fs::create_dir_all(lock_parent)
            .map_err(|error| environment_fault("config::lock-unavailable", error.to_string()))?;
        let lock = LockFile::new(
            LockName::new("config").expect("config is a valid lock name"),
            &self.lock,
        );
        let possession = match lock.try_acquire() {
            Ok(Acquisition::Acquired(possession)) => possession,
            Ok(Acquisition::Contended) => {
                return Err(refusal(
                    "config::busy",
                    Some(lock.path().display().to_string()),
                    "retry after the current local configuration writer exits",
                ));
            }
            Err(error) => {
                return Err(environment_fault(
                    "config::lock-unavailable",
                    error.to_string(),
                ));
            }
        };
        let result = (|| {
            self.ensure_unchanged(documents)?;
            if plan.before == plan.after {
                return Ok(false);
            }
            self.publish_candidate(documents, candidate)?;
            Ok(true)
        })();
        drop(possession);
        result
    }

    /// Refuse when the parent's path no longer names the held directory.
    fn verify_named(&self) -> Result<(), Outcome<()>> {
        let named = authority::path_identity(self.path()).map_err(|_| source_moved(self.path()))?;
        if named.object() == self.object {
            Ok(())
        } else {
            Err(source_moved(self.path()))
        }
    }

    fn ensure_unchanged(&self, documents: &Documents) -> Result<(), Outcome<()>> {
        self.verify_named()?;
        let mut remaining = config::GENERATION_BYTE_LIMIT;
        let tracked = read_optional(&documents.tracked_path, &mut remaining)?;
        let worktree = if self.target_path == documents.worktree_path {
            self.read_target(&mut remaining)?
        } else {
            read_optional(&documents.worktree_path, &mut remaining)?
        };
        let local = if self.target_path == documents.local_path {
            self.read_target(&mut remaining)?
        } else {
            read_optional(&documents.local_path, &mut remaining)?
        };
        self.verify_named()?;
        if tracked == documents.tracked
            && worktree == documents.worktree
            && local == documents.local
        {
            Ok(())
        } else {
            Err(refusal(
                "config::source-moved",
                Some(documents.local_path.display().to_string()),
                "reload the changed tracked, worktree, or local configuration and retry",
            ))
        }
    }

    fn read_target(&self, remaining: &mut u64) -> Result<Option<Vec<u8>>, Outcome<()>> {
        let mut file = match self.directory.open_entry(&self.target_name, false) {
            Ok(Entry::Opened(file)) => file,
            Ok(Entry::Missing) => return Ok(None),
            Ok(Entry::Link) => return Err(source_not_regular(&self.target_path)),
            Err(error) => {
                return Err(environment_fault(
                    "config::source-unreadable",
                    format!("{}: {error}", self.target_path.display()),
                ));
            }
        };
        read_bounded_source(&mut file, &self.target_path, remaining).map(Some)
    }

    fn publish_candidate(
        &self,
        documents: &Documents,
        candidate: &[u8],
    ) -> Result<(), Outcome<()>> {
        let (temporary_name, mut staged) = self.create_temporary()?;
        let publication_failed =
            |error: io::Error| environment_fault("config::publication-failed", error.to_string());
        let result = (|| {
            staged.write_all(candidate).map_err(publication_failed)?;
            staged.sync_all().map_err(publication_failed)?;
            self.ensure_unchanged(documents)?;
            self.directory
                .replace(&temporary_name, &self.target_name)
                .map_err(publication_failed)?;
            self.directory.sync().map_err(publication_failed)
        })();
        drop(staged);
        if result.is_err() {
            let _ = self.directory.remove_file(&temporary_name);
        }
        result
    }

    fn create_temporary(&self) -> Result<(OsString, fs::File), Outcome<()>> {
        self.verify_named()?;
        for _ in 0..PUBLICATION_ATTEMPTS {
            let name = temporary_name();
            match self.directory.create_private(&name) {
                Ok(file) => return Ok((name, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(environment_fault(
                        "config::publication-failed",
                        error.to_string(),
                    ));
                }
            }
        }
        Err(environment_fault(
            "config::publication-failed",
            "temporary filename attempts exhausted",
        ))
    }
}

fn temporary_name() -> OsString {
    OsString::from(format!(
        ".spice-config-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ))
}

fn source_not_regular(path: &Path) -> Outcome<()> {
    refusal(
        "config::source-not-regular",
        Some(path.display().to_string()),
        "replace the source with one regular file",
    )
}

fn receipt(plan: &MutationPlan, changed: bool) -> MutationReceipt {
    MutationReceipt {
        plan: plan.digest.clone(),
        path: plan.path.clone(),
        destination: plan.destination.clone(),
        before: plan.before.clone(),
        after: plan.after.clone(),
        changed,
    }
}

fn materialize_parent(
    documents: &Documents,
    target: MutationTarget,
) -> Result<ParentWitness, Outcome<()>> {
    let path = target.path(documents);
    let parent = config_parent(path)?;
    match target.parent(documents) {
        ParentWitness::Directory { .. } => {
            let current = admit_parent(path)?;
            if current == *target.parent(documents) {
                Ok(current)
            } else {
                Err(source_moved(parent))
            }
        }
        ParentWitness::Absent => {
            if !matches!(admit_parent(path)?, ParentWitness::Absent) {
                return Err(source_moved(parent));
            }
            match fs::create_dir(parent) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(source_moved(parent));
                }
                Err(error) => {
                    return Err(environment_fault(
                        "config::parent-unavailable",
                        error.to_string(),
                    ));
                }
            }
            let created = admit_parent(path)?;
            if matches!(created, ParentWitness::Directory { .. }) {
                Ok(created)
            } else {
                Err(source_moved(parent))
            }
        }
    }
}

fn admit_parent(target: &Path) -> Result<ParentWitness, Outcome<()>> {
    let parent = config_parent(target)?;
    let metadata = match fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ParentWitness::Absent),
        Err(error) => {
            return Err(environment_fault(
                "config::local-parent-unavailable",
                format!("{}: {error}", parent.display()),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(refusal(
            "config::parent-refused",
            Some(parent.display().to_string()),
            "replace the configuration parent with one real directory",
        ));
    }
    let canonical = fs::canonicalize(parent).map_err(|error| {
        environment_fault(
            "config::local-parent-unavailable",
            format!("{}: {error}", parent.display()),
        )
    })?;
    if canonical != parent {
        return Err(refusal(
            "config::parent-refused",
            Some(canonical.display().to_string()),
            "restore the exact unlinked configuration parent",
        ));
    }
    let object = authority::path_identity(&canonical)
        .map_err(|error| {
            environment_fault(
                "config::local-parent-unavailable",
                format!("{}: {error}", canonical.display()),
            )
        })?
        .object();
    Ok(ParentWitness::Directory { canonical, object })
}

fn config_parent(target: &Path) -> Result<&Path, Outcome<()>> {
    target
        .parent()
        .ok_or_else(|| environment_fault("config::parent-missing", target.display().to_string()))
}

fn source_moved(path: &Path) -> Outcome<()> {
    refusal(
        "config::source-moved",
        Some(path.display().to_string()),
        "preview the current tracked, worktree, and local configuration generation and retry",
    )
}

fn report(
    resolved: Resolved,
    key: Option<&str>,
    plan: Option<MutationPlan>,
    mutation: Option<MutationReceipt>,
) -> Result<ConfigReport, Outcome<()>> {
    let selected = key.map(|key| selection(&resolved, key)).transpose()?;
    let provenance = resolved
        .provenance()
        .iter()
        .map(|row| ProvenanceRow {
            path: row.path().to_string(),
            source: row.source(),
            builtin: row.builtin(),
        })
        .collect();
    Ok(ConfigReport {
        mode: ConfigMode::Current,
        effective: resolved.into_root(),
        provenance,
        selected,
        plan,
        mutation,
    })
}

fn selection(resolved: &Resolved, key: &str) -> Result<Selection, Outcome<()>> {
    let keys = parse_key(key)?;
    let effective = toml::Value::try_from(resolved.root())
        .map_err(|error| protocol_fault("config::effective-unencodable", error.to_string()))?;
    let value = lookup(&effective, &keys).cloned().ok_or_else(|| {
        refusal(
            "config::key-absent",
            Some(key.to_owned()),
            "select a concrete effective configuration leaf",
        )
    })?;
    let source = resolved
        .provenance()
        .iter()
        .find(|row| path_matches(row.path(), &keys))
        .map(config::Provenance::source)
        .ok_or_else(|| {
            protocol_fault(
                "config::provenance-missing",
                format!("effective key {key} has no provenance row"),
            )
        })?;
    Ok(Selection {
        path: key.to_owned(),
        value,
        source,
    })
}

fn parse_key(raw: &str) -> Result<Vec<String>, Outcome<()>> {
    let document = format!("{raw} = 0");
    let mut value: toml::Value = toml::from_str(&document)
        .map_err(|error| refusal("config::key-refused", Some(raw.to_owned()), error.message()))?;
    let mut keys = Vec::new();
    loop {
        let toml::Value::Table(table) = value else {
            break;
        };
        if table.len() != 1 {
            return Err(refusal(
                "config::key-refused",
                Some(raw.to_owned()),
                "supply one concrete dotted table leaf",
            ));
        }
        let (key, child) = table.into_iter().next().expect("one table entry exists");
        keys.push(key);
        value = child;
    }
    if keys.len() < 2 || value != toml::Value::Integer(0) {
        return Err(refusal(
            "config::key-refused",
            Some(raw.to_owned()),
            "supply one concrete dotted table leaf",
        ));
    }
    Ok(keys)
}

fn parse_value(raw: &str, kind: ValueKind) -> Result<toml::Value, Outcome<()>> {
    let parsed = toml::from_str::<toml::Value>(&format!("value = {raw}"))
        .ok()
        .and_then(|mut value| value.as_table_mut()?.remove("value"));
    if let Some(value) = parsed {
        return Ok(value);
    }
    if matches!(kind, ValueKind::String | ValueKind::Duration) {
        return Ok(toml::Value::String(raw.to_owned()));
    }
    Err(refusal(
        "config::value-refused",
        Some(raw.to_owned()),
        format!("supply a TOML {} value", kind.as_str()),
    ))
}

fn borrowed(keys: &[String]) -> Vec<PathSegment<'_>> {
    keys.iter()
        .map(|key| PathSegment::Key(key.as_str()))
        .collect()
}

fn set_value(
    root: &mut toml::Value,
    keys: &[String],
    value: toml::Value,
) -> Result<(), Outcome<()>> {
    let mut table = root.as_table_mut().ok_or_else(|| {
        refusal(
            "config::local-malformed",
            None,
            "make local configuration a TOML table",
        )
    })?;
    for key in &keys[..keys.len() - 1] {
        let child = table
            .entry(key.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        table = child.as_table_mut().ok_or_else(|| {
            refusal(
                "config::path-conflict",
                Some(key.clone()),
                "remove the scalar that occupies the requested table path",
            )
        })?;
    }
    table.insert(
        keys.last().expect("a parsed key is nonempty").clone(),
        value,
    );
    Ok(())
}

fn lookup<'a>(root: &'a toml::Value, keys: &[String]) -> Option<&'a toml::Value> {
    keys.iter().try_fold(root, |value, key| value.get(key))
}

fn path_matches(path: &config::ConcretePath, keys: &[String]) -> bool {
    let segments = path.segments().collect::<Vec<_>>();
    segments.len() == keys.len()
        && segments
            .iter()
            .zip(keys)
            .all(|(segment, key)| matches!(segment, PathSegment::Key(found) if *found == key))
}

fn read_optional(path: &Path, remaining: &mut u64) -> Result<Option<Vec<u8>>, Outcome<()>> {
    let named = match authority::path_identity(path) {
        Ok(identity) => identity,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(environment_fault(
                "config::source-unavailable",
                format!("{}: {error}", path.display()),
            ));
        }
    };
    let metadata = fs::symlink_metadata(path).map_err(|_| source_moved(path))?;
    if !metadata.file_type().is_file() || authority::is_link_like(&metadata) {
        return Err(source_not_regular(path));
    }
    let mut file = authority::open_unfollowed(path).map_err(|error| {
        environment_fault(
            "config::source-unreadable",
            format!("{}: {error}", path.display()),
        )
    })?;
    let opened = authority::file_identity(&file).map_err(|error| source_unreadable(path, error))?;
    if opened.object() != named.object() {
        return Err(source_moved(path));
    }
    read_bounded_source(&mut file, path, remaining).map(Some)
}

/// Read one opened regular source whose identity holds across the read.
fn read_bounded_source(
    file: &mut fs::File,
    path: &Path,
    remaining: &mut u64,
) -> Result<Vec<u8>, Outcome<()>> {
    if !file
        .metadata()
        .map_err(|error| source_unreadable(path, error))?
        .is_file()
    {
        return Err(source_not_regular(path));
    }
    let opened = authority::file_identity(file).map_err(|error| source_unreadable(path, error))?;
    let limit = config::DOCUMENT_BYTE_LIMIT.min(*remaining);
    if opened.bytes() > limit {
        return Err(refusal(
            "config::source-oversized",
            Some(format!(
                "{} bytes={} maximum={limit}",
                path.display(),
                opened.bytes()
            )),
            "reduce the tracked, worktree, and local configuration generation",
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened.bytes()).expect("admitted configuration length fits usize"),
    );
    Read::by_ref(file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| source_unreadable(path, error))?;
    let finished: FileIdentity =
        authority::file_identity(file).map_err(|error| source_unreadable(path, error))?;
    if finished != opened || u64::try_from(bytes.len()).ok() != Some(opened.bytes()) {
        return Err(source_moved(path));
    }
    *remaining -= opened.bytes();
    Ok(bytes)
}

fn source_unreadable(path: &Path, error: io::Error) -> Outcome<()> {
    environment_fault(
        "config::source-unreadable",
        format!("{}: {error}", path.display()),
    )
}

fn optional_utf8<'a>(bytes: Option<&'a [u8]>, path: &Path) -> Result<Option<&'a str>, Outcome<()>> {
    bytes
        .map(|bytes| {
            std::str::from_utf8(bytes).map_err(|error| {
                refusal(
                    "config::source-non-utf8",
                    Some(path.display().to_string()),
                    error.to_string(),
                )
            })
        })
        .transpose()
}

fn resolution_refusal(error: &config::ResolveError) -> Outcome<()> {
    refusal(
        "config::candidate-refused",
        Some(
            error
                .issues()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | "),
        ),
        "supply a value admitted by the named configuration leaf",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn renamed_parent_after_admission_refuses_before_lock_or_staging() {
        let root = std::env::temp_dir().join(format!(
            "spice-config-admitted-parent-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(root.join("parent")).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let parent = root.join("parent");
        let parked = root.join("parked");
        let outside = root.join("outside");
        let tracked_path = root.join(wire::TRACKED_POLICY_FILE);
        let worktree_path = root.join(".git/.spice/worktree.toml");
        let local_path = parent.join("config.toml");
        fs::create_dir(&outside).unwrap();
        let tracked = b"[taste.polling]\nwords = ['polling']\nmessage = 'use events'\n".to_vec();
        fs::write(&tracked_path, &tracked).unwrap();
        let witness = admit_parent(&local_path).unwrap();
        let documents = Documents {
            tracked: Some(tracked),
            worktree: None,
            local: None,
            tracked_path,
            worktree_path,
            local_path: local_path.clone(),
            worktree_parent: ParentWitness::Absent,
            local_parent: witness.clone(),
        };
        let candidate = b"[lock.worker]\ndirectory = 'locks/worker'\nshards = 2\n";
        let plan = MutationPlan::new(
            &documents,
            MutationTarget::Local,
            "lock.worker.shards",
            candidate,
        )
        .unwrap();
        let admitted = AdmittedParent::open(&local_path, &witness).unwrap();

        fs::rename(&parent, &parked).unwrap();
        symlink(&outside, &parent).unwrap();
        let error = admitted.commit(&documents, &plan, candidate).unwrap_err();

        assert!(matches!(
            error,
            Outcome::Refusal(ref refusal) if refusal.reason() == "config::source-moved"
        ));
        assert!(fs::read_dir(&parked).unwrap().next().is_none());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        drop(admitted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn text_focuses_keyed_reads_and_changes() {
        let root = std::env::temp_dir().join("spice-config-text-focus");
        let documents = Documents {
            tracked: None,
            worktree: None,
            local: None,
            tracked_path: root.join(wire::TRACKED_POLICY_FILE),
            worktree_path: root.join("worktree.toml"),
            local_path: root.join("config.toml"),
            worktree_parent: ParentWitness::Absent,
            local_parent: ParentWitness::Absent,
        };
        let text = |key: Option<&str>| -> toml::Table {
            let report = report(documents.resolve_current().unwrap(), key, None, None).unwrap();
            toml::from_str(std::str::from_utf8(&report.text().unwrap()).unwrap()).unwrap()
        };

        let keyed = text(Some("vendor.omp.binary"));
        assert_eq!(keyed.keys().collect::<Vec<_>>(), ["selected"]);
        assert_eq!(
            keyed["selected"]["path"].as_str(),
            Some("vendor.omp.binary")
        );
        assert_eq!(keyed["selected"]["value"].as_str(), Some("omp"));
        assert_eq!(keyed["selected"]["source"].as_str(), Some("builtin"));

        let bare = text(None);
        assert!(bare.contains_key("effective") && bare.contains_key("provenance"));
        assert!(!bare.contains_key("selected"));

        let plan = MutationPlan::new(
            &documents,
            MutationTarget::Local,
            "vendor.omp.effort",
            b"[vendor.omp]\neffort = 'high'\n",
        )
        .unwrap();
        let mutation = receipt(&plan, true);
        let report = report(
            documents.resolve_current().unwrap(),
            Some("vendor.omp.binary"),
            Some(plan),
            Some(mutation),
        )
        .unwrap();
        let changed: toml::Table =
            toml::from_str(std::str::from_utf8(&report.text().unwrap()).unwrap()).unwrap();
        assert!(
            ["selected", "plan", "mutation"]
                .iter()
                .all(|key| changed.contains_key(*key))
        );
        assert!(!changed.contains_key("effective") && !changed.contains_key("provenance"));
    }
}
