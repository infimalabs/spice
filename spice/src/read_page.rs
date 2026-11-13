//! One bounded page for every paged CLI read.
//!
//! A paged view lists records in relevance order and admits them through the
//! [`wire::output`] contract: `--limit`, `--max-bytes`, and `--max-text` raise
//! detail only up to the [`INTERACTIVE`] maxima, every record is charged its
//! encoded length, and a page that withholds records names the exact command
//! that continues at the first of them.
//!
//! The continuation repeats the caller's own words and adds `--after CURSOR`.
//! A timestamped view's cursor is the first withheld record's instant, with
//! `#N` when N records sharing that instant were already shown; resuming reads
//! only records at or before that instant and skips those N, so records that
//! arrive later never shift a page boundary. A view whose records are not
//! timestamped names its own stable record key instead.
//!
//! `--export FILE` writes the page as one complete document to a named file
//! with its digest receipt. Its byte budget is the repository ceiling rather
//! than the interactive one, so a record too large for the terminal is still
//! reachable; a record beyond the ceiling is refused, never streamed.
//!
//! Refusal reasons are scoped to the command family, the first word of the
//! command path: `session::bound-exceeded`, `journal::cursor-invalid`.

use std::path::PathBuf;

use clap::ArgMatches;
use clap::parser::ValueSource;
use serde::Serialize;
use wire::output::{
    self, Admission, Bounded, Budget, CEILING, ClippedText, Export, INTERACTIVE, Limits, Request,
    Resume, Surface,
};
use wire::{Refusal, SpiceCommand};

/// The same rows and text an export writes, charged against the repository
/// ceiling instead of the interactive byte budget.
const EXPORT: Surface = Surface::new(
    Limits {
        rows: INTERACTIVE.default_limits().rows,
        bytes: CEILING.bytes,
        text: INTERACTIVE.default_limits().text,
    },
    CEILING,
);

/// Caller words a continuation states for itself rather than repeating, and
/// the global output-form flag, which the caller chooses per invocation.
const RESTATED: [&str; 5] = ["after", "limit", "max-bytes", "export", crate::cli::JSON];

/// One parsed `--after`, `--limit`, `--max-bytes`, `--max-text`, `--export`.
pub(crate) struct Page {
    family: String,
    budget: Budget,
    after: Option<String>,
    rows: Option<usize>,
    bytes: Option<usize>,
    export: Option<PathBuf>,
    export_name: String,
    words: Vec<String>,
}

/// A page shown on the terminal, or written whole to the named file.
pub(crate) enum Paged<T> {
    Shown(Bounded<T>),
    Exported(Export),
}

/// The instant a timestamped page resumes at, and how many records sharing it
/// were already shown.
struct Cursor {
    ts: String,
    skip: usize,
}

impl Page {
    pub(crate) fn from_matches(matches: &ArgMatches, path: &[&str]) -> Result<Self, Refusal> {
        let family = path.first().copied().unwrap_or("read");
        let request = request(matches, family)?;
        let export = one(matches, "export").map(PathBuf::from);
        let surface = if export.is_some() {
            EXPORT
        } else {
            INTERACTIVE
        };
        let budget = surface.budget(request).map_err(|error| {
            refusal(
                family,
                "bound-exceeded",
                Some(error.to_string()),
                "pass a read bound within the surface maximum, or --export FILE for a complete \
                 page",
            )
        })?;
        Ok(Self {
            family: family.to_owned(),
            budget,
            after: one(matches, "after").cloned(),
            rows: request.rows,
            bytes: request.bytes,
            export,
            export_name: format!("spice-{}.json", path.join("-")),
            words: continuation_words(matches, path),
        })
    }

    /// Refuse the caller's `--after` cursor: it names no record this view
    /// can resume at.
    pub(crate) fn cursor_refusal(&self, detail: impl std::fmt::Display) -> Refusal {
        refusal(
            &self.family,
            "cursor-invalid",
            self.after.clone(),
            format!("pass the --after cursor exactly as a continuation named it: {detail}"),
        )
    }

    pub(crate) const fn limits(&self) -> Limits {
        self.budget.limits()
    }

    /// One text field with whitespace runs collapsed, cut to the text limit.
    pub(crate) fn clip(&self, text: &str) -> ClippedText {
        clip(text, self.budget.limits().text)
    }

    /// The caller's `--after` cursor, verbatim.
    pub(crate) fn after(&self) -> Option<&str> {
        self.after.as_deref()
    }

    /// Records a keyed scan must retain so the page can close on the first
    /// record it withholds.
    pub(crate) const fn retain(&self) -> usize {
        self.budget.limits().rows.saturating_add(1)
    }

