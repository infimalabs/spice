//! Debt sweeps: the Plan `spice sweep` would file from the latch catalogue
//! and the arm-time ledger, and which subjects a filed sweep task covers.
//!
//! `spice sweep --apply` files one Plan with one child per file and one per
//! slow arm, and records here the subjects each child covers. The record says
//! what was filed; the journal alone says whether that child is still pending,
//! so a cover whose task settled, was deleted, or never committed holds
//! nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use gate::{LatchEntry, LatchOrigin, StateLedger};

use super::arms::ArmTiming;
use serde::{Deserialize, Serialize};
use wire::intent::{SweepPreview, SweepText};
use wire::{Instant, TaskHandle};

use super::{AmbientClaimError, RunError, ambient_claim};
use crate::ambient::Ambient;
use crate::repository::StatePaths;

/// The topic every sweep task is filed in.
pub fn sweep_topic() -> wire::Topic {
    wire::Topic::root("gate").expect("gate is one topic component")
}

/// Wall seconds an arm's median accepted run may take before the sweep files
/// its repair.
pub const ARM_SECONDS_BOUND: u64 = 60;

const COVER_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverLedger {
    version: u32,
    covers: Vec<Cover>,
}

/// The subjects one filed sweep task covers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cover {
    pub task: TaskHandle,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latches: Vec<LatchEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arms: Vec<String>,
}

impl Cover {
    /// The cover `task` holds for one drafted child.
    #[must_use]
    pub fn new(task: TaskHandle, subjects: Subjects) -> Self {
        Self {
            task,
            latches: subjects.latches,
            arms: subjects.arms,
        }
    }
}

/// Latched fingerprints and slow arms, as one child covers them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Subjects {
    pub latches: Vec<LatchEntry>,
    pub arms: Vec<String>,
}

/// Every subject pending sweep tasks hold, each naming its task.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Held {
    pub latches: BTreeMap<LatchEntry, TaskHandle>,
    pub arms: BTreeMap<String, TaskHandle>,
}

/// Every recorded cover; an absent ledger records none.
///
/// # Errors
///
/// Returns the read failure, or `InvalidData` for bytes that are not one
/// current cover ledger.
pub fn read_covers(path: &Path) -> io::Result<Vec<Cover>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let ledger: CoverLedger =
        wire::decode(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if ledger.version != COVER_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "sweep cover ledger version {} is not {COVER_VERSION}",
                ledger.version
            ),
        ));
    }
    Ok(ledger.covers)
}

/// Replace the cover ledger in one atomic publication.
///
/// # Errors
///
/// Returns the encoding or publication failure.
pub fn publish_covers(path: &Path, covers: Vec<Cover>) -> io::Result<()> {
    let bytes = wire::encode(&CoverLedger {
        version: COVER_VERSION,
        covers,
    })
    .map_err(io::Error::other)?;
    atomic::Destination::new(path)
        .map_err(io::Error::other)?
        .publish(|staged| staged.write_all(&bytes))
        .map_err(io::Error::other)
}

/// The covers whose task the journal holds pending.
///
/// # Errors
///
/// Returns the journal open or read failure.
pub fn pending_tasks(journal: &Path, covers: &[Cover]) -> Result<BTreeSet<TaskHandle>, String> {
    let reader = journal::JournalReader::open(journal).map_err(|error| error.to_string())?;
    reader
        .read(|read| {
            let mut pending = BTreeSet::new();
            for cover in covers {
                let Ok(born) = cover.task.born() else {
                    continue;
                };
                if read
                    .task_heads_for(&born)?
                    .is_some_and(|heads| heads.workflow().body().status.is_pending())
                {
                    pending.insert(cover.task.clone());
                }
            }
            Ok(pending)
        })
        .map_err(|error| error.to_string())
}

/// Each subject a pending cover holds, except those `claimed` covers.
///
/// A sweep child's own claimant still faces the bounds it was filed to heal;
/// everyone else's finding on a held latch informs and passes.
#[must_use]
pub fn held(
    covers: &[Cover],
    pending: impl Fn(&TaskHandle) -> bool,
    claimed: Option<&TaskHandle>,
) -> Held {
    let mut held = Held::default();
    for cover in covers
        .iter()
        .filter(|cover| Some(&cover.task) != claimed && pending(&cover.task))
    {
        held.latches.extend(
            cover
                .latches
                .iter()
                .map(|latch| (latch.clone(), cover.task.clone())),
        );
        held.arms.extend(
            cover
                .arms
                .iter()
                .map(|arm| (arm.clone(), cover.task.clone())),
        );
    }
    held
}

