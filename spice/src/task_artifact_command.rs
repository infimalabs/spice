use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use artifact::{
    AddRequest, Artifact, ArtifactAnswer, ArtifactContent, ArtifactReader, ErrorClass, Retention,
    Store,
};
use spice::cli::Contract;
use spice::repository::{Repository, StateError, StatePaths};
use wire::output::Export;
use wire::rooms::{RoomContributed, RoomPublicationContent, RoomPublished, RoomSource};
use wire::{
    Digest, Fault, FaultClass, Instant, Outcome, Refusal, Reply, ReplyPosition, Seq, SpiceCommand,
    TaskHandle, TaskStamp,
};

#[cfg(test)]
mod tests;

pub(crate) fn run(matches: &clap::ArgMatches, contract: &Contract) -> Outcome<ArtifactAnswer> {
    match working_repository().and_then(|repository| run_inner(&repository, matches, contract)) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

/// Execute one artifact read or change against `repository`.
pub(crate) fn run_at(
    repository: &Repository,
    matches: &clap::ArgMatches,
    contract: &Contract,
) -> Outcome<ArtifactAnswer> {
    match run_inner(repository, matches, contract) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

fn working_repository() -> Result<Repository, Failure> {
    let working = std::env::current_dir().map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "artifact::working-directory-unavailable",
            error.to_string(),
        )
    })?;
    Repository::discover(&working).map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            error.to_string(),
        )
    })
}

fn run_inner(
    repository: &Repository,
    matches: &clap::ArgMatches,
    contract: &Contract,
) -> Result<(ReplyPosition, ArtifactAnswer), Failure> {
    let snapshot = snapshot(repository)?;
    let store = Store::new(snapshot.state.artifacts(), snapshot.state.artifacts_lock());
    let position = ReplyPosition::Snapshot(snapshot.cursor);
    match contract.path {
        ["task", "artifact", "add"] => {
            let selected = selected_task(matches, &snapshot)?;
            let source = matches
                .get_one::<String>("PATH")
                .expect("generated artifact add parser requires PATH");
            let retention = matches
                .get_one::<String>("retention")
                .map_or(Ok(Retention::Permanent), |value| value.parse())
                .map_err(|error: artifact::VocabularyError| {
                    Failure::refusal(
                        "artifact::retention-invalid",
                        "permanent or prunable",
                        error.to_string(),
                        "choose one named retention policy",
                    )
                })?;
            let created_at = now()?;
            let (artifact, budget) = store
                .add(AddRequest {
                    task: &selected.born,
                    source: Path::new(source),
                    name: matches.get_one::<String>("name").map(String::as_str),
                    media_type: matches.get_one::<String>("type").map(String::as_str),
                    retention,
                    created_at,
                })
                .map_err(artifact_failure)?;
            Ok((
                position,
                ArtifactAnswer::Added {
                    task: selected.handle,
                    artifact,
                    budget,
                },
            ))
        }
        ["task", "artifact", "list"] => {
            let selected = selected_task(matches, &snapshot)?;
            let request = wire::output::Request {
                rows: count(matches, "limit")?,
                bytes: count(matches, "max-bytes")?,
                text: None,
            };
            let after = matches.get_one::<String>("after").map(String::as_str);
            let answer = list_answer(&store, &selected.born, selected.handle, request, after)?;
            Ok((position, answer))
        }
        ["task", "artifact", "show"] => {
            let selected = selected_task(matches, &snapshot)?;
            let artifact_id = matches
                .get_one::<String>("ARTIFACT")
                .expect("generated artifact show parser requires ARTIFACT");
            let window = window(
                count(matches, "offset")?,
                count(matches, "bytes")?,
                matches.get_one::<String>("output").map(String::as_str),
            )?;
            let answer = show_answer(&store, &selected.born, selected.handle, artifact_id, window)?;
            Ok((position, answer))
        }
        ["task", "artifact", "remove"] => {
            Ok((position, remove_answer(&store, &snapshot, matches)?))
        }
        ["task", "artifact", "prune"] => {
            let older = matches
                .get_one::<String>("older-than")
                .map(|value| artifact_age(value))
                .transpose()?;
            let cutoff = older.as_ref().map(cutoff).transpose()?;
            let older_than_seconds = older.as_ref().map(Duration::as_secs);
            let plan = store
                .plan_prune(&snapshot.statuses, cutoff, older_than_seconds)
                .map_err(artifact_failure)?;
            let applied = matches.get_flag("apply");
            if applied {
                store.apply_prune(&plan).map_err(artifact_failure)?;
            }
            Ok((position, ArtifactAnswer::Pruned { plan, applied }))
        }
        _ => unreachable!("artifact dispatcher receives only artifact leaves"),
    }
}

