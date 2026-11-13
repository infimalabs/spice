use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cli::Contract;
use crate::journal_lock::JournalLock;
use sill::authority;
use spice::ambient::Ambient;
use spice::repository::{Repository, StateError, StatePaths};
use wire::manifest::{JOURNAL_BACKUP_VERSION, JournalBackupManifest, JournalBackupMember};
use wire::{
    Actor, Digest, Domain, FactRow, Fault, FaultClass, JournalBackup, JournalReindex,
    JournalReport, JournalVerification, Outcome, Producer, Refusal, Reply, ReplyPosition, Seq,
    SeqRange, ThreadReport, WireVersion,
};

use crate::read_page::Page;

pub(crate) mod tape;

const AUTHORITY_MEMBER: &str = "spicejournal.sqlite3";
const ARTIFACT_MEMBERS: &str = "artifacts/tasks";
const MANIFEST: &str = "manifest.json";
static NEXT_BACKUP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn backup(matches: &clap::ArgMatches) -> Outcome<JournalBackup> {
    let output = matches
        .get_one::<String>("OUTPUT")
        .expect("the generated backup parser requires OUTPUT");
    match backup_inner(Path::new(output)) {
        Ok((position, body)) => reply(ReplyPosition::Committed(position), body),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn report(
    matches: &clap::ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<JournalReport> {
    match Page::from_matches(matches, &["journal"])
        .map_err(Failure::Refusal)
        .and_then(|page| report_inner(page, contract, ambient))
    {
        Ok((position, body)) => reply(position, body),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn verify(contract: &Contract, ambient: &Ambient) -> Outcome<JournalVerification> {
    match verify_inner(contract, ambient) {
        Ok((position, body)) => reply(position, body),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn reindex() -> Outcome<JournalReindex> {
    match reindex_inner() {
        Ok((position, body)) => reply(position, body),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn thread(
    matches: &clap::ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<ThreadReport> {
    let reference = matches
        .get_one::<String>("REF")
        .expect("the generated thread parser requires REF");
    match Page::from_matches(matches, &["thread"])
        .map_err(Failure::Refusal)
        .and_then(|page| thread_inner(reference, page, contract, ambient))
    {
        Ok((position, body)) => reply(position, body),
        Err(failure) => failure.outcome(),
    }
}

fn backup_inner(output: &Path) -> Result<(SeqRange, JournalBackup), Failure> {
    let state = state()?;
    let _lock = acquire_lock(&state)?;
    let reader = open_reader(&state)?;
    reader.verify().map_err(verify_failure)?;
    let output_name = output.file_name().ok_or_else(|| {
        Failure::refusal(
            "backup::output-refused",
            "a new bundle path with a filename",
            output.display().to_string(),
            "choose a nonexistent output path in an existing directory",
        )
    })?;
    let requested_parent = output.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(requested_parent).map_err(|error| {
        Failure::environment(
            "backup::parent-unavailable",
            format!("{}: {error}", requested_parent.display()),
        )
    })?;
    let destination = parent.join(output_name);
    require_absent(&destination)?;

    let (temporary, mut cleanup) = create_staging_directory(&parent)?;
    let authority_path = temporary.join(AUTHORITY_MEMBER);
    let authority = reader
        .snapshot_file(&authority_path)
        .map_err(snapshot_file_failure)?;
    let authority_member = JournalBackupMember {
        path: AUTHORITY_MEMBER.to_owned(),
        mode: mode(&authority_path)?,
        bytes: authority.bytes(),
        digest: authority.digest().clone(),
    };
    let mut members = vec![authority_member];
    members.extend(stage_artifact_members(&state, &temporary)?);
    members.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = JournalBackupManifest {
        version: JOURNAL_BACKUP_VERSION,
        wire: WireVersion::CURRENT,
        journal_schema: journal::SCHEMA_VERSION,
        authority: AUTHORITY_MEMBER.to_owned(),
        members,
    };
    let manifest_bytes = wire::encode(&manifest)
        .map_err(|error| Failure::protocol("backup::manifest-unencodable", error.to_string()))?;
    write_private_file(&temporary.join(MANIFEST), &manifest_bytes)?;
    sync_tree_directories(&temporary)?;
    let verification = journal::verify_restore_bundle(&temporary).map_err(bundle_failure)?;
    publish_backup(&temporary, &destination)?;
    cleanup.disarm();
    sync_directory(&parent)?;
    let body = JournalBackup {
        output: output.display().to_string(),
        verification,
    };
    let position = match append_recovery_fact(
        &state,
        "backup",
        wire::body::JournalBacked {
            bundle: destination.display().to_string(),
            manifest: manifest
                .members
                .iter()
                .map(|member| member.path.clone())
                .collect(),
        },
    ) {
        Ok(position) => position,
        Err(failure) => {
            if let Err(error) = fs::remove_dir_all(&destination) {
                return Err(Failure::environment(
                    "backup::rollback-failed",
                    format!(
                        "{}: {error}; recovery fact was not committed",
                        destination.display()
                    ),
                ));
            }
            sync_directory(&parent)?;
            return Err(failure);
        }
    };
    Ok((position, body))
}

fn stage_artifact_members(
    state: &StatePaths,
    temporary: &Path,
) -> Result<Vec<JournalBackupMember>, Failure> {
    let store = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    let generation = store.capture().map_err(artifact_failure)?;
    let mut members = Vec::with_capacity(generation.members().len());
    for artifact in generation.members() {
        let relative = Path::new(ARTIFACT_MEMBERS).join(artifact.path());
        let destination = temporary.join(&relative);
        let directory = destination
            .parent()
            .expect("artifact backup member has a parent");
        create_private_directories(directory)?;
        let (bytes, digest) = write_backup_member(&destination, artifact)?;
        members.push(JournalBackupMember {
            path: relative_spelling(&relative)?,
            mode: mode(&destination)?,
            bytes,
            digest,
        });
    }
    Ok(members)
}

/// Newest facts first, from the caller's `--after SEQ` cursor down.
///
/// Facts occupy every position from the journal's first fact through its
/// cursor, so the population at or before a start position is its distance
/// from that first fact. Retained fact pages are read under the journal's own
/// decoded-byte ceiling and clipped to rows as they arrive, so a large or
/// oversized fact costs one bounded row.
fn report_inner(
    page: Page,
    contract: &Contract,
    ambient: &Ambient,
) -> Result<(ReplyPosition, JournalReport), Failure> {
    let state = state()?;
    let reader = open_reader(&state)?;
    let schema = reader.schema_version().map_err(open_failure)?;
    let text = page.limits().text;
    let retain = page.retain();
    let (cursor, (span, rows)) = read_authenticated(&state, &reader, contract, ambient, |read| {
        let span = Span::read(read, page.after(), |range| {
            Failure::Refusal(page.cursor_refusal(range))
        })?;
        let rows = newest_rows(read, span.start, span.horizon, retain, text)?;
        Ok((span, rows))
    })?;
    let population = usize::try_from(span.population(span.start))
        .map_err(|error| Failure::protocol("journal::count-overflow", error.to_string()))?;
    let entries = page.shown(rows, population).map_err(Failure::Refusal)?;
    Ok((
        ReplyPosition::Snapshot(cursor),
        JournalReport {
            schema,
            cursor: Some(cursor),
            facts: span.population(span.horizon),
            entries,
        },
    ))
}

/// The positions one journal read covers: its first fact, its horizon, and
/// the inclusive start an `--after SEQ` names between them.
struct Span {
    first: Seq,
    start: Seq,
    horizon: Seq,
}

impl Span {
    /// Two single-row reads; `refuse` receives the admitted range when `after`
    /// names no position inside it.
    fn read(
        read: &journal::JournalRead<'_>,
        after: Option<&str>,
        refuse: impl FnOnce(String) -> Failure,
    ) -> Result<Self, Failure> {
        let read_failed =
            |error: journal::GuardError| Failure::store("journal::read-failed", error.to_string());
        let horizon = read
            .cursor()
            .map_err(read_failed)?
            .ok_or_else(cursor_missing)?;
        let first = read
            .first_position()
            .map_err(read_failed)?
            .ok_or_else(cursor_missing)?;
        let start = match after {
            None => horizon,
            Some(after) => after
                .parse::<Seq>()
                .ok()
                .filter(|seq| (first..=horizon).contains(seq))
                .ok_or_else(|| refuse(format!("SEQ from {first} to {horizon}")))?,
        };
        Ok(Self {
            first,
            start,
            horizon,
        })
    }

    /// Facts at or before `through`, which positions fill contiguously from
    /// the first fact.
    fn population(&self, through: Seq) -> u64 {
        through.get() - self.first.get() + 1
    }
}

/// Up to `retain` rows newest first, starting inclusively at `start`.
///
/// The reader resumes strictly before an exclusive cursor that must lie
/// inside its snapshot, so a start at the horizon reads from the newest fact
/// and an older start resumes before its successor.
fn newest_rows(
    read: &journal::JournalRead<'_>,
    start: Seq,
    horizon: Seq,
    retain: usize,
    text: usize,
) -> Result<Vec<(FactRow, String)>, Failure> {
    let mut rows = Vec::new();
    let mut before = (start < horizon)
        .then(|| Seq::new(start.get() + 1))
        .flatten();
    while rows.len() < retain {
        let budget = journal::FactReadBudget::new(
            (retain - rows.len()).min(journal::MAX_RETAINED_FACT_PAGE_ROWS),
            journal::MAX_RETAINED_FACT_PAGE_BYTES,
        )
        .map_err(|error| Failure::protocol("journal::read-budget", error.to_string()))?;
        let fetched = read
            .recent_facts(before, budget)
            .map_err(|error| Failure::store("journal::read-failed", error.to_string()))?;
        rows.extend(fetched.rows.iter().map(|fact| {
            let row = FactRow::of(fact, text);
            let key = row.seq.to_string();
            (row, key)
        }));
        match fetched.continuation {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    Ok(rows)
}

fn verify_inner(
    contract: &Contract,
    ambient: &Ambient,
) -> Result<(ReplyPosition, JournalVerification), Failure> {
    let state = state()?;
    let reader = open_reader(&state)?;
    let schema = reader.schema_version().map_err(open_failure)?;
    let (cursor, verified) = read_authenticated(&state, &reader, contract, ambient, |read| {
        read.verify().map_err(verify_failure)
    })?;
    Ok((
        ReplyPosition::Snapshot(cursor),
        JournalVerification {
            schema,
            facts: count(verified.facts, "facts")?,
            receipts: count(verified.receipts, "receipts")?,
            heads: count(verified.heads, "heads")?,
            threads: count(verified.threads, "threads")?,
        },
    ))
}

fn reindex_inner() -> Result<(ReplyPosition, JournalReindex), Failure> {
    let state = state()?;
    let _lock = acquire_lock(&state)?;
    let mut journal = journal::Journal::open(state.journal()).map_err(open_failure)?;
    admit_steering_index(&mut journal)?;
    let rebuilt = journal.reindex().map_err(|error| match error {
        journal::ReindexError::Violations(_) => Failure::refusal(
            "journal::history-invalid",
            "canonical contiguous immutable facts",
            error.to_string(),
            "run `spice journal verify` and restore valid authority before reindexing",
        ),
        journal::ReindexError::Sqlite(_) => {
            Failure::store("journal::reindex-failed", error.to_string())
        }
    })?;
    let cursor = journal
        .fact_snapshot()
        .map_err(snapshot_failure)?
        .horizon()
        .ok_or_else(cursor_missing)?;
    Ok((
        ReplyPosition::Snapshot(cursor),
        JournalReindex {
            facts: count(rebuilt.facts, "facts")?,
            threads: count(rebuilt.threads, "threads")?,
        },
    ))
}

/// Oldest thread facts first, from the caller's inclusive `--after SEQ`.
///
/// The root resolves and the remaining population counts through the thread
/// index; retained rows are decoded in bounded pages, so a read costs its page
/// rather than the journal or the whole thread.
fn thread_inner(
    reference: &str,
    page: Page,
    contract: &Contract,
    ambient: &Ambient,
) -> Result<(ReplyPosition, ThreadReport), Failure> {
    let state = state()?;
    let reader = open_reader(&state)?;
    let text = page.limits().text;
    let retain = page.retain();
    let cursor_refusal = || Failure::Refusal(page.cursor_refusal("SEQ of a fact in this thread"));
    let at = page
        .after()
        .map(|after| after.parse::<Seq>().map_err(|_| cursor_refusal()))
        .transpose()?;
    let (cursor, (root, remaining, rows)) =
        read_authenticated(&state, &reader, contract, ambient, |read| {
            let root = read.thread_root(reference).map_err(thread_failure)?;
            let mut rows = Vec::new();
            let mut remaining = None;
            let mut next = at;
            loop {
                let budget = journal::FactReadBudget::new(
                    (retain - rows.len()).min(journal::MAX_RETAINED_FACT_PAGE_ROWS),
                    journal::MAX_RETAINED_FACT_PAGE_BYTES,
                )
                .map_err(|error| Failure::protocol("thread::read-budget", error.to_string()))?;
                let fetched = read
                    .thread_page(root, next, budget)
                    .map_err(thread_read_failure)?
                    .ok_or_else(cursor_refusal)?;
                let omitted = fetched.overflow.map_or(0, |overflow| overflow.omitted);
                remaining.get_or_insert(
                    u64::try_from(fetched.rows.len())
                        .unwrap_or(u64::MAX)
                        .saturating_add(omitted),
                );
                rows.extend(fetched.rows.iter().map(|fact| {
                    let row = FactRow::of(fact, text);
                    let key = row.seq.to_string();
                    (row, key)
                }));
                match fetched.overflow {
                    Some(overflow) if rows.len() < retain => next = Some(overflow.continuation),
                    _ => break,
                }
            }
            Ok((root, remaining.unwrap_or(0), rows))
        })?;
    let population = usize::try_from(remaining)
        .map_err(|error| Failure::protocol("thread::count-overflow", error.to_string()))?;
    let entries = page.shown(rows, population).map_err(Failure::Refusal)?;
    Ok((
        ReplyPosition::Snapshot(cursor),
        ThreadReport {
            reference: reference.to_owned(),
            root,
            entries,
        },
    ))
}

fn read_authenticated<T, F>(
    state: &StatePaths,
    reader: &journal::JournalReader,
    contract: &Contract,
    ambient: &Ambient,
    answer: F,
) -> Result<(Seq, T), Failure>
where
    F: for<'a> FnOnce(&journal::JournalRead<'a>) -> Result<T, Failure>,
{
    let epoch = crate::command_client::authenticate(state.serve(), contract, ambient)
        .map_err(authentication_failure)?;
    reader
        .read_with(|read| {
            let cursor = read
                .cursor()
                .map_err(|error| Failure::store("journal::read-failed", error.to_string()))?
                .ok_or_else(cursor_missing)?;
            let residency = read
                .residency_head()
                .map_err(|error| Failure::store("journal::read-failed", error.to_string()))?
                .ok_or_else(|| {
                    Failure::store(
                        "journal::residency-missing",
                        "the authenticated resident has no durable residency head",
                    )
                })?;
            if residency.body().epoch != epoch {
                return Err(Failure::refusal(
                    "journal::epoch-moved",
                    epoch.to_string(),
                    residency.body().epoch.to_string(),
                    "reconnect through current resident discovery and retry",
                ));
            }
            answer(read).map(|body| (cursor, body))
        })
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => {
                Failure::store("journal::read-failed", error.to_string())
            }
            journal::ReadError::Answer(failure) => failure,
        })
}

fn authentication_failure(outcome: Outcome<wire::intent::CommandAnswer>) -> Failure {
    match outcome {
        Outcome::Refusal(refusal) => Failure::Refusal(refusal),
        Outcome::Fault(fault) => Failure::Fault(fault),
        Outcome::Reply(_) => Failure::protocol(
            "journal::authentication-invalid",
            "resident authentication returned a command reply as an error",
        ),
    }
}

fn cursor_missing() -> Failure {
    Failure::store(
        "journal::cursor-missing",
        "a resident or recovery authority has no durable journal position",
    )
}

fn thread_failure(error: journal::ThreadError) -> Failure {
    match error {
        journal::ThreadError::NotFound { .. } | journal::ThreadError::Ambiguous { .. } => {
            Failure::refusal(
                "thread::reference-refused",
                "one existing sequence or unambiguous exact origin",
                error.to_string(),
                "select a fact sequence, or an origin that names exactly one root",
            )
        }
        journal::ThreadError::Read(error) => thread_read_failure(error),
    }
}

fn thread_read_failure(error: journal::GuardError) -> Failure {
    match error {
        journal::GuardError::Store(_) => Failure::store("thread::read-failed", error.to_string()),
        _ => Failure::refusal(
            "thread::authority-invalid",
            "a decodable thread index and fact envelopes",
            error.to_string(),
            "run `spice journal verify`, then restore or reindex as directed",
        ),
    }
}

fn append_recovery_fact<F: wire::Body>(
    state: &StatePaths,
    operation: &str,
    body: F,
) -> Result<SeqRange, Failure> {
    let origin = format!("recovery:{operation}")
        .parse::<wire::Origin>()
        .map_err(|error| Failure::protocol("journal::origin-invalid", error.to_string()))?;
    let at = wire::Instant::new(jiff::Timestamp::now())
        .map_err(|error| Failure::environment("journal::clock-invalid", error.to_string()))?;
    let mut write =
        journal::JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    write
        .fact(body, journal::FactCause::Root)
        .map_err(|error| Failure::protocol("journal::recovery-fact-invalid", error.to_string()))?;
    let mut journal = journal::Journal::open(state.journal()).map_err(open_failure)?;
    admit_steering_index(&mut journal)?;
    let committed = journal.apply(write).map_err(commit_failure)?;
    Ok(SeqRange {
        first: committed.first(),
        last: committed.last(),
    })
}

fn admit_steering_index(journal: &mut journal::Journal) -> Result<(), Failure> {
    crate::resident::admit_steering_index(journal).map_err(|outcome| match outcome {
        Outcome::Fault(fault) => Failure::store(fault.reason, fault.detail),
        Outcome::Refusal(refusal) => Failure::refusal(
            refusal.reason(),
            "one canonical steering replay",
            refusal.explanation(),
            "verify or restore the journal before retrying recovery",
        ),
        Outcome::Reply(_) => unreachable!("steering index admission does not reply"),
    })
}

fn state() -> Result<StatePaths, Failure> {
    let working = std::env::current_dir().map_err(|error| {
        Failure::environment("journal::working-directory-unavailable", error.to_string())
    })?;
    let repository = Repository::discover(&working).map_err(|error| {
        Failure::environment(spice::repository::ResolveError::REASON, error.to_string())
    })?;
    repository.admit_state().map_err(state_failure)
}

fn open_reader(state: &StatePaths) -> Result<journal::JournalReader, Failure> {
    journal::JournalReader::open(state.journal()).map_err(open_failure)
}

fn acquire_lock(state: &StatePaths) -> Result<JournalLock, Failure> {
    JournalLock::acquire(state).map_err(|error| match error {
        crate::journal_lock::Error::Contended => Failure::refusal(
            "journal::writer-active",
            "the resident writer lock to be unheld",
            "the resident or another maintenance command holds the lock",
            "quiesce the resident and retry the offline journal operation",
        ),
        crate::journal_lock::Error::Io(error) => {
            Failure::environment("journal::lock-unavailable", error.to_string())
        }
    })
}

fn state_failure(error: StateError) -> Failure {
    match error {
        StateError::Filesystem { .. } => {
            Failure::environment("repository::state-path-unavailable", error.to_string())
        }
        StateError::Retired { .. } => Failure::refusal(
            "repository::state-reset-required",
            "the canonical lifetime-grouped state layout",
            error.to_string(),
            "stop every writer and run the offline common-layout reset",
        ),
        StateError::Linked { .. }
        | StateError::NotDirectory { .. }
        | StateError::NotRegularFile { .. } => Failure::refusal(
            "repository::state-path-refused",
            spice::repository::state_shape_expectation(),
            error.to_string(),
            format!(
                "repair the malformed state component at {}",
                error.path().display()
            ),
        ),
    }
}

fn open_failure(error: journal::OpenError) -> Failure {
    match error {
        journal::OpenError::Migration(_) => Failure::refusal(
            "journal::schema-refused",
            format!("finalized journal schema {}", journal::SCHEMA_VERSION),
            error.to_string(),
            "finish or replace incompatible authority before retrying",
        ),
        _ => Failure::store("journal::open-failed", error.to_string()),
    }
}

fn snapshot_failure(error: journal::SnapshotError) -> Failure {
    match error {
        journal::SnapshotError::Invalid { .. } => Failure::refusal(
            "journal::history-invalid",
            "canonical contiguous immutable facts",
            error.to_string(),
            "run `spice journal verify` and restore valid authority",
        ),
        journal::SnapshotError::Sqlite(_) => {
            Failure::store("journal::read-failed", error.to_string())
        }
    }
}

fn snapshot_file_failure(error: journal::SnapshotFileError) -> Failure {
    Failure::environment("backup::snapshot-failed", error.to_string())
}

fn verify_failure(error: journal::VerifyError) -> Failure {
    match error {
        journal::VerifyError::Violations(violations) => Failure::refusal(
            "journal::verification-failed",
            "one valid schema, history, index, heads, receipts, and cursor closure",
            violations
                .iter()
                .map(|violation| {
                    format!(
                        "{:?}:{}:{}",
                        violation.class, violation.subject, violation.detail
                    )
                })
                .collect::<Vec<_>>()
                .join(" | "),
            "restore valid authority or use the named recovery command for a derived index",
        ),
        journal::VerifyError::Open(error) => Failure::refusal(
            "journal::verification-failed",
            "one finalized current journal schema",
            error.to_string(),
            "finish or replace incompatible authority before retrying",
        ),
        journal::VerifyError::Sqlite(_) => {
            Failure::store("journal::verify-failed", error.to_string())
        }
    }
}

fn commit_failure(error: journal::CommitError) -> Failure {
    match error {
        journal::CommitError::Refused(_) => Failure::refusal(
            "journal::recovery-refused",
            "one current exclusive recovery transaction",
            error.to_string(),
            "verify the current authority and retry while the resident is quiescent",
        ),
        journal::CommitError::Fault(_) => {
            Failure::store("journal::recovery-failed", error.to_string())
        }
    }
}

fn bundle_failure(error: journal::BackupVerifyError) -> Failure {
    Failure::store("backup::self-verification-failed", error.to_string())
}

fn count(value: usize, subject: &str) -> Result<u64, Failure> {
    u64::try_from(value).map_err(|error| {
        Failure::protocol(
            "journal::count-overflow",
            format!("{subject} cannot cross as u64: {error}"),
        )
    })
}

fn reply<B>(position: ReplyPosition, body: B) -> Outcome<B> {
    Outcome::Reply(Reply { position, body })
}

fn require_absent(path: &Path) -> Result<(), Failure> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Failure::environment(
            "backup::output-unavailable",
            format!("{}: {error}", path.display()),
        )),
        Ok(_) => Err(Failure::refusal(
            "backup::output-exists",
            "a nonexistent output path",
            path.display().to_string(),
            "choose a new bundle destination; backups never overwrite",
        )),
    }
}

fn publish_backup(source: &Path, destination: &Path) -> Result<(), Failure> {
    match authority::rename_no_replace(source, destination) {
        Ok(()) => Ok(()),
        Err(error) => match require_absent(destination) {
            Err(failure) => Err(failure),
            Ok(()) => Err(Failure::environment(
                "backup::publish-failed",
                format!("{}: {error}", destination.display()),
            )),
        },
    }
}

#[cfg(test)]
mod tests;

fn create_staging_directory(parent: &Path) -> Result<(PathBuf, DirectoryCleanup), Failure> {
    for _ in 0..128 {
        let path = parent.join(format!(
            ".spice-backup-{}-{}",
            std::process::id(),
            NEXT_BACKUP.fetch_add(1, Ordering::Relaxed)
        ));
        match authority::create_private_directory(&path) {
            Ok(()) => {
                return Ok((path.clone(), DirectoryCleanup(Some(path))));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(Failure::environment(
                    "backup::stage-failed",
                    format!("{}: {error}", path.display()),
                ));
            }
        }
    }
    Err(Failure::environment(
        "backup::stage-exhausted",
        parent.display().to_string(),
    ))
}

fn create_private_directories(path: &Path) -> Result<(), Failure> {
    authority::create_private_directories(path).map_err(|error| {
        Failure::environment(
            "backup::stage-failed",
            format!("{}: {error}", path.display()),
        )
    })
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let mut file = authority::create_private_file(path).map_err(|error| {
        Failure::environment(
            "backup::write-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    file.write_all(bytes).map_err(|error| {
        Failure::environment(
            "backup::write-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    file.sync_all().map_err(|error| {
        Failure::environment(
            "backup::sync-failed",
            format!("{}: {error}", path.display()),
        )
    })
}

fn write_backup_member(
    path: &Path,
    member: &artifact::BackupMember,
) -> Result<(u64, Digest), Failure> {
    let mut target = authority::create_private_file(path).map_err(|error| {
        Failure::environment(
            "backup::write-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    let copied = member.copy_to(&mut target).map_err(|error| {
        Failure::environment(
            "backup::write-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    if copied != member.bytes() {
        return Err(Failure::protocol(
            "backup::artifact-capture-invalid",
            format!(
                "{} declares {} bytes but copied {copied}",
                member.path(),
                member.bytes()
            ),
        ));
    }
    authority::set_file_mode(&target, member.mode()).map_err(|error| {
        Failure::environment(
            "backup::mode-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    target.sync_all().map_err(|error| {
        Failure::environment(
            "backup::sync-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    let file = File::open(path).map_err(|error| {
        Failure::environment(
            "backup::digest-failed",
            format!("{}: {error}", path.display()),
        )
    })?;
    let (digested, digest) =
        Digest::compute_reader(Domain::JournalBackupMember, file, member.bytes().max(1)).map_err(
            |error| {
                Failure::environment(
                    "backup::digest-failed",
                    format!("{}: {error}", path.display()),
                )
            },
        )?;
    if digested != member.bytes() {
        return Err(Failure::protocol(
            "backup::artifact-capture-invalid",
            format!(
                "{} declares {} bytes but staged digest read {digested}",
                member.path(),
                member.bytes()
            ),
        ));
    }
    Ok((digested, digest))
}

fn relative_spelling(path: &Path) -> Result<String, Failure> {
    path.components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
        .ok_or_else(|| {
            Failure::protocol(
                "backup::member-path-invalid",
                format!("{} is not UTF-8", path.display()),
            )
        })
}

fn sync_tree_directories(root: &Path) -> Result<(), Failure> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        for entry in fs::read_dir(&directory).map_err(|error| {
            Failure::environment(
                "backup::sync-failed",
                format!("{}: {error}", directory.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                Failure::environment(
                    "backup::sync-failed",
                    format!("{}: {error}", directory.display()),
                )
            })?;
            if entry
                .file_type()
                .map_err(|error| {
                    Failure::environment(
                        "backup::sync-failed",
                        format!("{}: {error}", entry.path().display()),
                    )
                })?
                .is_dir()
            {
                directories.push(entry.path());
            }
        }
        index += 1;
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn artifact_failure(error: artifact::Error) -> Failure {
    match error.class() {
        artifact::ErrorClass::Refusal => Failure::refusal(
            "backup::artifact-refused",
            "one current artifact store generation",
            error.to_string(),
            "repair or migrate the artifact store with the release that owns it",
        ),
        artifact::ErrorClass::Corrupt => {
            Failure::store("backup::artifact-corrupt", error.to_string())
        }
        artifact::ErrorClass::Environment => {
            Failure::environment("backup::artifact-unavailable", error.to_string())
        }
    }
}

fn sync_directory(path: &Path) -> Result<(), Failure> {
    authority::Directory::open(path)
        .and_then(|directory| directory.sync())
        .map_err(|error| {
            Failure::environment(
                "backup::sync-failed",
                format!("{}: {error}", path.display()),
            )
        })
}

fn mode(path: &Path) -> Result<u32, Failure> {
    File::open(path)
        .and_then(|file| authority::file_mode(&file))
        .map_err(|error| {
            Failure::environment(
                "backup::metadata-failed",
                format!("{}: {error}", path.display()),
            )
        })
}

struct DirectoryCleanup(Option<PathBuf>);

impl DirectoryCleanup {
    fn disarm(&mut self) {
        self.0.take();
    }
}

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    fn refusal(
        reason: impl Into<String>,
        expected: impl Into<String>,
        observed: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                reason.into(),
                None,
                Some(expected.into()),
                Some(observed.into()),
            )
            .with_detail(detail),
        ))
    }

    fn environment(reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class: FaultClass::Environment,
            reason: reason.into(),
            detail: detail.into(),
        })
    }

    fn store(reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class: FaultClass::Store,
            reason: reason.into(),
            detail: detail.into(),
        })
    }

    fn protocol(reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class: FaultClass::Protocol,
            reason: reason.into(),
            detail: detail.into(),
        })
    }

    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}
