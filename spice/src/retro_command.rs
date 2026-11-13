//! `spice retro`: one bounded, newest-first page of preserved transcript.
//!
//! A selector that resolves exactly one session tape is read through
//! [`listen::transcript_page`]: the scan decodes one bounded window from the
//! tape's end and the page admits whole vendor records under the
//! `--limit`/`--max-bytes`/`--max-text` budget, never above the interactive
//! maxima. `--after` resumes from the generation-authenticated cursor a
//! continuation named, so a replaced tape refuses the cursor rather than
//! paging another transcript. A selector naming several sessions lists them;
//! a locus, or a selector naming nothing, shows its observations.
//!
//! Every withheld row, older window, or clipped text field names the exact
//! command that recovers it. `--export FILE` writes the complete selected
//! transcript, unclipped and unpaged, to that file with its export digest and
//! prints only the receipt.

mod view;

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ArgMatches;
use listen::{
    Dialect, EventKind, Mode, Observation, RetroSelector, RetroSession, ScanError, SourceId,
    StreamOutcome, SurfaceError, TailBudget, TranscriptContinuation, TranscriptCursor,
    TranscriptFilter, TranscriptObservation, TranscriptPageRequest, TranscriptPageRow,
    TranscriptPageStart,
};
use spice::export::ExportFailure;
use wire::output::{Admission, Bounded, Budget, Exhausted, INTERACTIVE, Omission, Request};
use wire::{Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition, SpiceCommand};

use crate::read_page::{self, refusal};
use crate::render::Form;
use view::{
    Clipped, Continuation, History, RetroObservations, RetroPage, RetroSessions, RetroView,
};

const FAMILY: &str = "retro";
const SELECTOR: &str = "LANE|LOCUS|PATH";
/// Directory whose `<lane>/<vendor>` children are the retained sessions.
const ROOT: &str = ".";
/// File a continuation names when only an export recovers withheld bytes.
const EXPORT_NAME: &str = "spice-retro.json";

/// Newest tape bytes one page decodes.
const WINDOW_BYTES: u64 = 256 * 1024;
/// Tape index bytes a scan reads to find the newest members.
const INDEX_BYTES: u64 = 64 * 1024;
/// Longest record a tape holds, with its delimiter: a window that begins
/// inside one retreats over all of it.
const RECORD_BYTES: u64 = vendor::omp::frame::MAX_REASSEMBLED_BYTES as u64 + 1;
const SOURCE_BYTES: u64 = WINDOW_BYTES + INDEX_BYTES + RECORD_BYTES;
const RECORDS: usize = 64 * 1024;
/// Reads beyond the boundary retreat: the index, its extents, and the window.
const EXTENT_PASSES: usize = 256;
const PASSES: usize = (SOURCE_BYTES / listen::BOUNDARY_CHUNK_BYTES + 1) as usize + EXTENT_PASSES;
const DURATION: Duration = Duration::from_secs(5);

pub(crate) fn run(matches: &ArgMatches, form: Form) -> ! {
    let dialects = crate::extension_dialects();
    let outcome = Retro::from_matches(matches).map_or_else(Outcome::Refusal, |retro| {
        retro.read(Path::new(ROOT), &dialects, scan_budget())
    });
    crate::deliver_display(&outcome, form)
}

fn scan_budget() -> TailBudget {
    TailBudget::new(WINDOW_BYTES, SOURCE_BYTES, RECORDS, PASSES, DURATION)
        .expect("retro scan allowances are positive")
}

/// Why a read stopped before it had a view.
enum Stop {
    Refusal(Refusal),
    Fault(Fault),
}

impl From<SurfaceError> for Stop {
    fn from(error: SurfaceError) -> Self {
        Self::Fault(crate::surface_fault(error))
    }
}

/// One parsed `spice retro` invocation.
struct Retro {
    word: Option<String>,
    selector: RetroSelector,
    kinds: Vec<EventKind>,
    filter: TranscriptFilter,
    request: Request,
    budget: Budget,
    after: Option<String>,
    export: Option<PathBuf>,
}