/// Drop one citation of a pending task unless its room cites the payload.
fn remove_answer(
    store: &Store,
    snapshot: &Snapshot,
    matches: &clap::ArgMatches,
) -> Result<ArtifactAnswer, Failure> {
    let selected = selected_task(matches, snapshot)?;
    let artifact_id = matches
        .get_one::<String>("ARTIFACT")
        .expect("generated artifact remove parser requires ARTIFACT");
    let status = snapshot
        .statuses
        .get(&selected.born)
        .map_or("", |(_, status)| status.as_str());
    if status != "pending" {
        return Err(Failure::refusal(
            "artifact::task-settled",
            "one pending task",
            format!("{} is {status}", selected.handle),
            "settled evidence stays; prune removes expired prunable artifacts",
        ));
    }
    let cited = room_cited(snapshot.state.journal(), &selected.born)?;
    let (artifact, budget) = store
        .remove(&selected.born, artifact_id, &cited)
        .map_err(artifact_failure)?;
    Ok(ArtifactAnswer::Removed {
        task: selected.handle,
        artifact,
        budget,
    })
}

struct Snapshot {
    state: StatePaths,
    cursor: Seq,
    statuses: BTreeMap<TaskStamp, (TaskHandle, String)>,
}

fn snapshot(repository: &Repository) -> Result<Snapshot, Failure> {
    let state = repository.admit_state().map_err(state_failure)?;
    let journal = journal::JournalReader::open(state.journal()).map_err(|error| {
        Failure::fault(
            FaultClass::Store,
            "artifact::journal-unreadable",
            error.to_string(),
        )
    })?;
    // Keep every bounded page and the reply cursor in one read-only snapshot.
    // The transaction ends before artifact payload I/O or store locking.
    let (cursor, statuses) = journal
        .read_with(|read| {
            let cursor = read.cursor().map_err(task_heads_failure)?.ok_or_else(|| {
                Failure::fault(
                    FaultClass::Store,
                    "artifact::cursor-missing",
                    "journal has no current task snapshot",
                )
            })?;
            let mut statuses = BTreeMap::new();
            let mut after = None;
            loop {
                let page = read
                    .task_head_page(after.as_ref(), journal::FactReadBudget::RETAINED)
                    .map_err(task_heads_failure)?;
                if page.witness.cursor != Some(cursor) {
                    return Err(task_heads_failure(journal::GuardError::Invalid(
                        "task snapshot authority moved between bounded pages".to_owned(),
                    )));
                }
                for heads in &page.rows {
                    let entry = board::Entry::new(
                        heads.statement().body().clone(),
                        heads.workflow().body().clone(),
                        heads.completion().body().clone(),
                        heads.claim_bodies(),
                    )
                    .map_err(|error| {
                        Failure::fault(
                            FaultClass::Store,
                            "artifact::task-head-invalid",
                            error.to_string(),
                        )
                    })?;
                    statuses.insert(
                        entry.born().clone(),
                        (
                            entry.handle().clone(),
                            entry.workflow().status.as_str().to_owned(),
                        ),
                    );
                }
                after = page.continuation;
                if after.is_none() {
                    break;
                }
            }
            Ok((cursor, statuses))
        })
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => task_heads_failure(error),
            journal::ReadError::Answer(failure) => failure,
        })?;
    Ok(Snapshot {
        state,
        cursor,
        statuses,
    })
}

fn task_heads_failure(error: journal::GuardError) -> Failure {
    Failure::fault(
        FaultClass::Store,
        "artifact::task-heads-unreadable",
        error.to_string(),
    )
}

