//! Acquisition of one complete hygiene-family analysis.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::fs::{self, File, Metadata};
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

use wire::Digest;

use super::analyze::is_python_path;
use super::{
    COMPILED_DOCUMENT, COMPILED_LAYER, CURRENT_RELEASE, HYGIENE_ARMS, HygieneAnalysisError,
    HygieneConfiguration, HygieneMeasurement, HygieneProvenance, HygieneReport, SuppressedPath,
    analyze_hygiene, analyze_hygiene_arm,
};
use crate::GateId;
use crate::input::ParsedContract;
use crate::jurisdiction::{Admission, Ignore, Jurisdiction, Rule, Shape, WalkBudget};
use crate::tree::SelectedTree;

const SOURCE_FILE_LIMIT: usize = 4 * 1024;
const SOURCE_ENTRY_LIMIT: usize = SOURCE_FILE_LIMIT * 4;
const SOURCE_PATH_BYTES_LIMIT: usize = 1024 * 1024;
const SOURCE_BYTES_LIMIT: u64 = 64 * 1024 * 1024;
const SOURCE_ISSUE_LIMIT: usize = 1;
const SOURCE_DIAGNOSTIC_BYTES_LIMIT: usize = 4 * 1024;
/// Test builds never reach the capture deadline, so a starved fixture host
/// cannot turn a fixture's capture into a timeout.
#[cfg(not(test))]
const SOURCE_CAPTURE_DURATION: Duration = Duration::from_secs(30);
#[cfg(test)]
const SOURCE_CAPTURE_DURATION: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const _: () = assert!(SOURCE_ISSUE_LIMIT == 1 && SOURCE_DIAGNOSTIC_BYTES_LIMIT > 0);
/// Maximum bytes admitted from one selected Python source.
pub const HYGIENE_SOURCE_FILE_BYTE_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct HygieneDocument {
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Eq, PartialEq)]
struct SourceIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

struct Diagnostic(String);

impl fmt::Write for Diagnostic {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = SOURCE_DIAGNOSTIC_BYTES_LIMIT.saturating_sub(self.0.len());
        let mut end = remaining.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&value[..end]);
        Ok(())
    }
}

/// One immutable selected tree and the complete hygiene jurisdiction captured
/// from it.
///
/// The borrow keeps the private [`SelectedTree`] materialization alive for the
/// entire acquisition. Its digest is computed once from the same jurisdiction
/// every analyzer receives.
#[derive(Debug)]
pub struct HygieneSource<'a> {
    root: &'a Path,
    jurisdiction: Jurisdiction,
    digest: Digest,
    documents: Vec<HygieneDocument>,
    project_markers: Vec<String>,
    suppressed: Vec<SuppressedPath>,
    reached_paths: Option<BTreeSet<String>>,
}

impl<'a> HygieneSource<'a> {
    /// Capture the tracked and compiled hygiene jurisdiction over one selected
    /// tree.
    ///
    /// # Errors
    ///
    /// Refuses an invalid compiled exclusion, an incomplete walk, or a source
    /// whose admitted bytes cannot produce one complete identity.
    pub fn capture(
        tree: &'a SelectedTree,
        contract: &ParsedContract,
    ) -> Result<Self, HygieneAnalysisError> {
        Self::capture_inner(tree, contract, None)
    }

    /// Capture one dependency-expanded focused hygiene component.
    pub(crate) fn capture_focused(
        tree: &'a SelectedTree,
        contract: &ParsedContract,
        reached_paths: &[String],
    ) -> Result<Self, HygieneAnalysisError> {
        Self::capture_inner(tree, contract, Some(reached_paths))
    }