impl Retro {
    fn from_matches(matches: &ArgMatches) -> Result<Self, Refusal> {
        let word = matches.get_one::<String>(SELECTOR).cloned();
        let selector = match &word {
            Some(value) => RetroSelector::parse(value).map_err(|error| {
                Refusal::bare(
                    wire::RefusalDiagnostic::new(
                        "retro::selector".to_owned(),
                        None,
                        Some("LANE, M:O:L, or PATH".to_owned()),
                        Some(value.clone()),
                    )
                    .with_detail(error.to_string()),
                )
            })?,
            None => RetroSelector::Path(PathBuf::from(ROOT)),
        };
        let mut kinds = matches
            .get_many::<String>("kind")
            .into_iter()
            .flatten()
            .map(|value| kind(value))
            .collect::<Result<Vec<_>, _>>()?;
        kinds.sort_unstable();
        kinds.dedup();
        let request = read_page::request(matches, FAMILY)?;
        let budget = INTERACTIVE.budget(request).map_err(|error| {
            refusal(
                FAMILY,
                "bound-exceeded",
                Some(error.to_string()),
                "pass a read bound within the surface maximum, or --export FILE for the complete \
                 transcript",
            )
        })?;
        Ok(Self {
            word,
            selector,
            filter: TranscriptFilter::only(kinds.iter().copied()),
            kinds,
            request,
            budget,
            after: matches.get_one::<String>("after").cloned(),
            export: matches.get_one::<String>("export").map(PathBuf::from),
        })
    }