/// Every artifact digest the task's room cites: a contribution's sources or a
/// linked publication. Removing such a citation would leave a published room
/// source unreadable.
pub(crate) fn room_cited(journal: &Path, room: &TaskStamp) -> Result<BTreeSet<Digest>, Failure> {
    let unreadable =
        |error: String| Failure::fault(FaultClass::Store, "artifact::room-unreadable", error);
    let journal =
        journal::JournalReader::open(journal).map_err(|error| unreadable(error.to_string()))?;
    journal
        .read_with(|read| {
            let mut cited = BTreeSet::new();
            let mut after = None;
            loop {
                let page = read
                    .room_publications(room, after, journal::FactReadBudget::RETAINED)
                    .map_err(|error| unreadable(error.to_string()))?;
                cited.extend(cited_digests(&page.rows));
                after = page.continuation;
                if after.is_none() {
                    return Ok(cited);
                }
            }
        })
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => unreadable(error.to_string()),
            journal::ReadError::Answer(failure) => failure,
        })
}

/// The artifact digests these room publications cite: a contribution's
/// sources or a linked publication.
pub(crate) fn cited_digests<'a>(
    rows: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
) -> BTreeSet<Digest> {
    rows.into_iter()
        .flat_map(|row| {
            if let Some(contributed) = row.fact.read::<RoomContributed>() {
                contributed.sources
            } else if let Some(published) = row.fact.read::<RoomPublished>()
                && let RoomPublicationContent::Linked { link } = published.content
            {
                vec![link]
            } else {
                Vec::new()
            }
        })
        .filter_map(|source| match source {
            RoomSource::Artifact { digest } => Some(digest),
            _ => None,
        })
        .collect()
}

struct SelectedTask {
    handle: TaskHandle,
    born: TaskStamp,
}

fn selected_task(matches: &clap::ArgMatches, snapshot: &Snapshot) -> Result<SelectedTask, Failure> {
    let text = matches
        .get_one::<String>("HANDLE")
        .expect("generated artifact parser requires HANDLE");
    let task = text.parse::<TaskHandle>().map_err(|error| {
        Failure::refusal(
            "artifact::task-invalid",
            "one canonical task handle",
            error.to_string(),
            "read board and pass the task handle",
        )
    })?;
    let born = task.born().map_err(|error| {
        Failure::refusal(
            "artifact::task-invalid",
            "one handle carrying a canonical birth stamp",
            error.to_string(),
            "read board and pass the task handle",
        )
    })?;
    match snapshot.statuses.get(&born) {
        Some((current, _)) => {
            if let Some(notice) = crate::board_command::rename_notice(&task, current) {
                eprintln!("{notice}");
            }
            Ok(SelectedTask {
                handle: current.clone(),
                born,
            })
        }
        None => Err(Failure::refusal(
            "artifact::task-unknown",
            "one existing task",
            task.to_string(),
            "read board and pass an existing task handle",
        )),
    }
}

fn now() -> Result<Instant, Failure> {
    Instant::new(jiff::Timestamp::now()).map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "artifact::clock-invalid",
            error.to_string(),
        )
    })
}

fn artifact_age(word: &str) -> Result<Duration, Failure> {
    let parsed = || {
        let (&suffix, digits) = word.as_bytes().split_last()?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let unit = match suffix {
            b's' => 1_u64,
            b'm' => 60,
            b'h' => 60 * 60,
            b'd' => 24 * 60 * 60,
            b'w' => 7 * 24 * 60 * 60,
            _ => return None,
        };
        word[..word.len() - 1]
            .parse::<u64>()
            .ok()?
            .checked_mul(unit)
            .map(Duration::from_secs)
    };
    parsed().ok_or_else(|| {
        Failure::refusal(
            "artifact::age-invalid",
            "an unsigned duration ending in s, m, h, d, or w",
            word,
            "choose a bounded duration such as 30d or omit --older-than",
        )
    })
}

