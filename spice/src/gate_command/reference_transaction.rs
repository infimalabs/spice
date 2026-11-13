//! Native guard for Git reference transactions on the current branch.
//!
//! # The refusal can arrive after the worktree is written
//!
//! This guard cannot refuse a destructive rewind before that rewind touches
//! the tree, and no care taken in this module changes it. `reference-transaction`
//! is a *reference* hook: githooks(5) describes it as invoked by commands that
//! perform reference updates, defines its `prepared` state as the point where
//! "all reference updates have been queued to the transaction and references
//! were locked on disk", and scopes an abort to references alone. Git publishes
//! no `pre-reset` hook, so there is no earlier surface for this guard to
//! occupy. A `reset --hard` writes the index and worktree first and only then
//! opens the reference transaction this hook is able to veto.
//!
//! Verified against git 2.50.1 rather than recalled: with this guard installed,
//! a `reset --hard` onto an upstream-merged parent is refused, and leaves HEAD
//! unmoved while the index and worktree hold the abandoned rewind's target
//! content. The branch survives; the tree does not. That asymmetry is why the
//! `ProtectedHistory` refusal reports that the queued target and the current
//! index and worktree agree, but it deliberately prescribes no destructive
//! recovery. That observable state is consistent with `reset --hard` having
//! written the tree, but a soft or mixed rewind can present the same state when
//! the index and worktree already matched its target. The hook is told neither
//! command nor intent, so the operator has to inspect the retained state before
//! deciding what, if anything, to restore.
//!
//! The operational errors cannot finish that state inspection. They therefore
//! report only that the index and worktree may have changed, tell the operator
//! to inspect them after correcting the operational cause, and name
//! `git reset --hard HEAD` only for a command the operator establishes was a
//! destructive rewind. That recovery leaves the reference where it already is,
//! so `abandoned_upstream_commits` finds nothing to protect.
//!
//! That damage is not what every refused command leaves behind, so the refusal
//! reads the state rather than assuming the worst of it. A refused
//! `commit --amend` never wrote the working tree at all, and telling its
//! operator that their files were overwritten would send them to a destructive
//! recovery they do not need. `WorktreeDamage` names which of the two happened
//! and the refusal carries the matching account; the discriminator and its
//! bounds are recorded on `worktree_damage`.
//!
//! Refusing any earlier would mean intercepting the rewind above Git, in a
//! spice-side wrapper for destructive commands. That is a larger design
//! question and is deliberately outside this module. It is recorded here so
//! the next reader of this guard can tell why the refusal arrives when it does
//! without re-deriving Git's hook ordering to reach the same answer.
//!
//! # This guard runs inside a lock every worktree shares
//!
//! `prepared` means the references "were locked on disk", so whatever this hook
//! does, it does while Git holds those locks. One of them is not per-worktree:
//! deleting a reference takes `packed-refs.lock` in the *common* directory, which
//! every linked worktree shares, and an ordinary commit ends with a deletion
//! transaction that clears `AUTO_MERGE`, `CHERRY_PICK_HEAD` and `REVERT_HEAD`.
//! Those pseudo-references are normally absent, and deleting an absent reference
//! takes the shared lock anyway.
//!
//! Recorded from a real commit rather than reasoned about, by a hook that logged
//! its state alongside whether the common lock existed: the queue naming
//! `refs/heads/main` was handed over with the lock **free**, and the only queue
//! ever handed over with the lock **held** named `AUTO_MERGE` alone. So the
//! transaction this guard exists to inspect never holds the shared lock, and the
//! transaction that holds it can never contain anything this guard may refuse.
//!
//! That is why [`apply`] reads its own input before it reads Git. Resolving
//! `HEAD`, the publication baseline and the tip the branch holds costs several
//! subprocesses, and for a queue naming no branch every one of them was spent
//! inside the shared lock to reach a conclusion the queue already determined. A
//! queue that names no `refs/heads/` reference is answered from the queue alone,
//! launching nothing.
//!
//! # A successful commit can still report skipped cleanup
//!
//! Git's commit status and its cleanup diagnostic account for different work.
//! Measured on Git 2.50.1, the direct `git commit` can record the requested
//! branch update and exit zero, then exhaust `core.packedRefsTimeout` while
//! deleting an absent pseudo-reference and write an `Unable to create
//! .git/packed-refs.lock` diagnostic to stderr. If the other writer releases
//! the lock within the bound, the same successful commit says nothing about
//! it. A finite bound decides how long Git retries; it cannot guarantee silence.
//!
//! Callers therefore have to read status and both output streams together:
//! zero means the primary commit succeeded, while the diagnostic separately
//! says best-effort cleanup was skipped. A genuine nonzero commit failure keeps
//! its own output. This repository-local hook cannot intercept, suppress, or
//! reinterpret its parent Git process's streams, and it never removes a
//! `packed-refs.lock` owned by another writer.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use spice::ambient::Ambient;

