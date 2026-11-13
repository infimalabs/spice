//! Which paths a gate run is allowed to speak about.
//!
//! A gate that measures whatever it happens to find measures something
//! different on every machine, and a number nobody can reproduce cannot hold a
//! ratchet up. Jurisdiction is the answer to that: one ignore policy, one walk
//! whose result is a total function of the tree, and one recorded reason for
//! every path the walk reached. Refused paths are recorded exactly as carefully
//! as admitted ones, because a measurement's silence is only trustworthy when
//! the record can say what filled it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::cell::RefCell;
#[cfg(target_family = "unix")]
use std::os::unix::fs::MetadataExt as _;
#[cfg(not(target_family = "unix"))]
use std::time::SystemTime;

use wire::{Digest, Domain};

/// What the walk found at one path.
///
/// Symlinks are [`Shape::Other`] rather than resolved. Following them would let
/// a jurisdiction reach outside its own root and would let a cycle end the
/// walk, and neither outcome is one a deterministic record can survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shape {
    /// A regular file, the only shape a metric can measure.
    File,
    /// A directory, walked when admitted and pruned when not.
    Directory,
    /// Anything else, recorded and never descended or measured.
    Other,
}

impl Shape {
    /// The one spelling this shape takes in an inventory.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why one path is inside or outside a jurisdiction.
///
/// Each variant names the rule that decided, so an inventory line is enough to
/// replay the decision against the policy without re-walking anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Admission {
    /// No rule named this path, so the default admits it.
    Unnamed,
    /// The last rule to match re-admitted it.
    Readmitted(usize),
    /// The last rule to match excluded it.
    Ignored(usize),
}

impl Admission {
    /// Whether this path is inside the jurisdiction.
    #[must_use]
    pub const fn admits(self) -> bool {
        match self {
            Self::Unnamed | Self::Readmitted(_) => true,
            Self::Ignored(_) => false,
        }
    }

    /// The rule that decided, absent when the default did.
    #[must_use]
    pub const fn rule(self) -> Option<usize> {
        match self {
            Self::Unnamed => None,
            Self::Readmitted(rule) | Self::Ignored(rule) => Some(rule),
        }
    }
}

impl fmt::Display for Admission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unnamed => formatter.write_str("unnamed"),
            Self::Readmitted(rule) => write!(formatter, "readmitted:{rule}"),
            Self::Ignored(rule) => write!(formatter, "ignored:{rule}"),
        }
    }
}

/// One path the walk reached and what the policy said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    path: String,
    shape: Shape,
    admission: Admission,
}

impl Entry {
    /// Path relative to the jurisdiction root, always `/`-separated.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the walk found here.
    #[must_use]
    pub const fn shape(&self) -> Shape {
        self.shape
    }

    /// Why this path is in or out.
    #[must_use]
    pub const fn admission(&self) -> Admission {
        self.admission
    }

    /// Whether a metric may measure this path.
    #[must_use]
    pub const fn measurable(&self) -> bool {
        self.admission.admits() && matches!(self.shape, Shape::File)
    }
}

/// One ignore-file line, holding the shape of the match it performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    source: String,
    pattern: gix_glob::Pattern,
}

impl Rule {
    /// Parse one line, yielding nothing for blanks and comments.
    ///
    /// # Errors
    ///
    /// Returns the reason this line is not one of the admitted spellings.
    pub fn parse(line: &str) -> Result<Option<Self>, IgnoreError> {
        let refuse = |reason| Err(IgnoreError::new(line, reason));
        if line.trim() != line {
            return refuse(Malady::SurroundingSpace);
        }
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }

        if line.contains(['\r', '\n']) {
            return refuse(Malady::LineBreak);
        }
        let Some(pattern) = gix_glob::Pattern::from_bytes(line.as_bytes()) else {
            return refuse(Malady::EmptyPattern);
        };