fn cutoff(duration: &Duration) -> Result<Instant, Failure> {
    let duration = jiff::SignedDuration::try_from(*duration).map_err(|error| {
        Failure::refusal(
            "artifact::age-out-of-range",
            "a duration representable by the system clock",
            error.to_string(),
            "choose a smaller --older-than duration",
        )
    })?;
    let cutoff = jiff::Timestamp::now()
        .checked_sub(duration)
        .map_err(|error| {
            Failure::refusal(
                "artifact::age-out-of-range",
                "a duration representable by the system clock",
                error.to_string(),
                "choose a smaller --older-than duration",
            )
        })?;
    Instant::new(cutoff).map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "artifact::clock-invalid",
            error.to_string(),
        )
    })
}

/// Payload bytes a show previews when no `--bytes` is named.
const PREVIEW_BYTES: usize = 4096;

/// Most payload bytes one show window returns: the repository text ceiling.
const WINDOW_MAX: usize = wire::output::CEILING.text;

/// What a show reads of the payload.
enum Window<'a> {
    /// A bounded window; absent bounds take the preview.
    Inspect {
        offset: Option<usize>,
        length: Option<usize>,
    },
    /// The whole payload, to this file.
    Extract(&'a str),
}

fn window<'a>(
    offset: Option<usize>,
    length: Option<usize>,
    output: Option<&'a str>,
) -> Result<Window<'a>, Failure> {
    if let Some(length) = length
        && !(1..=WINDOW_MAX).contains(&length)
    {
        return Err(Failure::refusal(
            "artifact::window-bytes",
            format!("--bytes from 1 to {WINDOW_MAX}"),
            length.to_string(),
            "read a larger payload in windows, or extract it with --output PATH",
        ));
    }
    match output {
        Some(_) if offset.is_some() || length.is_some() => Err(Failure::refusal(
            "artifact::window-extract",
            "--output alone, or a window",
            "--output with --offset or --bytes",
            "extraction writes the whole payload; drop the window bounds",
        )),
        Some(output) => Ok(Window::Extract(output)),
        None => Ok(Window::Inspect { offset, length }),
    }
}

/// One page of the task's citations, newest first from `after` (inclusive).
fn list_answer(
    store: &Store,
    born: &TaskStamp,
    task: TaskHandle,
    request: wire::output::Request,
    after: Option<&str>,
) -> Result<ArtifactAnswer, Failure> {
    let budget = wire::output::INTERACTIVE.budget(request).map_err(|error| {
        Failure::refusal(
            "artifact::list-limit",
            "limits within the list maximum",
            error.to_string(),
            "lower the named limit or page with the continuation",
        )
    })?;
    let artifacts = store.list(born).map_err(artifact_failure)?;
    let held = artifact::ArtifactBudget::of(artifacts.len());
    let (artifacts, omission) = page(&task, artifacts, budget, after)?;
    Ok(ArtifactAnswer::Listed {
        task,
        artifacts,
        budget: held,
        omission,
    })
}

/// One artifact's metadata with a bounded window of its proved payload, or
/// the payload extracted whole to a file.
fn show_answer(
    store: &Store,
    born: &TaskStamp,
    task: TaskHandle,
    artifact_id: &str,
    window: Window<'_>,
) -> Result<ArtifactAnswer, Failure> {
    let mut payload = store
        .open_payload(born, artifact_id)
        .map_err(artifact_failure)?;
    let artifact = payload.artifact().clone();
    let (offset, length) = match window {
        Window::Inspect { offset, length } => (offset, length),
        Window::Extract(output) => {
            let export = extract(Path::new(output), &artifact, &mut payload)?;
            return Ok(ArtifactAnswer::Extracted {
                task,
                artifact,
                export,
            });
        }
    };
    let payload_bytes = usize::try_from(artifact.bytes).expect("artifact byte bound fits usize");
    let (content, end) = match (artifact.media_type.is_text(), offset, length) {
        (false, None, None) => (
            ArtifactContent::Binary {
                path: payload_path(store.root(), born, &artifact.digest)
                    .display()
                    .to_string(),
            },
            None,
        ),
        (true, ..) => {
            let (start, text, end) = text_window(
                &mut payload,
                payload_bytes,
                offset.unwrap_or(0),
                length.unwrap_or(PREVIEW_BYTES),
            )?;
            (
                ArtifactContent::Text {
                    offset: wide(start),
                    text,
                    remaining: wide(payload_bytes - end),
                },
                Some(end),
            )
        }
        (false, ..) => {
            let start = offset.unwrap_or(0).min(payload_bytes);
            let bytes = payload
                .window(
                    wide(start),
                    length.unwrap_or(PREVIEW_BYTES).min(payload_bytes - start),
                )
                .map_err(artifact_failure)?;
            let end = start + bytes.len();
            (
                ArtifactContent::Bytes {
                    offset: wide(start),
                    hex: hex(&bytes),
                    remaining: wide(payload_bytes - end),
                },
                Some(end),
            )
        }
    };
    let next = end.filter(|end| *end < payload_bytes).map(|end| {
        let mut arguments = vec![
            "artifact".to_owned(),
            "show".to_owned(),
            task.to_string(),
            artifact.id.clone(),
            "--offset".to_owned(),
            end.to_string(),
        ];
        if let Some(length) = length {
            arguments.extend(["--bytes".to_owned(), length.to_string()]);
        }
        SpiceCommand::new("task", arguments).expect("artifact window command is valid")
    });
    Ok(ArtifactAnswer::Shown {
        task,
        artifact,
        content,
        next,
    })
}