    /// The newest instant a timestamped scan considers: the caller's `--end`
    /// tightened to the cursor.
    pub(crate) fn end(&self, end: Option<&str>) -> Result<Option<String>, Refusal> {
        Ok(match (self.cursor()?, end) {
            (Some(cursor), Some(end)) if end < cursor.ts.as_str() => Some(end.to_owned()),
            (Some(cursor), _) => Some(cursor.ts),
            (None, end) => end.map(str::to_owned),
        })
    }

    /// Newest records a timestamped scan must retain so the page can close
    /// on the first record it withholds.
    pub(crate) fn frontier(&self) -> Result<usize, Refusal> {
        let skip = self.cursor()?.map_or(0, |cursor| cursor.skip);
        Ok(self
            .budget
            .limits()
            .rows
            .saturating_add(skip)
            .saturating_add(1))
    }

    /// Page newest-first timestamped records; `population` counts every
    /// record the scan matched at or before [`Page::end`], retained or not.
    pub(crate) fn newest<T: Serialize>(
        self,
        rows: Vec<T>,
        population: usize,
        ts: impl Fn(&T) -> &str,
    ) -> Result<Paged<T>, Refusal> {
        let cursor = self.cursor()?;
        let mut keyed = Vec::with_capacity(rows.len());
        let mut skipped = 0;
        let mut run: Option<(String, usize)> = None;
        for row in rows {
            let at = ts(&row).to_owned();
            let index = match &run {
                Some((previous, count)) if *previous == at => *count,
                _ => 0,
            };
            run = Some((at.clone(), index + 1));
            if cursor
                .as_ref()
                .is_some_and(|cursor| cursor.ts == at && index < cursor.skip)
            {
                skipped += 1;
                continue;
            }
            let key = if index == 0 {
                at
            } else {
                format!("{at}#{index}")
            };
            keyed.push((row, key));
        }
        self.admit(keyed, population.saturating_sub(skipped))
    }

    /// Page a complete in-memory list already in relevance order, resuming
    /// at the record whose key the caller's `--after` names; `cursor`
    /// describes that key when it names no record.
    ///
    /// For views that declare no `--export`.
    pub(crate) fn listed<T: Serialize>(
        self,
        rows: Vec<T>,
        key: impl Fn(&T) -> String,
        cursor: &str,
    ) -> Result<Bounded<T>, Refusal> {
        let start = match self.after() {
            None => 0,
            Some(after) => rows
                .iter()
                .position(|row| key(row) == after)
                .ok_or_else(|| self.cursor_refusal(cursor))?,
        };
        let population = rows.len() - start;
        let retain = self.retain();
        let keyed = rows
            .into_iter()
            .skip(start)
            .take(retain)
            .map(|row| {
                let key = key(&row);
                (row, key)
            })
            .collect();
        self.shown(keyed, population)
    }

    /// [`Page::admit`] for views that declare no `--export`.
    pub(crate) fn shown<T: Serialize>(
        self,
        rows: Vec<(T, String)>,
        population: usize,
    ) -> Result<Bounded<T>, Refusal> {
        assert!(
            self.export.is_none(),
            "only a view that declares --export can export"
        );
        match self.admit(rows, population)? {
            Paged::Shown(bounded) => Ok(bounded),
            Paged::Exported(_) => unreachable!("a page without --export is shown"),
        }
    }

    /// Page records already in relevance order and already past the
    /// caller's cursor, each with the key that resumes at it.
    pub(crate) fn admit<T: Serialize>(
        self,
        rows: Vec<(T, String)>,
        population: usize,
    ) -> Result<Paged<T>, Refusal> {
        let offered = rows.len();
        let mut admission = Admission::new(self.budget);
        let mut stop = None;
        for (row, key) in rows {
            let admitted = admission
                .offer(row)
                .map_err(|error| self.unencodable(&error))?;
            if !admitted && stop.is_none() {
                stop = Some(key);
            }
        }
        let unretained = population.saturating_sub(offered);
        if unretained > 0 {
            if !admission.is_closed() {
                return Err(refusal(
                    &self.family,
                    "page-frontier-short",
                    Some(format!("population={population} retained={offered}")),
                    "report this read; its scan retained fewer records than one page",
                ));
            }
            admission.withhold(unretained);
        }
        let bounded = admission.finish(|resume| {
            let key = stop
                .as_deref()
                .expect("a closed admission refused one keyed record");
            self.continuation(resume, key)
        });
        let Some(path) = &self.export else {
            return Ok(Paged::Shown(bounded));
        };
        if bounded.rows.is_empty() && bounded.omission.is_some() {
            return Err(refusal(
                &self.family,
                "record-exceeds-ceiling",
                bounded.omission.map(|omission| {
                    format!(
                        "next record {} bytes; ceiling {}",
                        omission.next_bytes, CEILING.bytes
                    )
                }),
                "narrow the record with --max-text; no read carries a record beyond the \
                 repository ceiling",
            ));
        }
        let document = wire::encode(&bounded).map_err(|error| self.unencodable(&error))?;
        spice::export::export(path, &document).map(Paged::Exported)
    }