        Ok(Some(Self {
            source: line.to_owned(),
            pattern,
        }))
    }

    /// Whether this rule re-admits rather than excludes.
    #[must_use]
    pub fn negated(&self) -> bool {
        self.pattern.is_negative()
    }

    /// Whether this rule matches a path of this shape.
    #[must_use]
    pub fn matches(&self, path: &str, shape: Shape) -> bool {
        self.pattern.matches_repo_relative_path(
            path.as_bytes().into(),
            path.rfind('/').map(|slash| slash + 1),
            Some(matches!(shape, Shape::Directory)),
            gix_glob::pattern::Case::Sensitive,
            gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
        )
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

/// The ordered ignore policy one jurisdiction answers to.
///
/// The last rule to match decides, which is what lets a broad exclusion be
/// narrowed by a later `!` line without the two orders meaning the same thing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ignore {
    rules: Vec<Rule>,
}

impl Ignore {
    /// Build a policy from already admitted rules in decision order.
    #[must_use]
    pub fn from_rules(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// Parse an ignore file.
    ///
    /// # Errors
    ///
    /// Returns the first line that is not an admitted spelling, with that line
    /// as evidence.
    pub fn parse(text: &str) -> Result<Self, IgnoreError> {
        let mut rules = Vec::new();
        for line in text.lines() {
            if let Some(rule) = Rule::parse(line)? {
                rules.push(rule);
            }
        }
        Ok(Self { rules })
    }

    /// The rules in the order they decide.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// What this policy says about one path.
    #[must_use]
    pub fn decide(&self, path: &str, shape: Shape) -> Admission {
        let mut decision = Admission::Unnamed;
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.matches(path, shape) {
                decision = if rule.negated() {
                    Admission::Readmitted(index)
                } else {
                    Admission::Ignored(index)
                };
            }
        }
        decision
    }

    /// The canonical rendering of this policy, one rule per line.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = String::new();
        for rule in &self.rules {
            let _ = writeln!(rendered, "{rule}");
        }
        rendered
    }
}

/// Every path one walk reached, with the reason each is in or out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jurisdiction {
    root: PathBuf,
    entries: Vec<Entry>,
}

/// Fixed work admitted while constructing one bounded jurisdiction.
pub(crate) struct WalkBudget {
    entry_limit: usize,
    path_bytes_limit: usize,
    entries: usize,
    path_bytes: usize,
    started: Instant,
    duration: Duration,
}

impl WalkBudget {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) const fn new(
        entry_limit: usize,
        path_bytes_limit: usize,
        started: Instant,
        duration: Duration,
    ) -> Self {
        Self {
            entry_limit,
            path_bytes_limit,
            entries: 0,
            path_bytes: 0,
            started,
            duration,
        }
    }

    pub(crate) fn admit(&mut self, relative: &str, name: &str) -> Result<(), WalkError> {
        self.check_elapsed()?;
        if self.entries == self.entry_limit {
            return Err(WalkError::Bounded {
                detail: format!("jurisdiction entry limit {} was exceeded", self.entry_limit),
            });
        }
        let separator = usize::from(!relative.is_empty());
        let bytes = relative
            .len()
            .saturating_add(separator)
            .saturating_add(name.len());
        let total = self.path_bytes.saturating_add(bytes);
        if total > self.path_bytes_limit {
            return Err(WalkError::Bounded {
                detail: format!("jurisdiction path bytes exceeded {}", self.path_bytes_limit),
            });
        }
        self.entries += 1;
        self.path_bytes = total;
        Ok(())
    }

    fn check_elapsed(&self) -> Result<(), WalkError> {
        if self.started.elapsed() <= self.duration {
            Ok(())
        } else {
            Err(WalkError::Bounded {
                detail: format!("jurisdiction walk exceeded {:?}", self.duration),
            })
        }
    }
}

/// One private materialization of the admitted bytes a run measures.
pub(crate) struct Snapshot {
    root: SnapshotRoot,
    jurisdiction: Jurisdiction,
    tree: Digest,
    seals: Vec<Seal>,
    changes: SnapshotChangeSeal,
}