    fn read(
        &self,
        root: &Path,
        dialects: &[(&'static str, Dialect)],
        scan: TailBudget,
    ) -> Outcome<RetroView> {
        let view = match &self.export {
            Some(path) => self.export(root, dialects, path),
            None => self.page(root, dialects, scan),
        };
        match view {
            Ok(body) => Outcome::Reply(Reply {
                position: ReplyPosition::Direct {},
                body,
            }),
            Err(Stop::Refusal(refusal)) => Outcome::Refusal(refusal),
            Err(Stop::Fault(fault)) => Outcome::Fault(fault),
        }
    }

    fn page(
        &self,
        root: &Path,
        dialects: &[(&'static str, Dialect)],
        scan: TailBudget,
    ) -> Result<RetroView, Stop> {
        let mut sessions = listen::retro_sessions(root, &self.selector, dialects)
            .map_err(|error| Stop::Fault(crate::surface_fault(error)))?;
        if matches!(self.selector, RetroSelector::Locus(_)) || sessions.is_empty() {
            return self.observations(root, dialects);
        }
        if sessions.len() > 1 {
            return self.sessions(sessions);
        }
        let session = sessions.remove(0);
        let page = listen::transcript_page(
            &session.tape,
            session.lane.clone(),
            session.dialect,
            TranscriptPageRequest {
                scan,
                output: self.budget,
                start: self.start()?,
                filter: self.filter.clone(),
            },
        )
        .map_err(|error| self.scan_stop(error))?;
        let rows = remap(page.rows, |_, omission| {
            self.page_continuation(&omission.continuation, widened(omission))
        });
        let clipped = Clipped::of(
            rows.rows
                .iter()
                .filter_map(|row| match row {
                    TranscriptPageRow::Record { record } => Some(&record.observations),
                    TranscriptPageRow::Status { .. } => None,
                })
                .flatten(),
            || self.export_continuation(),
        );
        let history = page.history.map(|history| History {
            withheld_source_bytes: history.withheld_source_bytes,
            continuation: Continuation::Next {
                command: self.next_command(Some(&history.continuation.to_string()), None),
            },
        });
        Ok(RetroView::Page(RetroPage {
            session,
            rows,
            history,
            clipped,
            work: page.work,
        }))
    }

    /// The resolved sessions after `--after TAPE`, in tape-path order.
    fn sessions(&self, sessions: Vec<RetroSession>) -> Result<RetroView, Stop> {
        let mut admission = Admission::new(self.budget);
        for session in sessions.into_iter().filter(|session| {
            self.after
                .as_deref()
                .is_none_or(|after| session.tape.as_path() > Path::new(after))
        }) {
            admission.offer(session).map_err(unencodable)?;
        }
        let rows = remap(admission.finish(|_| ()), |shown, omission| {
            if exceeds_maximum(omission) {
                return self.export_continuation();
            }
            let after = shown
                .last()
                .map(|session| session.tape.display().to_string())
                .or_else(|| self.after.clone());
            Continuation::Next {
                command: self.next_command(after.as_deref(), widened(omission)),
            }
        });
        Ok(RetroView::Sessions(RetroSessions {
            selector: self.selector_text(),
            rows,
        }))
    }

    /// A locus replay, or the evidence that nothing resolved: one record at
    /// most, so whatever its budget withholds is recovered by export.
    fn observations(
        &self,
        root: &Path,
        dialects: &[(&'static str, Dialect)],
    ) -> Result<RetroView, Stop> {
        let outcome = listen::retro(root, self.selector.clone(), dialects)
            .map_err(|error| Stop::Fault(crate::surface_fault(error)))?;
        let mut admission = Admission::new(self.budget);
        for observation in outcome.observations {
            if self.selected(&observation) {
                admission
                    .offer(TranscriptObservation::clipped(observation, self.budget))
                    .map_err(unencodable)?;
            }
        }
        let rows = remap(admission.finish(|_| ()), |_, _| self.export_continuation());
        let clipped = Clipped::of(&rows.rows, || self.export_continuation());
        Ok(RetroView::Observations(RetroObservations {
            selector: self.selector_text(),
            rows,
            clipped,
        }))
    }

    /// The document is `wire::encode` of the whole selected outcome, written
    /// one observation at a time: the encoding of the same outcome with no
    /// observations supplies the frame around them.
    fn export(
        &self,
        root: &Path,
        dialects: &[(&'static str, Dialect)],
        path: &Path,
    ) -> Result<RetroView, Stop> {
        let frame = wire::encode(&StreamOutcome {
            mode: Mode::Retro,
            root: root.to_path_buf(),
            observations: Vec::new(),
        })
        .map_err(unencodable)?;
        let open = frame
            .strip_suffix(b"]}")
            .expect("an outcome's encoding ends with its observation list");
        // A failed write is reported by the export as its own refusal.
        let wrote = |error| unencodable(serde_json::Error::io(error));
        spice::export::export_with(path, |writer| {
            writer.write_all(open).map_err(wrote)?;
            let mut first = true;
            listen::retro_each(root, self.selector.clone(), dialects, |observation| {
                if !self.selected(&observation) {
                    return Ok(());
                }
                if !std::mem::take(&mut first) {
                    writer.write_all(b",").map_err(wrote)?;
                }
                serde_json::to_writer(&mut *writer, &observation).map_err(unencodable)
            })?;
            writer.write_all(b"]}").map_err(wrote)
        })
        .map(RetroView::Exported)
        .map_err(|failure| match failure {
            ExportFailure::Refused(refusal) => Stop::Refusal(refusal),
            ExportFailure::Produce(stop) => stop,
        })
    }

    /// Absence and ambiguity are what the selector resolved, so a kind filter
    /// never hides them.
    fn selected(&self, observation: &Observation) -> bool {
        matches!(
            observation,
            Observation::Missing { .. }
                | Observation::Ambiguous { .. }
                | Observation::Absent { .. }
        ) || self.filter.admits(observation)
    }

    fn start(&self) -> Result<TranscriptPageStart, Stop> {
        let Some(after) = &self.after else {
            return Ok(TranscriptPageStart::Current { expected: None });
        };
        if let Ok(cursor) = after.parse::<TranscriptCursor>() {
            return Ok(TranscriptPageStart::Before(cursor));
        }
        after
            .parse::<SourceId>()
            .map(|source| TranscriptPageStart::Current {
                expected: Some(source),
            })
            .map_err(|_| {
                Stop::Refusal(refusal(
                    FAMILY,
                    "cursor-invalid",
                    Some(after.clone()),
                    "pass the --after cursor exactly as a continuation named it: \
                     SOURCE:MEMBER:OFFSET or SOURCE",
                ))
            })
    }

    fn scan_stop(&self, error: SurfaceError) -> Stop {
        match error {
            SurfaceError::Scan {
                source:
                    ScanError::SourceChanged { .. }
                    | ScanError::CheckpointRetreated { .. }
                    | ScanError::CheckpointGap { .. },
                ..
            } if self.after.is_some() => Stop::Refusal(refusal(
                FAMILY,
                "cursor-stale",
                self.after.clone(),
                format!(
                    "the tape was replaced after this cursor was issued; restart with {}",
                    self.next_command(None, None)
                ),
            )),
            error => Stop::Fault(crate::surface_fault(error)),
        }
    }

    fn page_continuation(
        &self,
        continuation: &TranscriptContinuation,
        bytes: Option<usize>,
    ) -> Continuation {
        match continuation {
            TranscriptContinuation::Export { continuation, .. } => Continuation::Export {
                export: self.export_command(),
                then: Some(self.next_command(Some(&resume_word(continuation)), None)),
            },
            TranscriptContinuation::Current { .. } | TranscriptContinuation::Cursor { .. } => {
                Continuation::Next {
                    command: self.next_command(Some(&resume_word(continuation)), bytes),
                }
            }
        }
    }

    fn export_continuation(&self) -> Continuation {
        Continuation::Export {
            export: self.export_command(),
            then: None,
        }
    }

    /// `retro`, the selector, and the kind filter, as the caller wrote them.
    fn base_words(&self) -> Vec<String> {
        let mut words = vec![FAMILY.to_owned()];
        words.extend(self.word.clone());
        for kind in &self.kinds {
            words.extend(["--kind".to_owned(), kind.as_str().to_owned()]);
        }
        words
    }

    fn next_command(&self, after: Option<&str>, bytes: Option<usize>) -> SpiceCommand {
        let mut words = self.base_words();
        // A count-only read continues at the default row limit.
        for (flag, bound) in [
            ("--limit", self.request.rows.filter(|rows| *rows > 0)),
            ("--max-bytes", bytes.or(self.request.bytes)),
            ("--max-text", self.request.text),
        ] {
            if let Some(bound) = bound {
                words.extend([flag.to_owned(), bound.to_string()]);
            }
        }
        if let Some(after) = after {
            words.extend(["--after".to_owned(), after.to_owned()]);
        }
        SpiceCommand::try_from(words).expect("a retro read is one Spice command")
    }

    fn export_command(&self) -> SpiceCommand {
        let mut words = self.base_words();
        words.extend(["--export".to_owned(), EXPORT_NAME.to_owned()]);
        SpiceCommand::try_from(words).expect("a retro export is one Spice command")
    }

    fn selector_text(&self) -> String {
        self.word.clone().unwrap_or_else(|| ROOT.to_owned())
    }
}

fn kind(value: &str) -> Result<EventKind, Refusal> {
    EventKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or_else(|| {
            refusal(
                FAMILY,
                "kind-invalid",
                Some(value.to_owned()),
                format!(
                    "pass one of {}",
                    EventKind::ALL.map(EventKind::as_str).join(", ")
                ),
            )
        })
}

/// The cursor word a transcript continuation resumes at.
fn resume_word(continuation: &TranscriptContinuation) -> String {
    match continuation {
        TranscriptContinuation::Current { source } => source.to_string(),
        TranscriptContinuation::Cursor { cursor } => cursor.to_string(),
        TranscriptContinuation::Export { continuation, .. } => resume_word(continuation),
    }
}

/// The byte limit that admits the first withheld row alone, when it fits
/// under the interactive maximum but not the page's limit.
fn widened<C>(omission: &Omission<C>) -> Option<usize> {
    (omission.exhausted == Exhausted::Bytes
        && omission.next_bytes > omission.limits.bytes
        && !exceeds_maximum(omission))
    .then_some(omission.next_bytes)
}

/// Whether the first withheld row exceeds every interactive page.
fn exceeds_maximum<C>(omission: &Omission<C>) -> bool {
    omission.exhausted == Exhausted::Bytes && omission.next_bytes > INTERACTIVE.maximum().bytes
}

/// The same rows and counts with a retro continuation.
fn remap<T, A>(
    bounded: Bounded<T, A>,
    continuation: impl FnOnce(&[T], &Omission<A>) -> Continuation,
) -> Bounded<T, Continuation> {
    let Bounded { rows, omission } = bounded;
    let omission = omission.map(|omission| {
        Box::new(Omission {
            continuation: continuation(&rows, &omission),
            population: omission.population,
            shown: omission.shown,
            withheld: omission.withheld,
            shown_bytes: omission.shown_bytes,
            exhausted: omission.exhausted,
            next_bytes: omission.next_bytes,
            limits: omission.limits,
        })
    });
    Bounded { rows, omission }
}

fn unencodable(error: serde_json::Error) -> Stop {
    Stop::Fault(Fault {
        class: FaultClass::Protocol,
        reason: "retro::record-unencodable".to_owned(),
        detail: error.to_string(),
    })
}

#[cfg(test)]
mod tests;