/// A non-negative count flag.
fn count(matches: &clap::ArgMatches, flag: &str) -> Result<Option<usize>, Failure> {
    matches
        .get_one::<String>(flag)
        .map(|value| {
            value.parse().map_err(|_| {
                Failure::refusal(
                    "artifact::count-invalid",
                    format!("--{flag} as a non-negative integer"),
                    value.clone(),
                    "name a whole number",
                )
            })
        })
        .transpose()
}

/// Read one bounded text window on character boundaries. A start inside a
/// character moves to its end, and a window too short for the next character
/// widens to hold it, so paging always advances.
fn text_window(
    payload: &mut ArtifactReader,
    payload_bytes: usize,
    offset: usize,
    length: usize,
) -> Result<(usize, String, usize), Failure> {
    let requested = offset.min(payload_bytes);
    let mut probe = [0_u8; 4];
    let probed = payload
        .read_window(wide(requested), &mut probe)
        .map_err(artifact_failure)?;
    let skipped = probe[..probed]
        .iter()
        .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
        .count();
    let start = requested + skipped;
    let mut bytes = payload
        .window(
            wide(start),
            length.saturating_add(3).min(payload_bytes - start),
        )
        .map_err(artifact_failure)?;
    let limit = length.min(bytes.len());
    let mut kept = limit;
    if kept < bytes.len() {
        while kept > 0 && (bytes[kept] & 0b1100_0000) == 0b1000_0000 {
            kept -= 1;
        }
    }
    if kept == 0 && !bytes.is_empty() {
        kept = match bytes[0] {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => unreachable!("store proved a UTF-8 character boundary"),
        };
    }
    bytes.truncate(kept);
    let text = String::from_utf8(bytes).expect("store proved text UTF-8");
    Ok((start, text, start + kept))
}