pub(crate) struct SnapshotChange {
    pub(crate) path: String,
    pub(crate) tree: Digest,
    pub(crate) observed_tree: Option<Digest>,
}

struct SnapshotRoot {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
struct SnapshotChangeSeal {
    inotify: RefCell<inotify::Inotify>,
    watched: Vec<(inotify::WatchDescriptor, String, Shape)>,
}

#[cfg(not(target_os = "linux"))]
struct SnapshotChangeSeal;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seal {
    path: String,
    shape: Shape,
    fingerprint: Fingerprint,
}

#[cfg(target_family = "unix")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[cfg(not(target_family = "unix"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    length: u64,
    readonly: bool,
    modified: Option<SystemTime>,
}

impl Jurisdiction {
    /// Walk a foreign repository under one ignore policy.
    ///
    /// The result is a total function of the tree: children are read in sorted
    /// order, excluded directories are pruned but recorded, and the entries are
    /// sorted by path before they are returned, so traversal order cannot leak
    /// into the inventory.
    ///
    /// # Errors
    ///
    /// Returns the failure that stopped the walk, naming the path it happened
    /// at.
    pub fn walk(root: &Path, ignore: &Ignore) -> Result<Self, WalkError> {
        Self::walk_with_budget(root, ignore, None)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn walk_bounded(
        root: &Path,
        ignore: &Ignore,
        budget: &mut WalkBudget,
    ) -> Result<Self, WalkError> {
        Self::walk_with_budget(root, ignore, Some(budget))
    }

    fn walk_with_budget(
        root: &Path,
        ignore: &Ignore,
        mut budget: Option<&mut WalkBudget>,
    ) -> Result<Self, WalkError> {
        let mut entries = Vec::new();
        let mut directories = vec![String::new()];

        while let Some(relative) = directories.pop() {
            if let Some(budget) = budget.as_deref() {
                budget.check_elapsed()?;
            }
            for (name, shape) in read_children(root, &relative, budget.as_deref_mut())? {
                let path = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                let admission = ignore.decide(&path, shape);

                if matches!(shape, Shape::Directory) && admission.admits() {
                    directories.push(path.clone());
                }
                entries.push(Entry {
                    path,
                    shape,
                    admission,
                });
            }
        }

        if let Some(budget) = budget.as_deref() {
            budget.check_elapsed()?;
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        if let Some(budget) = budget.as_deref() {
            budget.check_elapsed()?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            entries,
        })
    }

    /// Walk only the regular paths captured from one Git index.
    ///
    /// Parent directories are derived from the captured paths and inspected
    /// directly. Directory listings are never consulted, so an untracked name
    /// cannot enter the inventory or perturb a tracked read while it changes.
    /// Ignore rules still decide whether a tracked path is measurable.
    ///
    /// # Errors
    ///
    /// Returns the first tracked path or derived parent that cannot be named or
    /// inspected.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn walk_tracked(
        root: &Path,
        ignore: &Ignore,
        tracked: &[String],
    ) -> Result<Self, WalkError> {
        let mut candidates = BTreeMap::<String, bool>::new();
        for file in tracked {
            candidates.insert(file.clone(), true);
            let mut parent = Path::new(file).parent();
            while let Some(path) = parent {
                if path.as_os_str().is_empty() {
                    break;
                }
                let path = path.to_str().ok_or_else(|| WalkError::Unnameable {
                    path: root.join(path),
                })?;
                candidates.entry(path.to_owned()).or_insert(false);
                parent = Path::new(path).parent();
            }
        }
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort_by(|(left, _), (right, _)| {
            left.matches('/')
                .count()
                .cmp(&right.matches('/').count())
                .then_with(|| left.cmp(right))
        });

        let mut entries = Vec::with_capacity(candidates.len());
        let mut pruned = Vec::<String>::new();
        for (path, expected_file) in candidates {
            if pruned.iter().any(|parent| {
                path.strip_prefix(parent)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            }) {
                continue;
            }
            let absolute = root.join(&path);
            let metadata = fs::symlink_metadata(&absolute)
                .map_err(|source| WalkError::io("inspecting a tracked path", &absolute, source))?;
            let shape = if metadata.is_dir() {
                Shape::Directory
            } else if metadata.is_file() {
                Shape::File
            } else {
                Shape::Other
            };
            let admission = ignore.decide(&path, shape);
            if !matches!(shape, Shape::Directory) || !admission.admits() || expected_file {
                pruned.push(path.clone());
            }
            entries.push(Entry {
                path,
                shape,
                admission,
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self {
            root: root.to_path_buf(),
            entries,
        })
    }

    /// The root every entry path is relative to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every path the walk reached, sorted, admitted and refused alike.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The admitted regular files, in path order.
    pub fn measurable(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.measurable())
    }

    /// The canonical inventory: one line per reached path, reason first.
    ///
    /// The path is last because a path may contain spaces and the reason and
    /// shape may not, which makes every line readable by splitting twice.
    #[must_use]
    pub fn inventory(&self) -> String {
        let mut rendered = String::new();
        for entry in &self.entries {
            let _ = writeln!(
                rendered,
                "{} {} {}",
                entry.admission, entry.shape, entry.path
            );
        }
        rendered
    }

    /// Digest of the inventory, covering refusals as well as admissions.
    #[must_use]
    pub fn inventory_digest(&self) -> Digest {
        Digest::compute(Domain::GateInventory, self.inventory().as_bytes())
    }

    /// Digest of what the admitted files contain.
    ///
    /// Each file is digested under its own domain and the listing of those
    /// digests is digested under another, so a file whose bytes happen to spell
    /// a listing cannot be mistaken for one.
    ///
    /// # Errors
    ///
    /// Returns the read failure, naming the file it happened at.
    pub fn tree_digest(&self) -> Result<Digest, WalkError> {
        let mut listing = String::new();
        for entry in self.measurable() {
            let path = self.root.join(&entry.path);
            let bytes = fs::read(&path)
                .map_err(|source| WalkError::io("reading an admitted file", &path, source))?;
            let _ = writeln!(
                listing,
                "{} {}",
                Digest::compute(Domain::GateFile, &bytes),
                entry.path
            );
        }
        Ok(Digest::compute(Domain::GateTree, listing.as_bytes()))
    }

    /// Every inventory or measurable-content path that differs in another walk.
    pub(crate) fn changed_paths(&self, observed: &Self) -> Result<Vec<String>, WalkError> {
        let before = self
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let after = observed
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect::<BTreeMap<_, _>>();
        let names = before
            .keys()
            .chain(after.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let mut changed = Vec::new();
        for name in names {
            let (Some(left), Some(right)) = (before.get(name), after.get(name)) else {
                changed.push(name.to_owned());
                continue;
            };
            if *left != *right {
                changed.push(name.to_owned());
                continue;
            }
            if left.measurable() {
                let left_path = self.root.join(name);
                let right_path = observed.root.join(name);
                let left_bytes = fs::read(&left_path).map_err(|source| {
                    WalkError::io("reading captured jurisdiction contents", &left_path, source)
                })?;
                let right_bytes = fs::read(&right_path).map_err(|source| {
                    WalkError::io(
                        "reading observed jurisdiction contents",
                        &right_path,
                        source,
                    )
                })?;
                if left_bytes != right_bytes {
                    changed.push(name.to_owned());
                }
            }
        }
        Ok(changed)
    }

    pub(crate) fn snapshot(&self) -> Result<Snapshot, WalkError> {
        Snapshot::capture(self)
    }
}

impl Snapshot {
    fn capture(source: &Jurisdiction) -> Result<Self, WalkError> {
        let root = SnapshotRoot::create()?;
        for entry in source.measurable() {
            let original = source.root.join(entry.path());
            let captured = root.path.join(entry.path());
            if let Some(parent) = captured.parent() {
                fs::create_dir_all(parent).map_err(|source| {
                    WalkError::io("creating a snapshot directory", parent, source)
                })?;
            }
            let bytes = fs::read(&original)
                .map_err(|source| WalkError::io("capturing an admitted file", &original, source))?;
            let permissions = fs::metadata(&original)
                .map_err(|source| WalkError::io("inspecting an admitted file", &original, source))?
                .permissions();
            fs::write(&captured, bytes)
                .map_err(|source| WalkError::io("writing a snapshot file", &captured, source))?;
            fs::set_permissions(&captured, permissions).map_err(|source| {
                WalkError::io("preserving snapshot permissions", &captured, source)
            })?;
        }

        let jurisdiction = Jurisdiction {
            root: root.path.clone(),
            entries: source.entries.clone(),
        };
        let tree = jurisdiction.tree_digest()?;
        let seals = seal_tree(&root.path)?;
        let changes = SnapshotChangeSeal::capture(&root.path, &seals)?;
        Ok(Self {
            root,
            jurisdiction,
            tree,
            seals,
            changes,
        })
    }

    pub(crate) const fn jurisdiction(&self) -> &Jurisdiction {
        &self.jurisdiction
    }

    pub(crate) fn tree(&self) -> Digest {
        self.tree.clone()
    }

    pub(crate) fn verify(&self) -> Result<Option<SnapshotChange>, WalkError> {
        if let Some(path) = self.changes.changed(&self.root.path)? {
            return Ok(Some(SnapshotChange {
                path,
                tree: self.tree.clone(),
                observed_tree: self.jurisdiction.tree_digest().ok(),
            }));
        }
        let observed_seals = seal_tree(&self.root.path)?;
        if observed_seals != self.seals {
            return Ok(Some(SnapshotChange {
                path: first_changed_path(&self.seals, &observed_seals),
                tree: self.tree.clone(),
                observed_tree: self.jurisdiction.tree_digest().ok(),
            }));
        }
        if let Some(path) = self.changes.changed(&self.root.path)? {
            return Ok(Some(SnapshotChange {
                path,
                tree: self.tree.clone(),
                observed_tree: self.jurisdiction.tree_digest().ok(),
            }));
        }

        let observed_tree = self.jurisdiction.tree_digest()?;
        Ok((observed_tree != self.tree).then_some(SnapshotChange {
            path: "admitted contents".to_owned(),
            tree: self.tree.clone(),
            observed_tree: Some(observed_tree),
        }))
    }
}

impl SnapshotChangeSeal {
    fn capture(root: &Path, seals: &[Seal]) -> Result<Self, WalkError> {
        #[cfg(target_os = "linux")]
        return Self::capture_linux(root, seals);
        #[cfg(not(target_os = "linux"))]
        Self::capture_portable(root, seals)
    }

    fn changed(&self, root: &Path) -> Result<Option<String>, WalkError> {
        #[cfg(target_os = "linux")]
        return self.changed_linux(root);
        #[cfg(not(target_os = "linux"))]
        self.changed_portable(root)
    }
}

#[cfg(target_os = "linux")]
impl SnapshotChangeSeal {
    fn capture_linux(root: &Path, seals: &[Seal]) -> Result<Self, WalkError> {
        let inotify = inotify::Inotify::init()
            .map_err(|source| WalkError::io("creating a snapshot change seal", root, source))?;
        let mask = inotify::WatchMask::ATTRIB
            | inotify::WatchMask::CLOSE_WRITE
            | inotify::WatchMask::CREATE
            | inotify::WatchMask::DELETE
            | inotify::WatchMask::DELETE_SELF
            | inotify::WatchMask::MODIFY
            | inotify::WatchMask::MOVE
            | inotify::WatchMask::MOVE_SELF;
        let mut watched = Vec::with_capacity(seals.len());
        for seal in seals {
            let path = if seal.path == "." {
                root.to_path_buf()
            } else {
                root.join(&seal.path)
            };
            let descriptor = inotify
                .watches()
                .add(&path, mask)
                .map_err(|source| WalkError::io("watching a snapshot path", &path, source))?;
            watched.push((descriptor, seal.path.clone(), seal.shape));
        }
        Ok(Self {
            inotify: RefCell::new(inotify),
            watched,
        })
    }

    fn changed_linux(&self, root: &Path) -> Result<Option<String>, WalkError> {
        let mut inotify = self.inotify.borrow_mut();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match inotify.read_events(&mut buffer) {
                Ok(events) => {
                    let mut events = events;
                    if let Some(event) = events.next() {
                        if event.mask.intersects(
                            inotify::EventMask::Q_OVERFLOW | inotify::EventMask::UNMOUNT,
                        ) {
                            return Ok(Some("snapshot".to_owned()));
                        }
                        let Some((_, relative, shape)) = self
                            .watched
                            .iter()
                            .find(|(descriptor, _, _)| descriptor == &event.wd)
                        else {
                            return Ok(Some("snapshot".to_owned()));
                        };
                        if event.mask.contains(inotify::EventMask::IGNORED) {
                            return Ok(Some(relative.clone()));
                        }
                        if *shape == Shape::Directory
                            && let Some(name) = event.name
                        {
                            let Some(name) = name.to_str() else {
                                return Ok(Some("snapshot".to_owned()));
                            };
                            return Ok(Some(if relative == "." {
                                name.to_owned()
                            } else {
                                format!("{relative}/{name}")
                            }));
                        }
                        return Ok(Some(relative.clone()));
                    }
                }
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    return Err(WalkError::io(
                        "reading the snapshot change seal",
                        root,
                        source,
                    ));
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl SnapshotChangeSeal {
    fn capture_portable(_root: &Path, _seals: &[Seal]) -> Result<Self, WalkError> {
        Ok(Self)
    }

    fn changed_portable(&self, _root: &Path) -> Result<Option<String>, WalkError> {
        Ok(None)
    }
}

impl SnapshotRoot {
    fn create() -> Result<Self, WalkError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = std::env::temp_dir();
        for _ in 0..128 {
            let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = temporary.join(format!(
                "spice-gate-snapshot-{}-{nonce}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(WalkError::io(
                        "creating a jurisdiction snapshot",
                        &path,
                        source,
                    ));
                }
            }
        }
        Err(WalkError::io(
            "creating a jurisdiction snapshot",
            &temporary,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "snapshot name attempts exhausted",
            ),
        ))
    }
}

impl Drop for SnapshotRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn seal_tree(root: &Path) -> Result<Vec<Seal>, WalkError> {
    let mut seals = vec![seal(root, ".", Shape::Directory)?];
    let mut directories = vec![String::new()];
    while let Some(relative) = directories.pop() {
        for (name, shape) in read_children(root, &relative, None)? {
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            seals.push(seal(&root.join(&path), &path, shape)?);
            if matches!(shape, Shape::Directory) {
                directories.push(path);
            }
        }
    }
    seals.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(seals)
}

fn seal(path: &Path, relative: &str, shape: Shape) -> Result<Seal, WalkError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| WalkError::io("sealing a snapshot path", path, source))?;
    Ok(Seal {
        path: relative.to_owned(),
        shape,
        fingerprint: fingerprint(&metadata),
    })
}