const USAGE: &str =
    "usage: spice gate reference-transaction <preparing|prepared|committed|aborted>";
const PREPARED: &str = "prepared";
const PROTECTED_COMMIT_LIMIT: usize = 5;
const ASCII_ZERO: u8 = 0x30;
const LOCAL_PREFIX: &str = "refs/heads/";
const OPERATIONAL_RECOVERY: &str = concat!(
    ". This error aborts the reference transaction, so HEAD is not moved, but ",
    "this hook runs after the command's tree operations: the index and worktree ",
    "may already have changed. After correcting this operational failure, ",
    "inspect them and, if this was a destructive rewind, restore the kept ",
    "commit with `git reset --hard HEAD`."
);
const EVIDENCE_RECOVERY: &str = concat!(
    ". This error aborts the reference transaction, so HEAD is not moved, but ",
    "the command may already have materialized index or worktree bytes. Inspect ",
    "`git status`, `git diff`, and `git diff --cached`; do not use a destructive ",
    "reset based only on this diagnostic."
);

/// Refuse a queued update that would abandon published history on this branch.
///
/// The order of the checks is a contract, not a detail. Cheapest first, and no
/// subprocess until the queue proves one is needed, because Git calls this while
/// holding locks that reach outside this worktree. See the module header.
///
/// # Errors
///
/// Returns [`ReferenceTransactionError::ProtectedHistory`] for a rewind that
/// gives up published commits, [`ReferenceTransactionError::UnjudgeableUpstream`]
/// for a rewind whose upstream resolves to nothing, or the operational variants
/// for a malformed queue or a failure to read Git.
pub fn apply(
    repository: &Path,
    state: &OsStr,
    input: &str,
    ambient: &Ambient,
) -> Result<(), ReferenceTransactionError> {
    let state = TransactionState::parse(state).unwrap_or(TransactionState::Preparing);
    if state == TransactionState::Preparing {
        return Ok(());
    }
    let updates = parse_updates(input)?;
    if !requires_repository(&updates) {
        return Ok(());
    }

    if state != TransactionState::Prepared {
        return finish_consumption(repository, state, &updates);
    }

    let Some(current_ref) = git_read(repository, &["symbolic-ref", "--quiet", "HEAD"])? else {
        return Ok(());
    };
    match publication(repository, &current_ref, ambient)? {
        Publication::Untracked => Ok(()),
        Publication::Unjudgeable { reference } => {
            refuse_unjudgeable_rewind(repository, &current_ref, &reference, &updates)
        }
        Publication::Published(baseline) => {
            refuse_abandoning_rewind(repository, &current_ref, &baseline, &updates)
        }
    }?;
    prepare_consumption(repository, &updates)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransactionState {
    Preparing,
    Prepared,
    Committed,
    Aborted,
}

impl TransactionState {
    fn parse(value: &OsStr) -> Option<Self> {
        match value.to_str() {
            Some("preparing") => Some(Self::Preparing),
            Some("prepared") => Some(Self::Prepared),
            Some("committed") => Some(Self::Committed),
            Some("aborted") => Some(Self::Aborted),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Prepared => "prepared",
            Self::Committed => "committed",
            Self::Aborted => "aborted",
        }
    }
}

/// Enter the hook-only command before ordinary CLI and mount initialization.
///
/// Returning `None` means the arguments do not select this command. Once its
/// exact prefix is present, every malformed state or arity is owned here and
/// fails with the public usage exit rather than falling through to another
/// parser or execution path.
pub(super) fn entry(
    mut arguments: impl Iterator<Item = OsString>,
    ambient: &Ambient,
) -> Option<i32> {
    arguments.next()?;
    if arguments.next().as_deref() != Some(OsStr::new("gate")) {
        return None;
    }
    if arguments.next().as_deref() != Some(OsStr::new("reference-transaction")) {
        return None;
    }
    let state = arguments.next();
    let extra = arguments.next();
    let Some(state) = state
        .as_deref()
        .filter(|_| extra.is_none())
        .and_then(TransactionState::parse)
    else {
        eprintln!("{USAGE}");
        return Some(2);
    };
    let result = run(state, ambient);
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    })
}