    fn continuation(&self, resume: Resume<'_, impl Sized>, key: &str) -> SpiceCommand {
        let mut words = self.words.clone();
        words.extend(["--after".to_owned(), key.to_owned()]);
        // A count-only read continues at the default row limit.
        if let Some(rows) = self.rows.filter(|rows| *rows > 0) {
            words.extend(["--limit".to_owned(), rows.to_string()]);
        }
        let bytes = match resume {
            Resume::Widened { bytes, .. } => Some(bytes),
            Resume::At(_) | Resume::Export(_) => self.bytes,
        };
        if let Some(bytes) = bytes {
            words.extend(["--max-bytes".to_owned(), bytes.to_string()]);
        }
        if matches!(resume, Resume::Export(_)) || self.export.is_some() {
            let path = self.export.as_ref().map_or_else(
                || self.export_name.clone(),
                |path| path.display().to_string(),
            );
            words.extend(["--export".to_owned(), path]);
        }
        SpiceCommand::try_from(words).expect("a read path is one Spice command")
    }

    fn unencodable(&self, error: &serde_json::Error) -> Refusal {
        refusal(
            &self.family,
            "record-unencodable",
            Some(error.to_string()),
            "report the record that cannot be encoded",
        )
    }

    fn cursor(&self) -> Result<Option<Cursor>, Refusal> {
        let Some(after) = &self.after else {
            return Ok(None);
        };
        let invalid = || self.cursor_refusal("TIMESTAMP or TIMESTAMP#N");
        let (ts, skip) = after
            .split_once('#')
            .map_or(Ok((after.as_str(), 0)), |(ts, skip)| {
                skip.parse::<usize>()
                    .ok()
                    .filter(|skip| *skip > 0)
                    .map(|skip| (ts, skip))
                    .ok_or_else(invalid)
            })?;
        let ts = crate::session_command::project::normalize_bound(ts).map_err(|_| invalid())?;
        Ok(Some(Cursor { ts, skip }))
    }
}

/// The caller's `--limit`, `--max-bytes`, and `--max-text`.
pub(crate) fn request(matches: &ArgMatches, family: &str) -> Result<Request, Refusal> {
    Ok(Request {
        rows: count(matches, family, "limit")?,
        bytes: count(matches, family, "max-bytes")?,
        text: count(matches, family, "max-text")?,
    })
}

/// Collapse whitespace runs and keep at most `max_bytes` of the result.
pub(crate) fn clip(text: &str, max_bytes: usize) -> ClippedText {
    output::clip(&collapse(text), max_bytes)
}

/// `text` as one line: every whitespace run collapsed to one space.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The caller's invocation, less the words a continuation restates.
fn continuation_words(matches: &ArgMatches, path: &[&str]) -> Vec<String> {
    let mut positionals = Vec::new();
    let mut options = Vec::new();
    for id in matches.ids() {
        let name = id.as_str();
        if RESTATED.contains(&name) || matches.value_source(name) != Some(ValueSource::CommandLine)
        {
            continue;
        }
        if matches.try_get_one::<bool>(name).is_ok() {
            options.push(format!("--{name}"));
            continue;
        }
        let values = matches
            .get_raw(name)
            .into_iter()
            .flatten()
            .map(|value| value.to_string_lossy().into_owned());
        if name
            .chars()
            .all(|character| !character.is_ascii_lowercase())
        {
            positionals.extend(values);
        } else {
            for value in values {
                options.extend([format!("--{name}"), value]);
            }
        }
    }
    path.iter()
        .map(|word| (*word).to_owned())
        .chain(positionals)
        .chain(options)
        .collect()
}

fn count(matches: &ArgMatches, family: &str, name: &str) -> Result<Option<usize>, Refusal> {
    one(matches, name)
        .map(|value| {
            value.parse::<usize>().map_err(|_| {
                refusal(
                    family,
                    "bound-invalid",
                    Some(format!("--{name}={value}")),
                    "pass a nonnegative decimal integer bound",
                )
            })
        })
        .transpose()
}

fn one<'a>(matches: &'a ArgMatches, name: &str) -> Option<&'a String> {
    matches.try_get_one::<String>(name).ok().flatten()
}

pub(crate) fn refusal(
    family: &str,
    reason: &str,
    observed: Option<String>,
    detail: impl Into<String>,
) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            format!("{family}::{reason}"),
            Some(format!("{family}-query")),
            Some(format!("one bounded {family} read")),
            observed,
        )
        .with_detail(detail),
    )
}

#[cfg(test)]
mod tests;