fn fingerprint(metadata: &fs::Metadata) -> Fingerprint {
    #[cfg(target_family = "unix")]
    {
        Fingerprint {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    #[cfg(not(target_family = "unix"))]
    {
        Fingerprint {
            length: metadata.len(),
            readonly: metadata.permissions().readonly(),
            modified: metadata.modified().ok(),
        }
    }
}

fn first_changed_path(before: &[Seal], after: &[Seal]) -> String {
    for index in 0..before.len().max(after.len()) {
        match (before.get(index), after.get(index)) {
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) => return left.path.clone().min(right.path.clone()),
            (Some(left), None) => return left.path.clone(),
            (None, Some(right)) => return right.path.clone(),
            (None, None) => break,
        }
    }
    "snapshot".to_owned()
}

/// Read one directory's children, sorted, refusing names an inventory cannot
/// carry.
fn read_children(
    root: &Path,
    relative: &str,
    mut budget: Option<&mut WalkBudget>,
) -> Result<Vec<(String, Shape)>, WalkError> {
    let directory = if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let listing = fs::read_dir(&directory)
        .map_err(|source| WalkError::io("opening a directory", &directory, source))?;

    let mut children = Vec::new();
    for child in listing {
        if let Some(budget) = budget.as_deref() {
            budget.check_elapsed()?;
        }
        let child =
            child.map_err(|source| WalkError::io("reading a directory", &directory, source))?;
        let name = child
            .file_name()
            .into_string()
            .map_err(|name| WalkError::Unnameable {
                path: directory.join(name),
            })?;
        if name.contains('\n') {
            return Err(WalkError::Unnameable {
                path: directory.join(&name),
            });
        }
        if let Some(budget) = budget.as_deref_mut() {
            budget.admit(relative, &name)?;
        }

        let path = child.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| WalkError::io("inspecting a path", &path, source))?;
        let shape = if metadata.is_dir() {
            Shape::Directory
        } else if metadata.is_file() {
            Shape::File
        } else {
            Shape::Other
        };

        children.push((name, shape));
    }

    if let Some(budget) = budget.as_deref() {
        budget.check_elapsed()?;
    }
    children.sort();
    if let Some(budget) = budget.as_deref() {
        budget.check_elapsed()?;
    }
    Ok(children)
}