fn run(state: TransactionState, ambient: &Ambient) -> Result<(), Box<dyn Error>> {
    if state == TransactionState::Preparing {
        return Ok(());
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let repository = std::env::current_dir()?;
    apply(&repository, OsStr::new(state.as_str()), &input, ambient).map_err(Into::into)
}

/// The commit the branch holds before this transaction is written.
///
/// Read from the reference rather than taken from the queue: see [`RefUpdate`].
/// Asking the repository is sound because `prepared` is before the write -- the
/// reference is locked but still holds its pre-transaction value. No tip at all
/// means the branch is unborn and this queue creates it, which abandons nothing.
///
/// Both refusals need it and exactly one of them ever runs, so it is read inside
/// them rather than before the pairing resolves: a branch that tracks nothing
/// still never pays for it.
fn branch_tip(
    repository: &Path,
    current_ref: &str,
) -> Result<Option<String>, ReferenceTransactionError> {
    commit_oid(repository, current_ref)
}

/// Refuse a rewind whose declared upstream resolves to no commit.
///
/// The append is the reason this reads the queue at all rather than refusing the
/// branch outright. An append gives up no history whatever the upstream turns out
/// to be, so a lane whose remote is merely unfetched keeps working; the rewind is
/// the call that needed the upstream and did not get it.
fn refuse_unjudgeable_rewind(
    repository: &Path,
    current_ref: &str,
    upstream: &str,
    updates: &[RefUpdate<'_>],
) -> Result<(), ReferenceTransactionError> {
    let Some(tip) = branch_tip(repository, current_ref)? else {
        return Ok(());
    };
    for update in updates {
        if update.reference != current_ref {
            continue;
        }
        if rewind(repository, update, &tip)?.is_none() {
            continue;
        }
        return Err(ReferenceTransactionError::UnjudgeableUpstream {
            reference: current_ref.to_owned(),
            upstream: upstream.to_owned(),
            damage: worktree_damage(repository, update, &tip)?,
        });
    }
    Ok(())
}

/// Refuse a rewind that gives up commits the upstream already contains.
fn refuse_abandoning_rewind(
    repository: &Path,
    current_ref: &str,
    baseline: &Baseline,
    updates: &[RefUpdate<'_>],
) -> Result<(), ReferenceTransactionError> {
    let Some(tip) = branch_tip(repository, current_ref)? else {
        return Ok(());
    };
    for update in updates {
        if update.reference != current_ref {
            continue;
        }
        let Some(rewind) = rewind(repository, update, &tip)? else {
            continue;
        };
        let protected = abandoned_upstream_commits(repository, &rewind, &tip, &baseline.commit)?;
        if protected.is_empty() {
            continue;
        }
        let listed = protected
            .iter()
            .map(|commit| short_oid(repository, commit))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        return Err(ReferenceTransactionError::ProtectedHistory {
            reference: current_ref.to_owned(),
            commits: listed,
            baseline: baseline.reference.clone(),
            baseline_commit: short_oid(repository, &baseline.commit)?,
            damage: worktree_damage(repository, update, &tip)?,
        });
    }
    Ok(())
}

/// The reference this guard treats as published, and the commit it stands at.
///
/// Both travel together because the refusal has to name them together. A
/// baseline reduced to its commit is a number the reader cannot check, and a
/// reference without its commit cannot be compared against what the branch
/// actually holds.
struct Baseline {
    reference: String,
    commit: String,
}

/// What the branch's own pairing establishes about history this lane published.
///
/// Three states, because two of them were one state for as long as the answer
/// was an `Option` and the guard passed both in silence. They are different
/// claims. A branch that tracks nothing has no published history to give up, and
/// passing its rewind is correct. A branch whose pairing names a reference that
/// resolves to no commit has an upstream nobody fetched, so whether a rewind
/// gives up published work was never established -- and permitting it reports
/// exactly what a guard reports after checking and finding the rewind clear.
/// Collapsing the two spends the guard's silence on both.
enum Publication {
    /// The pairing names no reference, so no published history exists to protect.
    Untracked,
    /// The pairing names a reference that resolves to no commit.
    Unjudgeable { reference: String },
    /// The pairing names a reference standing at a commit.
    Published(Baseline),
}

/// Resolve what the branch's pairing establishes about published history.
///
/// The branch's own pairing names the reference, read by
/// [`spice::git_shadow::declared_pairing`], the same read task completion
/// integrates through: the history this guard refuses to abandon is the
/// history `task done` publishes to, a `.` pairing's local branch included. A
/// branch that declares no pairing leaves the guard inert; no second
/// resolution stands behind the first, because a backstop reached only here
/// would be a policy this side invented.
///
/// A reference the pairing names but that resolves to no commit is a third
/// answer rather than a second way of saying the first, which is why this
/// returns [`Publication`] and not an `Option`.
///
/// Naming and resolution are answered together here. On an unborn branch,
/// [`branch_tip`] leaves both refusals inert before either can fire. An unborn
/// branch that names a pairing still resolves the upstream before discovering
/// there is no tip to judge; that costs one subprocess in a state no lane
/// commits from.
fn publication(
    repository: &Path,
    current_ref: &str,
    ambient: &Ambient,
) -> Result<Publication, ReferenceTransactionError> {
    let branch = current_ref
        .strip_prefix(LOCAL_PREFIX)
        .unwrap_or(current_ref);
    let environment = ambient
        .environment()
        .map_err(|source| ReferenceTransactionError::Environment { source })?;
    let Some(pairing) = spice::git_shadow::declared_pairing(repository, branch, &environment)
        .map_err(|source| ReferenceTransactionError::Pairing { source })?
    else {
        return Ok(Publication::Untracked);
    };
    let reference = pairing.reference();
    let Some(commit) = commit_oid(repository, &reference)? else {
        return Ok(Publication::Unjudgeable { reference });
    };
    Ok(Publication::Published(Baseline { reference, commit }))
}

/// One line of the hook's queue.
///
/// Git names three fields and [`parse_updates`] still requires all three, but
/// The first field remains untrusted for history judgement: the guard still
/// reads the held reference itself. It is retained only to correlate Git's
/// prepared callback with the matching committed or aborted callback without
/// reading refs after they have moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RefUpdate<'a> {
    old: &'a str,
    new: &'a str,
    reference: &'a str,
}

fn parse_updates(input: &str) -> Result<Vec<RefUpdate<'_>>, ReferenceTransactionError> {
    let mut updates = Vec::new();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(ReferenceTransactionError::MalformedInput { line: index + 1 });
        }
        updates.push(RefUpdate {
            old: fields[0],
            new: fields[1],
            reference: fields[2],
        });
    }
    Ok(updates)
}