    fn capture_inner(
        tree: &'a SelectedTree,
        contract: &ParsedContract,
        reached: Option<&[String]>,
    ) -> Result<Self, HygieneAnalysisError> {
        let (ignore, provenance) = hygiene_ignore(contract)?;
        let started = Instant::now();
        let mut budget = WalkBudget::new(
            SOURCE_ENTRY_LIMIT,
            SOURCE_PATH_BYTES_LIMIT,
            started,
            SOURCE_CAPTURE_DURATION,
        );
        let reached_paths = reached
            .map(|paths| admit_reached_paths(paths, &mut budget))
            .transpose()?;
        let jurisdiction = Jurisdiction::walk_bounded(tree.root(), &ignore, &mut budget)
            .map_err(|error| capture_error("jurisdiction", error))?;
        let (documents, digest) =
            capture_documents(&jurisdiction, reached_paths.as_ref(), started)?;
        let mut project_markers = Vec::new();
        for entry in jurisdiction.measurable() {
            check_capture_time(started, entry.path())?;
            if entry.path() == "pyproject.toml" || entry.path() == wire::TRACKED_POLICY_FILE {
                project_markers.push(entry.path().to_owned());
            }
        }
        let mut suppressed = suppressed_candidates(&jurisdiction, &ignore, &provenance, started)?;
        if let Some(reached) = &reached_paths {
            let mut retained = Vec::with_capacity(suppressed.len());
            for candidate in suppressed {
                check_capture_time(started, &candidate.path)?;
                if reached.iter().any(|path| {
                    path == &candidate.path
                        || path
                            .strip_prefix(&candidate.path)
                            .is_some_and(|tail| tail.starts_with('/'))
                }) {
                    retained.push(candidate);
                }
            }
            suppressed = retained;
        }
        check_capture_time(started, "hygiene source acquisition")?;
        Ok(Self {
            root: tree.root(),
            jurisdiction,
            digest,
            documents,
            project_markers,
            suppressed,
            reached_paths,
        })
    }

    /// Immutable selected-tree root every analyzer reads.
    #[must_use]
    pub const fn root(&self) -> &Path {
        self.root
    }

    /// Complete path inventory, including every tracked or compiled exclusion.
    #[must_use]
    pub const fn jurisdiction(&self) -> &Jurisdiction {
        &self.jurisdiction
    }

    /// Identity of every admitted source byte supplied to the family.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// What this jurisdiction withheld from measurement, each naming its rule.
    ///
    /// The jurisdiction retains a rule *index*, and the rule list it indexes
    /// lives only here, so this is the last point at which a suppression can be
    /// attributed to the layer that asked for it.
    #[must_use]
    pub fn suppressed(&self) -> &[SuppressedPath] {
        &self.suppressed
    }

    fn reached_paths(&self) -> Option<&BTreeSet<String>> {
        self.reached_paths.as_ref()
    }

    fn documents(&self) -> &[HygieneDocument] {
        &self.documents
    }

    fn project_markers(&self) -> &[String] {
        &self.project_markers
    }
}

fn admit_reached_paths(
    paths: &[String],
    budget: &mut WalkBudget,
) -> Result<BTreeSet<String>, HygieneAnalysisError> {
    let mut admitted = BTreeSet::new();
    for path in paths {
        budget
            .admit("", path)
            .map_err(|error| capture_error(path, error))?;
        admitted.insert(path.clone());
    }
    Ok(admitted)
}

/// Acquire all five hygiene measurements from one captured source.
///
/// This entrypoint performs no executable lookup or process launch. The
/// selected source, tracked configuration, compiled analyzers, and Rust parser
/// are its complete authority.
///
/// # Errors
///
/// Returns a source or analysis refusal that prevents a complete five-arm
/// report.
pub fn acquire_hygiene(
    source: &HygieneSource<'_>,
    configuration: &HygieneConfiguration,
) -> Result<HygieneReport, HygieneAnalysisError> {
    let configuration = configuration
        .clone()
        .with_suppressions(source.suppressed().to_vec())?;
    analyze_hygiene(source.documents(), source.project_markers(), &configuration)
}

/// Acquire exactly one hygiene measurement from the shared captured source.
pub(crate) fn acquire_hygiene_arm(
    source: &HygieneSource<'_>,
    configuration: &HygieneConfiguration,
    gate_id: GateId,
) -> Result<HygieneMeasurement, HygieneAnalysisError> {
    let configuration = configuration
        .clone()
        .with_suppressions(source.suppressed().to_vec())?;
    analyze_hygiene_arm(
        source.documents(),
        source.project_markers(),
        &configuration,
        gate_id,
        source.reached_paths(),
    )
}