/// The latches pending sweep tasks hold for this process's gate run.
///
/// An absent cover ledger costs no journal read.
pub(super) fn rim_covers(
    state: &StatePaths,
    ambient: &Ambient,
) -> Result<BTreeMap<LatchEntry, TaskHandle>, RunError> {
    let covers = read_covers(state.gate_sweep())
        .map_err(|error| RunError::refusal("gate::sweep-covers-refused", error))?;
    if covers.is_empty() {
        return Ok(BTreeMap::new());
    }
    let claimed = match ambient_claim(state, ambient) {
        Ok(claim) => claim.map(|(task, _)| task),
        Err(AmbientClaimError::Invalid(detail)) => {
            return Err(RunError::refusal("gate::lane-identity-invalid", detail));
        }
        Err(AmbientClaimError::Unavailable(detail)) => {
            return Err(RunError::fault("gate::sweep-covers-unavailable", detail));
        }
    };
    let pending = pending_tasks(state.journal(), &covers)
        .map_err(|error| RunError::fault("gate::sweep-covers-unavailable", error))?;
    Ok(held(&covers, |task| pending.contains(task), claimed.as_ref()).latches)
}

/// What one sweep would file now.
#[derive(Debug)]
pub struct SweepDraft {
    pub content: SweepPreview,
    /// The subjects each child covers, in `content.children` order.
    pub covers: Vec<Subjects>,
    /// Latched fingerprints and slow arms a pending sweep task already covers.
    pub held: usize,
}

impl SweepDraft {
    /// The child covering `arm`, when this draft files one.
    #[must_use]
    pub fn arm_child(&self, arm: &str) -> Option<usize> {
        self.covers
            .iter()
            .position(|subjects| subjects.arms.iter().any(|covered| covered == arm))
    }
}

/// Group every latch no pending sweep task holds by file, then every arm over
/// [`ARM_SECONDS_BOUND`] no pending sweep task holds: one child each.
#[must_use]
pub fn draft(
    ledger: &StateLedger,
    arms: &[ArmTiming],
    held: &Held,
    until: Option<Instant>,
) -> SweepDraft {
    let mut files: BTreeMap<&str, Vec<(&LatchEntry, Option<&LatchOrigin>)>> = BTreeMap::new();
    let mut covered = 0;
    for (latch, origin) in ledger.catalogue() {
        if held.latches.contains_key(latch) {
            covered += 1;
        } else {
            files
                .entry(latch.key().repo_path())
                .or_default()
                .push((latch, origin));
        }
    }
    let mut slow = Vec::new();
    for timing in arms
        .iter()
        .filter(|timing| timing.seconds() > ARM_SECONDS_BOUND)
    {
        if held.arms.contains_key(&timing.arm) {
            covered += 1;
        } else {
            slow.push(timing);
        }
    }
    slow.sort_by(|left, right| {
        right
            .seconds()
            .cmp(&left.seconds())
            .then_with(|| left.arm.cmp(&right.arm))
    });
    let plan = plan_text(&files, &slow);
    let mut children = Vec::with_capacity(files.len() + slow.len());
    let mut covers = Vec::with_capacity(children.capacity());
    for (path, rows) in &files {
        children.push(child_text(path, rows));
        covers.push(Subjects {
            latches: rows.iter().map(|(latch, _)| (*latch).clone()).collect(),
            arms: Vec::new(),
        });
    }
    for timing in slow {
        children.push(arm_text(timing));
        covers.push(Subjects {
            latches: Vec::new(),
            arms: vec![timing.arm.clone()],
        });
    }
    SweepDraft {
        content: SweepPreview {
            topic: sweep_topic(),
            until,
            plan,
            children,
        },
        covers,
        held: covered,
    }
}