/// What is wrong with one ignore-file line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malady {
    /// The line carries leading or trailing space, so it has two spellings.
    SurroundingSpace,
    /// The line names no pattern at all.
    EmptyPattern,
    /// One array entry attempted to carry more than one ignore line.
    LineBreak,
}

impl fmt::Display for Malady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SurroundingSpace => {
                formatter.write_str("carries surrounding space, so it has more than one spelling")
            }
            Self::EmptyPattern => formatter.write_str("names no pattern"),
            Self::LineBreak => formatter.write_str("contains a line break"),
        }
    }
}

/// A line that is not one of the admitted ignore spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreError {
    line: String,
    malady: Malady,
}

impl IgnoreError {
    fn new(line: &str, malady: Malady) -> Self {
        Self {
            line: line.to_owned(),
            malady,
        }
    }

    /// The exact line that was refused.
    #[must_use]
    pub fn line(&self) -> &str {
        &self.line
    }

    /// Why it was refused.
    #[must_use]
    pub const fn malady(&self) -> Malady {
        self.malady
    }
}

impl fmt::Display for IgnoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ignore line {:?} {}", self.line, self.malady)
    }
}

impl std::error::Error for IgnoreError {}

/// What stopped a walk.
#[derive(Debug)]
pub enum WalkError {
    /// The filesystem refused an operation the walk needed.
    Io {
        /// What the walk was doing.
        operation: &'static str,
        /// Where it was doing it.
        path: PathBuf,
        /// What the filesystem said.
        source: io::Error,
    },
    /// A name no canonical inventory line could carry.
    Unnameable {
        /// The path that cannot be written down.
        path: PathBuf,
    },
    /// A bounded caller exhausted its declared walk work.
    Bounded {
        /// Stable bounded detail suitable for caller diagnostics.
        detail: String,
    },
}

impl WalkError {
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for WalkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation} at {} failed: {source}",
                path.display()
            ),
            Self::Unnameable { path } => write!(
                formatter,
                "{} cannot be written into a canonical inventory",
                path.display()
            ),
            Self::Bounded { detail } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Unnameable { .. } | Self::Bounded { .. } => None,
        }
    }
}