fn transaction_id(
    state: TransactionState,
    updates: &[RefUpdate<'_>],
) -> Result<gate::AdmissionTransactionId, ReferenceTransactionError> {
    let mut rows = updates
        .iter()
        .map(|update| (update.reference, update.old, update.new))
        .collect::<Vec<_>>();
    rows.sort_unstable();
    let bytes = wire::encode(&rows).map_err(|error| ReferenceTransactionError::Evidence {
        state: state.as_str(),
        reason: "gate-evidence::admission-consumption-invalid".to_owned(),
        detail: error.to_string(),
    })?;
    Ok(gate::AdmissionTransactionId::derive(&bytes))
}

fn prepare_consumption(
    repository: &Path,
    updates: &[RefUpdate<'_>],
) -> Result<(), ReferenceTransactionError> {
    let transaction = transaction_id(TransactionState::Prepared, updates)?;
    let mut candidates = Vec::new();
    for update in updates
        .iter()
        .filter(|update| update.reference.starts_with(LOCAL_PREFIX) && !is_zero_oid(update.new))
    {
        let Some(commit) = commit_oid(repository, update.new)? else {
            continue;
        };
        let branch_tip = commit_oid(repository, update.reference)?;
        if branch_tip.as_deref() == Some(commit.as_str())
            || reachable_from_ref(repository, &commit)?
        {
            continue;
        }
        let Some(tree) = git_read(repository, &["rev-parse", &format!("{commit}^{{tree}}")])?
        else {
            continue;
        };
        let parent = commit_first_parent(repository, &commit)?;
        candidates.push(
            gate::AdmissionCandidate::new(update.reference, parent, commit, tree)
                .map_err(|error| evidence_error(TransactionState::Prepared, error))?,
        );
    }
    if candidates.is_empty() {
        return Ok(());
    }
    let store = evidence_store(repository, TransactionState::Prepared)?;
    let now = wire::Instant::new(jiff::Timestamp::now()).map_err(|error| {
        ReferenceTransactionError::Evidence {
            state: TransactionState::Prepared.as_str(),
            reason: "gate-evidence::admission-consumption-clock-invalid".to_owned(),
            detail: error.to_string(),
        }
    })?;
    store
        .prepare_admission_consumption(&transaction, &candidates, now)
        .map(|_| ())
        .map_err(|error| evidence_error(TransactionState::Prepared, error))
}

/// Moving a ref within existing history does not author a prospective commit.
/// Admissions bind the prospective commit's first parent at authoring time.
/// Compare commit reachability, never tree equality: a new commit with identical
/// content and different ancestry still needs its own admission.
/// History protection runs before this check and continues to reject rewinds.
fn reachable_from_ref(repository: &Path, commit: &str) -> Result<bool, ReferenceTransactionError> {
    let output = git(
        repository,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "--count=1",
            "--contains",
            commit,
            "refs/heads/",
            "refs/remotes/",
            "refs/tags/",
        ],
    )?;
    if !output.status.success() {
        return Err(ReferenceTransactionError::Evidence {
            state: PREPARED,
            reason: "gate-evidence::history-unreadable".to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(!output.stdout.is_empty())
}

fn commit_first_parent(
    repository: &Path,
    commit: &str,
) -> Result<Option<String>, ReferenceTransactionError> {
    Ok(
        git_read(repository, &["show", "-s", "--format=%P", commit])?
            .and_then(|parents| parents.split_whitespace().next().map(str::to_owned)),
    )
}

fn finish_consumption(
    repository: &Path,
    state: TransactionState,
    updates: &[RefUpdate<'_>],
) -> Result<(), ReferenceTransactionError> {
    let transaction = transaction_id(state, updates)?;
    let store = evidence_store(repository, state)?;
    match state {
        TransactionState::Committed => {
            let now = wire::Instant::new(jiff::Timestamp::now()).map_err(|error| {
                ReferenceTransactionError::Evidence {
                    state: state.as_str(),
                    reason: "gate-evidence::admission-consumption-clock-invalid".to_owned(),
                    detail: error.to_string(),
                }
            })?;
            store
                .commit_admission_consumption(&transaction, now)
                .map(|_| ())
        }
        TransactionState::Aborted => store.abort_admission_consumption(&transaction).map(|_| ()),
        TransactionState::Preparing | TransactionState::Prepared => unreachable!(),
    }
    .map_err(|error| evidence_error(state, error))
}

fn evidence_store(
    repository: &Path,
    state: TransactionState,
) -> Result<gate::EvidenceStore, ReferenceTransactionError> {
    let repository = spice::repository::Repository::discover(repository).map_err(|error| {
        ReferenceTransactionError::Evidence {
            state: state.as_str(),
            reason: spice::repository::ResolveError::REASON.to_owned(),
            detail: error.to_string(),
        }
    })?;
    gate::EvidenceStore::open(repository.common_dir()).map_err(|error| evidence_error(state, error))
}

fn evidence_error(
    state: TransactionState,
    error: gate::EvidenceStoreError,
) -> ReferenceTransactionError {
    ReferenceTransactionError::Evidence {
        state: state.as_str(),
        reason: error.reason().to_owned(),
        detail: error.detail().to_owned(),
    }
}

fn requires_repository(updates: &[RefUpdate<'_>]) -> bool {
    updates
        .iter()
        .any(|update| update.reference.starts_with(LOCAL_PREFIX))
}

/// The value a rewind puts in place of the branch's tip.
///
/// `None` is a deletion. It reaches nothing, so it stays subject to judgement
/// rather than escaping it, which is why this is not simply the new commit.
struct Rewind {
    new: Option<String>,
}

/// Read an update as a rewind, or `None` when it gives up no history.
///
/// One definition serves both refusals. Written twice, an update could be an
/// append for the upstream that resolved and a rewind for the one that did not,
/// and the guard's two halves would disagree about the same queue line.
///
/// A `new` naming no commit this repository holds is not this guard's to rule
/// on. Anything still reaching `tip` is an append. The tip is the reference's
/// own value and never the caller's declared old one: see [`RefUpdate`].
fn rewind(
    repository: &Path,
    update: &RefUpdate<'_>,
    tip: &str,
) -> Result<Option<Rewind>, ReferenceTransactionError> {
    let new = if is_zero_oid(update.new) {
        None
    } else {
        let Some(new) = commit_oid(repository, update.new)? else {
            return Ok(None);
        };
        Some(new)
    };
    if let Some(new) = new.as_deref()
        && is_ancestor(repository, tip, new)?
    {
        return Ok(None);
    }
    Ok(Some(Rewind { new }))
}

fn abandoned_upstream_commits(
    repository: &Path,
    rewind: &Rewind,
    tip: &str,
    upstream: &str,
) -> Result<Vec<String>, ReferenceTransactionError> {
    let Some(merge_bases) = git_read(repository, &["merge-base", "--all", tip, upstream])? else {
        return Ok(Vec::new());
    };
    let mut arguments = vec![
        "rev-list".to_owned(),
        format!("--max-count={PROTECTED_COMMIT_LIMIT}"),
    ];
    arguments.extend(merge_bases.lines().map(str::to_owned));
    if let Some(new) = &rewind.new {
        arguments.push("--not".to_owned());
        arguments.push(new.clone());
    }
    Ok(git_read_owned(repository, &arguments)?
        .map(|output| output.lines().map(str::to_owned).collect())
        .unwrap_or_default())
}

fn is_zero_oid(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte == ASCII_ZERO)
}

fn commit_oid(repository: &Path, value: &str) -> Result<Option<String>, ReferenceTransactionError> {
    if value.starts_with("ref:") {
        return Ok(None);
    }
    git_read_owned(
        repository,
        &[
            "rev-parse".to_owned(),
            "--verify".to_owned(),
            "--quiet".to_owned(),
            format!("{value}^{{commit}}"),
        ],
    )
}

fn short_oid(repository: &Path, value: &str) -> Result<String, ReferenceTransactionError> {
    Ok(git_read(repository, &["rev-parse", "--short", value])?.unwrap_or_else(|| value.to_owned()))
}

fn is_ancestor(
    repository: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, ReferenceTransactionError> {
    Ok(git(
        repository,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?
    .status
    .success())
}

/// What the refused command had already done to the working tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeDamage {
    /// The queued target matches the index and worktree, consistently with a
    /// rewrite but not proving one occurred.
    Rewritten,
    /// The command left the files on disk as they were.
    Untouched,
}

/// Read which damage the refused command left behind.
///
/// The hook is handed a state name, the queued updates and nothing else: git
/// exports no variable naming the command that opened the transaction, which was
/// confirmed by dumping the hook environment for `reset` and for `commit --amend`
/// rather than taken from documentation. So this reads the state the command left
/// instead of trying to recognise the command.
///
/// Two facts together are decisive. The update moves the branch backwards onto an
/// ancestor of its own tip, and the working tree already holds that ancestor's
/// content even though HEAD has not moved yet. A command that wrote the working
/// tree itself leaves that pair behind. It is not the only way to arrive there:
/// a rewind that wrote nothing leaves the same pair whenever the tree already
/// held the target's content, which is the bound named below.
///
/// Both halves are load-bearing, measured live against git 2.50.1 at the
/// `prepared` state. Dropping the ancestor test would report a refused
/// `commit --amend` as rewritten, because an amend leaves index and worktree
/// holding its own new tree; the amend is excluded instead because its
/// replacement tip is no ancestor of the tip it replaces. Dropping the content
/// test would report `reset --soft` and `reset --mixed` as rewritten, because
/// both move backwards; they are excluded instead because neither writes the
/// files on disk, so the worktree ordinarily still disagrees with the rewind
/// target. Ordinarily rather than always: a worktree that already agreed is not
/// made to disagree by a rewind that wrote nothing, so the content test admits
/// those two after all, which is the bound below.
///
/// The bound worth naming: a `--soft` or `--mixed` rewind onto a commit whose
/// content the index and worktree already happen to match is reported as
/// rewritten. The refusal text therefore treats this arm as an ambiguous
/// observable state and never prescribes a destructive reset from it.
fn worktree_damage(
    repository: &Path,
    update: &RefUpdate<'_>,
    tip: &str,
) -> Result<WorktreeDamage, ReferenceTransactionError> {
    if is_zero_oid(update.new) {
        return Ok(WorktreeDamage::Untouched);
    }
    if is_ancestor(repository, update.new, tip)? && worktree_holds(repository, update.new)? {
        return Ok(WorktreeDamage::Rewritten);
    }
    Ok(WorktreeDamage::Untouched)
}

/// Whether the working tree and index already hold this commit's content.
fn worktree_holds(repository: &Path, commit: &str) -> Result<bool, ReferenceTransactionError> {
    Ok(git(repository, &["diff-index", "--quiet", commit, "--"])?
        .status
        .success())
}

fn git_read(
    repository: &Path,
    arguments: &[&str],
) -> Result<Option<String>, ReferenceTransactionError> {
    git_read_owned(
        repository,
        &arguments
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>(),
    )
}

fn git_read_owned(
    repository: &Path,
    arguments: &[String],
) -> Result<Option<String>, ReferenceTransactionError> {
    let output = git(repository, arguments)?;
    if !output.status.success() {
        return Ok(None);
    }
    let output =
        String::from_utf8(output.stdout).map_err(|_| ReferenceTransactionError::NonUtf8Output {
            arguments: arguments.to_vec(),
        })?;
    let output = output.trim().to_owned();
    Ok((!output.is_empty()).then_some(output))
}

fn git<S>(repository: &Path, arguments: &[S]) -> Result<Output, ReferenceTransactionError>
where
    S: AsRef<OsStr>,
{
    let arguments = arguments
        .iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    Command::new("git")
        .args(&arguments)
        .current_dir(repository)
        .output()
        .map_err(|source| ReferenceTransactionError::GitLaunch {
            repository: repository.to_path_buf(),
            arguments: arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect(),
            source,
        })
}

/// A native `reference-transaction` refusal.
#[derive(Debug)]
pub enum ReferenceTransactionError {
    MalformedInput {
        line: usize,
    },
    GitLaunch {
        repository: PathBuf,
        arguments: Vec<String>,
        source: io::Error,
    },
    NonUtf8Output {
        arguments: Vec<String>,
    },
    Evidence {
        state: &'static str,
        reason: String,
        detail: String,
    },
    Environment {
        source: sill::SpecError,
    },
    Pairing {
        source: spice::git_shadow::Error,
    },
    ProtectedHistory {
        reference: String,
        commits: String,
        baseline: String,
        baseline_commit: String,
        damage: WorktreeDamage,
    },
    UnjudgeableUpstream {
        reference: String,
        upstream: String,
        damage: WorktreeDamage,
    },
}

impl fmt::Display for ReferenceTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedInput { line } => {
                write!(formatter, "malformed reference-transaction line {line}")?;
                formatter.write_str(OPERATIONAL_RECOVERY)
            }
            Self::GitLaunch {
                repository,
                arguments,
                source,
            } => {
                write!(
                    formatter,
                    "reference-transaction could not run git -C {} {}: {source}",
                    repository.display(),
                    arguments.join(" ")
                )?;
                formatter.write_str(OPERATIONAL_RECOVERY)
            }
            Self::NonUtf8Output { arguments } => {
                write!(
                    formatter,
                    "reference-transaction git output was not UTF-8: git {}",
                    arguments.join(" ")
                )?;
                formatter.write_str(OPERATIONAL_RECOVERY)
            }
            Self::Environment { source } => {
                write!(
                    formatter,
                    "reference-transaction could not capture its environment: {source}"
                )?;
                formatter.write_str(OPERATIONAL_RECOVERY)
            }
            Self::Pairing { source } => {
                write!(
                    formatter,
                    "reference-transaction could not read the branch's declared pairing: {source}"
                )?;
                formatter.write_str(OPERATIONAL_RECOVERY)
            }
            Self::Evidence {
                state,
                reason,
                detail,
            } => {
                write!(
                    formatter,
                    "reference-transaction {state} evidence failed: {reason}; {detail}"
                )?;
                if *state == PREPARED {
                    formatter.write_str(EVIDENCE_RECOVERY)
                } else {
                    formatter.write_str(
                        ". No gate action or test was executed; retry this exact hook state after correcting the evidence store.",
                    )
                }
            }
            Self::ProtectedHistory {
                reference,
                commits,
                baseline,
                baseline_commit,
                damage,
            } => {
                write!(
                    formatter,
                    concat!(
                        "reference-transaction guard refused to abandon upstream-merged ",
                        "commits on current branch {}: {}. Published history was resolved ",
                        "to {} at {}, and those commits are contained in it; if that is not ",
                        "the reference this lane publishes to, the refusal is a ",
                        "misresolution and not a protection. This is expected when a task ",
                        "boundary has advanced origin/upstream; keep those commits and ",
                        "continue with an append-only commit instead of amending or ",
                        "resetting backwards. "
                    ),
                    reference, commits, baseline, baseline_commit
                )?;
                formatter.write_str(worktree_account(*damage))
            }
            Self::UnjudgeableUpstream {
                reference,
                upstream,
                damage,
            } => {
                write!(
                    formatter,
                    concat!(
                        "reference-transaction guard cannot establish the upstream of ",
                        "current branch {}: its pairing names {}, which does not resolve to ",
                        "a commit, so whether this rewind gives up published work is unknown ",
                        "rather than cleared. Fetch that remote, or correct the branch's ",
                        "remote and merge pairing, and run this again. "
                    ),
                    reference, upstream
                )?;
                formatter.write_str(worktree_account(*damage))
            }
        }
    }
}

/// The account of the working tree that a refused rewind owes the operator.
///
/// Both refusals abort a rewind at the same point in the transaction, so they
/// leave the tree in the same state and say so in the same words. Worded twice,
/// the two could come to disagree about what a refusal does to the files on disk,
/// and an operator reading one of them would recover from the wrong description.
const fn worktree_account(damage: WorktreeDamage) -> &'static str {
    match damage {
        WorktreeDamage::Rewritten => concat!(
            "This refusal protected the branch, not necessarily the tree: HEAD ",
            "was not moved, and the index and worktree match the refused rewind ",
            "target. A hard rewind may have written them before any hook could ",
            "run, but a soft or mixed rewind can look identical when that content ",
            "was already present. Inspect `git status`, `git diff`, and ",
            "`git diff --cached` before choosing any recovery; do not use a ",
            "destructive reset based only on this diagnostic."
        ),
        WorktreeDamage::Untouched => concat!(
            "HEAD was not moved and this command did not write the working ",
            "tree, so the files on disk are the ones you already had and there ",
            "is nothing here to restore. Do not reset to recover from this ",
            "refusal; it would discard work this refusal did not touch."
        ),
    }
}

impl Error for ReferenceTransactionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::GitLaunch { source, .. } => Some(source),
            Self::Environment { source } => Some(source),
            Self::Pairing { source } => Some(source),
            Self::MalformedInput { .. }
            | Self::NonUtf8Output { .. }
            | Self::Evidence { .. }
            | Self::ProtectedHistory { .. }
            | Self::UnjudgeableUpstream { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