/// Acquire one hygiene arm over only its authored path reach.
///
/// The path projection comes from the complete immutable jurisdiction, so it
/// preserves the original ignore decisions without walking or snapshotting an
/// unrelated file. Suppressed ancestors are retained only when they explain a
/// reached descendant.
pub(crate) fn acquire_hygiene_arm_reached(
    source: &HygieneSource<'_>,
    configuration: &HygieneConfiguration,
    gate_id: GateId,
    reached: &[String],
) -> Result<HygieneMeasurement, HygieneAnalysisError> {
    let suppressions: Vec<_> = source
        .suppressed()
        .iter()
        .filter(|suppressed| {
            reached.iter().any(|path| {
                path == &suppressed.path
                    || path
                        .strip_prefix(&suppressed.path)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
        })
        .cloned()
        .collect();
    let configuration = configuration.clone().with_suppressions(suppressions)?;
    let reached_paths = reached.iter().cloned().collect();
    let project_markers = source
        .project_markers()
        .iter()
        .filter(|path| reached.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    analyze_hygiene_arm(
        source.documents(),
        &project_markers,
        &configuration,
        gate_id,
        Some(&reached_paths),
    )
}

fn capture_documents(
    jurisdiction: &Jurisdiction,
    reached: Option<&BTreeSet<String>>,
    started: Instant,
) -> Result<(Vec<HygieneDocument>, Digest), HygieneAnalysisError> {
    let mut documents = Vec::new();
    let mut path_bytes = 0_usize;
    let mut source_bytes = 0_u64;
    for entry in jurisdiction
        .measurable()
        .filter(|entry| is_python_path(entry.path()))
        .filter(|entry| reached.is_none_or(|paths| paths.contains(entry.path())))
    {
        check_capture_time(started, entry.path())?;
        if documents.len() == SOURCE_FILE_LIMIT {
            return Err(capture_error(
                entry.path(),
                format_args!("selected Python file limit {SOURCE_FILE_LIMIT} was exceeded"),
            ));
        }
        path_bytes = path_bytes.saturating_add(entry.path().len());
        if path_bytes > SOURCE_PATH_BYTES_LIMIT {
            return Err(capture_error(
                entry.path(),
                format_args!("selected path bytes exceeded {SOURCE_PATH_BYTES_LIMIT}"),
            ));
        }
        let absolute = jurisdiction.root().join(entry.path());
        let bytes = read_source(&absolute, entry.path(), &mut source_bytes, started)?;
        std::str::from_utf8(&bytes).map_err(|error| {
            capture_error(entry.path(), format_args!("source is not UTF-8: {error}"))
        })?;
        check_capture_time(started, entry.path())?;
        documents.push(HygieneDocument {
            path: entry.path().to_owned(),
            bytes,
        });
    }
    check_capture_time(started, "selected Python sources")?;
    let digest = document_digest(&documents);
    check_capture_time(started, "selected Python source identity")?;
    Ok((documents, digest))
}

fn read_source(
    path: &Path,
    relative: &str,
    source_bytes: &mut u64,
    started: Instant,
) -> Result<Vec<u8>, HygieneAnalysisError> {
    read_source_with(path, relative, source_bytes, started, || {})
}

fn read_source_with(
    path: &Path,
    relative: &str,
    source_bytes: &mut u64,
    started: Instant,
    after_open: impl FnOnce(),
) -> Result<Vec<u8>, HygieneAnalysisError> {
    let named = fs::symlink_metadata(path).map_err(|error| capture_error(relative, error))?;
    check_capture_time(started, relative)?;
    if !named.file_type().is_file() {
        return Err(capture_error(
            relative,
            "selected source is not a regular file",
        ));
    }
    let identity = SourceIdentity::new(&named);
    if identity.len > HYGIENE_SOURCE_FILE_BYTE_LIMIT {
        return Err(capture_error(
            relative,
            format_args!(
                "source bytes {} exceed {HYGIENE_SOURCE_FILE_BYTE_LIMIT}",
                identity.len
            ),
        ));
    }
    let total = source_bytes
        .checked_add(identity.len)
        .ok_or_else(|| capture_error(relative, "aggregate selected source bytes overflowed u64"))?;
    if total > SOURCE_BYTES_LIMIT {
        return Err(capture_error(
            relative,
            format_args!("aggregate selected source bytes exceed {SOURCE_BYTES_LIMIT}"),
        ));
    }
    let mut file = File::open(path).map_err(|error| capture_error(relative, error))?;
    check_capture_time(started, relative)?;
    if SourceIdentity::new(
        &file
            .metadata()
            .map_err(|error| capture_error(relative, error))?,
    ) != identity
    {
        return Err(capture_error(
            relative,
            "selected source moved before capture",
        ));
    }
    after_open();
    let mut bytes = Vec::with_capacity(
        usize::try_from(identity.len).expect("admitted hygiene source length fits usize"),
    );
    file.by_ref()
        .take(identity.len.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| capture_error(relative, error))?;
    check_capture_time(started, relative)?;
    let finished = file
        .metadata()
        .map_err(|error| capture_error(relative, error))?;
    let renamed = fs::symlink_metadata(path).map_err(|error| capture_error(relative, error))?;
    check_capture_time(started, relative)?;
    if SourceIdentity::new(&finished) != identity
        || SourceIdentity::new(&renamed) != identity
        || u64::try_from(bytes.len()).ok() != Some(identity.len)
    {
        return Err(capture_error(
            relative,
            "selected source moved during capture",
        ));
    }
    *source_bytes = total;
    Ok(bytes)
}

fn check_capture_time(started: Instant, path: &str) -> Result<(), HygieneAnalysisError> {
    if started.elapsed() > SOURCE_CAPTURE_DURATION {
        Err(capture_error(
            path,
            format_args!("source capture exceeded {SOURCE_CAPTURE_DURATION:?}"),
        ))
    } else {
        Ok(())
    }
}

fn capture_error(path: &str, detail: impl fmt::Display) -> HygieneAnalysisError {
    let mut diagnostic = Diagnostic(String::with_capacity(SOURCE_DIAGNOSTIC_BYTES_LIMIT));
    let _ = write!(diagnostic, "hygiene source capture at {path}: {detail}");
    HygieneAnalysisError::Source(diagnostic.0)
}

fn document_digest(documents: &[HygieneDocument]) -> Digest {
    let mut listing = String::new();
    for document in documents {
        let _ = writeln!(
            listing,
            "{} {}",
            Digest::compute(wire::Domain::GateFile, &document.bytes),
            document.path
        );
    }
    Digest::compute(wire::Domain::GateTree, listing.as_bytes())
}

impl SourceIdentity {
    fn new(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

/// Everything the walk withheld from measurement, paired with the rule that
/// withheld it.
///
/// Two shapes withhold: an ignored Python file, which is one candidate lost,
/// and an ignored directory, which loses every candidate beneath it at once.
/// The walk records the pruned directory and stops there, so nothing downstream
/// can name those candidates or count them. The directory itself is the whole
/// truth available: a region of the selected tree no analyzer entered, and the
/// rule that closed it. Reporting it is what keeps that loss from being silent.
/// A readmitted path was measured, and an ignored file that is no Python
/// candidate was never going to be, so neither withholds anything.
fn suppressed_candidates(
    jurisdiction: &Jurisdiction,
    ignore: &Ignore,
    provenance: &[HygieneProvenance],
    started: Instant,
) -> Result<Vec<SuppressedPath>, HygieneAnalysisError> {
    let mut suppressed = Vec::new();
    for entry in jurisdiction.entries() {
        check_capture_time(started, entry.path())?;
        let Admission::Ignored(rule) = entry.admission() else {
            continue;
        };
        let shape = entry.shape();
        match shape {
            Shape::File if is_python_path(entry.path()) => {}
            Shape::Directory => {}
            Shape::File | Shape::Other => continue,
        }
        let Some((rule, provenance)) = ignore.rules().get(rule).zip(provenance.get(rule)) else {
            return Err(HygieneAnalysisError::Source(format!(
                "hygiene walk decided {} by rule {rule} but the composed policy has no such rule",
                entry.path()
            )));
        };
        suppressed.push(SuppressedPath {
            path: entry.path().to_owned(),
            shape: shape.as_str().to_owned(),
            rule: rule.to_string(),
            provenance: provenance.clone(),
        });
    }
    Ok(suppressed)
}

/// Compose the hygiene walk policy and the origin of each rule in it.
///
/// The returned provenance is index-aligned with [`Ignore::rules`], which is
/// what an [`Admission::Ignored`] index means. Three ordered ranges make it up:
/// the compiled walk defaults, the tracked `gate.walk.skip` entries, and the
/// compiled per-arm excludes appended here.
fn hygiene_ignore(
    contract: &ParsedContract,
) -> Result<(Ignore, Vec<HygieneProvenance>), HygieneAnalysisError> {
    let mut rules = contract.policy().ignore().rules().to_vec();
    let mut provenance = walk_provenance(rules.len(), contract.packaged_walk_rules());
    let mut compiled = BTreeSet::new();
    for declaration in HYGIENE_ARMS {
        for exclusion in declaration.jurisdiction().excludes() {
            if *exclusion == super::TRACKED_EXCLUSION_SOURCE || !compiled.insert(*exclusion) {
                continue;
            }
            let rule = Rule::parse(exclusion)
                .map_err(|error| {
                    HygieneAnalysisError::Source(format!(
                        "compiled hygiene exclusion {exclusion:?}: {error}"
                    ))
                })?
                .ok_or_else(|| {
                    HygieneAnalysisError::Source(format!(
                        "compiled hygiene exclusion {exclusion:?} has no rule"
                    ))
                })?;
            rules.push(rule);
            provenance.push(compiled_arm_provenance());
        }
    }
    Ok((Ignore::from_rules(rules), provenance))
}

/// Attribute each walk rule to the layer that supplied it.
///
/// The first `packaged` rules come from the rack policy and the rest are
/// tracked, so a tracked rule's own declaration index is its distance past the
/// boundary. Nothing downstream of the composed list can recover that boundary.
fn walk_provenance(rules: usize, packaged: usize) -> Vec<HygieneProvenance> {
    (0..rules)
        .map(|index| match index.checked_sub(packaged) {
            Some(index) => tracked_walk_provenance(index),
            None => packaged_walk_provenance(index),
        })
        .collect()
}

fn packaged_walk_provenance(index: usize) -> HygieneProvenance {
    HygieneProvenance {
        release: CURRENT_RELEASE.to_owned(),
        layer: "packaged".to_owned(),
        document: crate::policy::PACKAGED_POLICY_SOURCE.to_owned(),
        field: format!("gate.walk.skip[{index}]"),
    }
}

fn tracked_walk_provenance(index: usize) -> HygieneProvenance {
    HygieneProvenance {
        release: CURRENT_RELEASE.to_owned(),
        layer: "repository".to_owned(),
        document: wire::TRACKED_POLICY_FILE.to_owned(),
        field: format!("gate.walk.skip[{index}]"),
    }
}

fn compiled_arm_provenance() -> HygieneProvenance {
    HygieneProvenance {
        release: CURRENT_RELEASE.to_owned(),
        layer: COMPILED_LAYER.to_owned(),
        document: COMPILED_DOCUMENT.to_owned(),
        field: "gate::hygiene::HygieneJurisdiction::excludes".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packaged policy prunes whole directories, and a pruned directory is a
    /// suppression, so a repository that tracks a path under `target/` reaches
    /// this half through a real acquisition. What stays here is the arithmetic
    /// over a boundary no fixture has to arrange.
    #[test]
    fn the_boundary_decides_which_layer_owns_each_walk_rule() {
        let provenance = walk_provenance(4, 2);

        assert_eq!(
            provenance
                .iter()
                .map(|item| (item.layer.as_str(), item.field.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("packaged", "gate.walk.skip[0]"),
                ("packaged", "gate.walk.skip[1]"),
                ("repository", "gate.walk.skip[0]"),
                ("repository", "gate.walk.skip[1]"),
            ]
        );
        assert!(provenance.iter().all(HygieneProvenance::is_complete));
    }

    #[test]
    fn a_policy_of_only_packaged_defaults_attributes_nothing_to_the_repository() {
        assert!(
            walk_provenance(2, 2)
                .iter()
                .all(|item| item.layer == "packaged")
        );
        assert!(walk_provenance(0, 0).is_empty());

        let mut budget = WalkBudget::new(2, 3, Instant::now(), SOURCE_CAPTURE_DURATION);
        assert!(budget.admit("", "a").is_ok());
        assert!(budget.admit("", "bc").is_ok());
        assert!(budget.admit("", "d").is_err());
        let mut budget = WalkBudget::new(3, 3, Instant::now(), SOURCE_CAPTURE_DURATION);
        assert!(budget.admit("", "a").is_ok());
        assert!(budget.admit("", "bc").is_ok());
        assert!(budget.admit("", "d").is_err());

        let mut ast_work = crate::hygiene::analyze::AstWork::new(Instant::now(), u64::MAX);
        assert!(!ast_work.retain_text(usize::MAX));
        assert!(
            !ast_work.retain_text(1),
            "AST exhaustion must stay terminal"
        );

        #[cfg(unix)]
        {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "gate-hygiene-source-movement-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).expect("create movement fixture");
            let source = root.join("source.py");
            let displaced = root.join("captured.py");
            std::fs::write(&source, b"original = 1\n").expect("write original source");
            let mut total = 0;
            read_source_with(&source, "source.py", &mut total, Instant::now(), || {
                std::fs::rename(&source, &displaced).expect("displace opened source");
                std::fs::write(&source, b"replacement = 2\n").expect("write replacement source");
            })
            .expect_err("source substitution refuses");
            std::fs::remove_dir_all(root).expect("remove movement fixture");
        }
    }
}