fn plan_text(
    files: &BTreeMap<&str, Vec<(&LatchEntry, Option<&LatchOrigin>)>>,
    slow: &[&ArmTiming],
) -> SweepText {
    let mut description = String::from(
        "Plan the repair of the bound debt `spice sweep` found: latched fingerprints filed as \
         one child per file so no two children touch the same file, and slow gate arms filed \
         as one child each. The children carry no inferred dependency edges: the fixes are \
         routine and mechanical, and the lanes that take them order any remaining overlap as \
         they go. Review each child's scope, then release it.",
    );
    if !files.is_empty() {
        description.push_str("\n\nFiles:");
        for (path, rows) in files {
            let _ = write!(description, "\n- {path}: {} latched", rows.len());
        }
    }
    if !slow.is_empty() {
        description.push_str("\n\nArms:");
        for timing in slow {
            let _ = write!(
                description,
                "\n- {}: {}s over {ARM_SECONDS_BOUND}s",
                timing.arm,
                timing.seconds()
            );
        }
    }
    let mut subjects = Vec::new();
    if !files.is_empty() {
        subjects.push(plural(files.len(), "file"));
    }
    if !slow.is_empty() {
        subjects.push(plural(slow.len(), "slow arm"));
    }
    SweepText {
        title: format!("Sweep bound debt across {}", subjects.join(" and ")),
        pitch: "Repairing inherited bound debt once, on purpose, stops lanes from colliding over \
                the same inline repairs."
            .to_owned(),
        description,
        term: Vec::new(),
    }
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// The child that brings one slow arm back under the bound; a Cargo test arm
/// also carries the durable-source registry constraint on splitting targets.
fn arm_text(timing: &ArmTiming) -> SweepText {
    let arm = &timing.arm;
    let mut description = format!(
        "Bring the gate arm {arm} back under the sweep bound of {ARM_SECONDS_BOUND}s wall \
         time.\n\nObserved: {}s at the median of {} accepted run{} over the {ARM_SECONDS_BOUND}s \
         bound.",
        timing.seconds(),
        timing.runs(),
        if timing.runs() == 1 { "" } else { "s" },
    );
    if arm.starts_with("cargo:test:") {
        description.push_str(
            "\n\nA change that splits or renames this test target carries the durable-source \
             registry constraint: name the new target and install it before moving tests.",
        );
    }
    SweepText {
        title: format!("Speed up the slow gate arm {arm}"),
        pitch: "A slow arm costs every lane on every commit, so repairing it once on purpose \
                pays back on every proof."
            .to_owned(),
        description,
        term: vec![format!(
            "{arm} proves within {ARM_SECONDS_BOUND}s at the median of its accepted runs"
        )],
    }
}

fn child_text(path: &str, rows: &[(&LatchEntry, Option<&LatchOrigin>)]) -> SweepText {
    let mut description = format!(
        "Heal the latched bound debt in {path} so each fingerprint below measures within its \
         base bound and leaves the latch catalogue.\n\nFingerprints, as first recorded over base:"
    );
    for (latch, origin) in rows {
        let _ = write!(description, "\n- {latch}: ");
        match origin {
            Some(origin) => provenance(&mut description, origin),
            None => description.push_str("origin unknown"),
        }
    }
    SweepText {
        title: format!("Heal latched bound debt in {path}"),
        pitch: "Repairing this file's inherited bound debt once, on purpose, stops every lane \
                that touches it from repairing it inline."
            .to_owned(),
        description,
        term: rows
            .iter()
            .map(|(latch, _)| {
                format!("{latch} measures within its base bound and is no longer latched")
            })
            .collect(),
    }
}

/// `measured M over base B, first over base at T on commit C by A`, with the
/// instant absolute so the preview digest does not drift as the debt ages.
fn provenance(text: &mut String, origin: &LatchOrigin) {
    let _ = write!(
        text,
        "measured {} over base {}, first over base at ",
        origin.measured(),
        origin.base_limit()
    );
    match jiff::SignedDuration::try_from_secs_f64(origin.at())
        .ok()
        .and_then(|elapsed| jiff::Timestamp::from_duration(elapsed).ok())
        .and_then(|at| at.round(jiff::Unit::Second).ok())
    {
        Some(at) => {
            let _ = write!(text, "{at}");
        }
        None => text.push_str("an unrecorded time"),
    }
    match origin.commit() {
        Some(commit) => {
            let _ = write!(text, " on commit {commit}");
        }
        None => text.push_str(" on an unknown commit"),
    }
    match origin.actor() {
        Some(actor) => {
            let _ = write!(text, " by {actor}");
        }
        None => text.push_str(" by an unknown actor"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latch(kind: &str, path: &str) -> LatchEntry {
        serde_json::from_value(serde_json::json!({"kind": kind, "key": {"path": path}}))
            .expect("a path latch decodes")
    }

    fn task(handle: &str) -> TaskHandle {
        handle.parse().expect("a task handle parses")
    }

    fn timing(arm: &str, millis: &[u64]) -> ArmTiming {
        ArmTiming {
            arm: arm.to_owned(),
            millis: millis.to_vec(),
        }
    }

    #[test]
    fn only_a_pending_cover_holds_and_never_for_its_own_claimant() {
        let (a, b) = (latch("file_loc", "src/a.rs"), latch("file_loc", "src/b.rs"));
        let (pending, settled) = (task("GATE-dXcL8gr1q"), task("GATE-dXcL6kgQt"));
        let covers = [
            Cover {
                task: pending.clone(),
                latches: vec![a.clone()],
                arms: vec!["cargo:test:app::lib:app".to_owned()],
            },
            Cover {
                task: settled.clone(),
                latches: vec![b],
                arms: vec!["rust-typecheck".to_owned()],
            },
        ];
        let is_pending = |task: &TaskHandle| task == &pending;
        let others = Held {
            latches: BTreeMap::from([(a, pending.clone())]),
            arms: BTreeMap::from([("cargo:test:app::lib:app".to_owned(), pending.clone())]),
        };

        assert_eq!(
            held(&covers, is_pending, None),
            others,
            "a settled or deleted task holds nothing"
        );
        assert_eq!(
            held(&covers, is_pending, Some(&pending)),
            Held::default(),
            "the task's own claimant still faces the bound"
        );
        assert_eq!(
            held(&covers, is_pending, Some(&settled)),
            others,
            "another claim releases nothing"
        );
    }

    #[test]
    fn a_draft_files_one_child_per_file_and_per_unheld_slow_arm() {
        let ledger: StateLedger = serde_json::from_value(serde_json::json!({
            "version": gate::state::STATE_LEDGER_VERSION,
            "latches": [
                {"kind": "file_loc", "key": {"path": "src/a.rs"}},
                {"kind": "file_loc", "key": {"path": "src/b.rs"}},
                {"kind": "file_bytes", "key": {"path": "src/a.rs"}},
            ],
            "origins": [{
                "latch": {"kind": "file_loc", "key": {"path": "src/a.rs"}},
                "commit": "c0ffee",
                "at": 1_000.0,
                "actor": "actor-a",
                "measured": 812,
                "base_limit": 600,
            }],
            "claims": [],
        }))
        .expect("a ledger with one recorded origin decodes");
        let arms = [
            timing("cargo:test:app::lib:app", &[123_400, 118_000, 130_000]),
            timing("app-web-typecheck", &[67_000]),
            timing("cargo:test:wire::lib:wire", &[300, 400]),
            timing("windows-typecheck", &[74_000, 70_000]),
        ];
        let b = latch("file_loc", "src/b.rs");
        let held = Held {
            latches: BTreeMap::from([(b.clone(), task("GATE-dXcL8gr1q"))]),
            arms: BTreeMap::from([("windows-typecheck".to_owned(), task("GATE-dXcL8gr1q"))]),
        };

        let draft = draft(&ledger, &arms, &held, None);
        assert_eq!(
            draft.covers,
            [
                Subjects {
                    latches: vec![
                        latch("file_loc", "src/a.rs"),
                        latch("file_bytes", "src/a.rs")
                    ],
                    arms: Vec::new(),
                },
                Subjects {
                    latches: Vec::new(),
                    arms: vec!["cargo:test:app::lib:app".to_owned()],
                },
                Subjects {
                    latches: Vec::new(),
                    arms: vec!["app-web-typecheck".to_owned()],
                },
            ],
            "one child per unheld file, then one per unheld arm over the bound, slowest first"
        );
        assert_eq!(draft.held, 2, "the held file and the held arm file nothing");
        assert_eq!(draft.arm_child("cargo:test:app::lib:app"), Some(1));
        assert_eq!(draft.arm_child("windows-typecheck"), None);

        let digest = draft.content.digest();
        assert_eq!(
            super::draft(&ledger, &arms, &held, None).content.digest(),
            digest,
            "an unchanged catalogue previews the same digest"
        );
        assert_ne!(
            super::draft(&ledger, &arms, &Held::default(), None)
                .content
                .digest(),
            digest,
            "a released cover moves the preview, so its old digest is refused"
        );
        let until = "2026-12-01T00:00:00.000Z"
            .parse()
            .expect("an instant parses");
        assert_ne!(
            super::draft(&ledger, &arms, &held, Some(until))
                .content
                .digest(),
            digest,
            "the deferral is part of what an apply files"
        );
    }
}