fn wide(length: usize) -> u64 {
    u64::try_from(length).expect("an in-memory length fits u64")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Citations newest first from `after` (inclusive), admitted under `budget`.
fn page(
    task: &TaskHandle,
    mut artifacts: Vec<Artifact>,
    budget: wire::output::Budget,
    after: Option<&str>,
) -> Result<(Vec<Artifact>, Option<wire::output::Omission>), Failure> {
    artifacts.reverse();
    let start = match after {
        None => 0,
        Some(after) => artifacts
            .iter()
            .position(|artifact| artifact.id == after)
            .ok_or_else(|| {
                Failure::refusal(
                    "artifact::cursor-unknown",
                    "an artifact of this task",
                    after.to_owned(),
                    "continue from the cursor the previous page named",
                )
            })?,
    };
    let mut admission = wire::output::Admission::new(budget);
    for artifact in artifacts.into_iter().skip(start) {
        admission.offer(artifact).map_err(|error| {
            Failure::fault(
                FaultClass::Protocol,
                "artifact::list-encode",
                error.to_string(),
            )
        })?;
    }
    let limits = budget.resume();
    let bounded = admission.finish(|resume| {
        // A record alone past the byte maximum cannot ride any page; its own
        // show names it singly.
        let (record, bytes) = match resume {
            wire::output::Resume::At(record) => (record, limits.bytes),
            wire::output::Resume::Widened { record, bytes } => (record, bytes),
            wire::output::Resume::Export(record) => {
                return SpiceCommand::new(
                    "task",
                    ["artifact", "show", &task.to_string(), &record.id],
                )
                .expect("artifact show command is valid");
            }
        };
        let arguments = [
            "artifact".to_owned(),
            "list".to_owned(),
            task.to_string(),
            wire::output::CURSOR_FLAG.to_owned(),
            record.id.clone(),
            wire::output::ROWS_FLAG.to_owned(),
            limits.rows.to_string(),
            wire::output::BYTES_FLAG.to_owned(),
            bytes.to_string(),
        ];
        SpiceCommand::new("task", arguments).expect("artifact list command is valid")
    });
    Ok((bounded.rows, bounded.omission.map(|omission| *omission)))
}

/// Stream the proved payload to `output`, then verify the published file
/// against the artifact's retained digest without retaining either copy.
fn extract(
    output: &Path,
    artifact: &Artifact,
    payload: &mut ArtifactReader,
) -> Result<Export, Failure> {
    payload.rewind().map_err(artifact_failure)?;
    let export =
        spice::export::export_reader(output, payload, artifact.bytes).map_err(Failure::Refusal)?;
    let written = std::fs::File::open(&export.path).map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "artifact::extract-unreadable",
            format!("{}: {error}", export.path),
        )
    })?;
    let observed = wire::Digest::compute_reader(
        wire::Domain::TaskArtifactPayload,
        written,
        artifact.bytes.max(1),
    )
    .map_err(|error| {
        Failure::fault(
            if error.kind() == std::io::ErrorKind::InvalidData {
                FaultClass::Store
            } else {
                FaultClass::Environment
            },
            if error.kind() == std::io::ErrorKind::InvalidData {
                "artifact::extract-mismatch"
            } else {
                "artifact::extract-unreadable"
            },
            format!("{}: {error}", export.path),
        )
    })?;
    if observed.0 != artifact.bytes
        || observed.1 != artifact.digest
        || export.bytes != artifact.bytes
    {
        return Err(Failure::fault(
            FaultClass::Store,
            "artifact::extract-mismatch",
            format!(
                "{} holds {} bytes with digest {}; artifact {} retains {} bytes with digest {}",
                export.path, observed.0, observed.1, artifact.id, artifact.bytes, artifact.digest
            ),
        ));
    }
    Ok(export)
}

fn payload_path(root: &Path, task: &TaskStamp, digest: &wire::Digest) -> PathBuf {
    root.join(task.as_str())
        .join("objects")
        .join(digest.as_str())
        .join("payload")
}

/// The repair an artifact store refusal prints: the store names the refused
/// operand and its reason, including any admitted values.
pub(crate) fn artifact_refusal_detail(error: &artifact::Error) -> String {
    format!("{error}; correct it and retry")
}

fn artifact_failure(error: artifact::Error) -> Failure {
    match error.class() {
        ErrorClass::Refusal => Failure::refusal(
            "artifact::operation-refused",
            "one admitted task artifact operation",
            error.to_string(),
            artifact_refusal_detail(&error),
        ),
        ErrorClass::Corrupt => Failure::fault(
            FaultClass::Store,
            "artifact::store-corrupt",
            error.to_string(),
        ),
        ErrorClass::Environment => Failure::fault(
            FaultClass::Environment,
            "artifact::store-unavailable",
            error.to_string(),
        ),
    }
}

fn state_failure(error: StateError) -> Failure {
    match error {
        StateError::Filesystem { .. } => Failure::fault(
            FaultClass::Environment,
            "repository::state-path-unavailable",
            error.to_string(),
        ),
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

#[derive(Debug)]
pub(crate) enum Failure {
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
                Some("task-artifact".to_owned()),
                Some(expected.into()),
                Some(observed.into()),
            )
            .with_detail(detail),
        ))
    }

    fn fault(class: FaultClass, reason: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class,
            reason: reason.into(),
            detail: detail.into(),
        })
    }

    fn outcome(self) -> Outcome<ArtifactAnswer> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}
