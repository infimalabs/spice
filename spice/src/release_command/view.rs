//! What a person reads from `release notes`.
//!
//! The document is complete only as a file. The terminal gets bounded
//! metadata and a relevance-ordered page; the complete note Markdown is an
//! explicit `--output` export.

use std::fmt::{self, Write as _};
use std::path::Path;

use serde::Serialize;
use wire::output::{Admission, Bounded, Budget, ClippedText, Export, INTERACTIVE, Request};
use wire::{Digest, Refusal, ReleaseNotes, SpiceCommand};

/// The file a complete note export names by default.
const NOTES_FILE: &str = "release-notes.md";

/// One `release notes` invocation's answer.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum NotesView {
    Preview(Box<NotesPreview>),
    Export(NotesExport),
}

impl NotesView {
    pub(crate) fn text(&self) -> Option<&str> {
        Some(match self {
            Self::Preview(preview) => &preview.text,
            Self::Export(export) => &export.text,
        })
    }
}

/// Bounded metadata and the most relevant entries of one note document.
#[derive(Debug, Serialize)]
pub(crate) struct NotesPreview {
    pub(crate) version: u16,
    pub(crate) identity: Digest,
    pub(crate) release_version: String,
    pub(crate) release_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) previous_tag: Option<String>,
    pub(crate) bounded_recent_window: bool,
    /// Byte length of the complete Markdown document.
    pub(crate) document_bytes: usize,
    /// Entries with the most landings first, then in first-landing order.
    pub(crate) entries: Bounded<NotesRow>,
    #[serde(skip)]
    text: String,
}

/// One task-level change as the preview names it.
#[derive(Debug, Serialize)]
pub(crate) struct NotesRow {
    pub(crate) topic: String,
    pub(crate) subject: ClippedText,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task: Option<String>,
    pub(crate) landings: usize,
}

impl fmt::Display for NotesRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "  {:<12} {}", self.topic, self.subject)?;
        if let Some(task) = &self.task {
            write!(formatter, " [{task}]")?;
        }
        if self.landings > 1 {
            write!(formatter, " ({} landings)", self.landings)?;
        }
        Ok(())
    }
}

/// The receipt for one complete note document written to a file.
#[derive(Debug, Serialize)]
pub(crate) struct NotesExport {
    pub(crate) identity: Digest,
    pub(crate) release_version: String,
    pub(crate) release_commit: String,
    pub(crate) entries: usize,
    pub(crate) export: Export,
    #[serde(skip)]
    text: String,
}

/// Write the complete document to `output`, or page its entries.
pub(super) fn notes_view(
    notes: &ReleaseNotes,
    output: Option<&str>,
    release_commit: Option<&str>,
) -> Result<NotesView, Refusal> {
    let Some(output) = output else {
        let budget = INTERACTIVE
            .budget(Request::default())
            .expect("default limits sit under the interactive maxima");
        return Ok(NotesView::Preview(Box::new(notes_preview(
            notes,
            budget,
            release_commit,
        ))));
    };
    let export = spice::export::export(Path::new(output), notes.document.as_bytes())?;
    let text = format!(
        "release notes {} identity {}: {} entries; {export}",
        notes.release_version,
        notes.identity,
        notes.entries.len()
    );
    Ok(NotesView::Export(NotesExport {
        identity: notes.identity.clone(),
        release_version: notes.release_version.clone(),
        release_commit: notes.release_commit.clone(),
        entries: notes.entries.len(),
        export,
        text,
    }))
}

/// The command that writes this note document to a file.
fn notes_export(release_commit: Option<&str>) -> SpiceCommand {
    let mut arguments = vec!["notes".to_owned()];
    if let Some(commit) = release_commit {
        arguments.extend(["--release-commit".to_owned(), commit.to_owned()]);
    }
    arguments.extend(["--output".to_owned(), NOTES_FILE.to_owned()]);
    SpiceCommand::new("release", arguments).expect("release is a command token")
}

/// Bound one note document to its metadata and most relevant entries.
pub(super) fn notes_preview(
    notes: &ReleaseNotes,
    budget: Budget,
    release_commit: Option<&str>,
) -> NotesPreview {
    let mut order = (0..notes.entries.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| std::cmp::Reverse(notes.entries[*index].references.len()));
    let mut admission = Admission::new(budget);
    for (position, index) in order.iter().enumerate() {
        if admission.is_closed() {
            admission.withhold(order.len() - position);
            break;
        }
        let entry = &notes.entries[*index];
        admission
            .offer(NotesRow {
                topic: entry.topic.clone(),
                subject: budget.clip(&entry.subject),
                task: entry.task.clone(),
                landings: entry.references.len(),
            })
            .expect("a note row of strings and counts always encodes");
    }
    let entries = admission.finish(|_| notes_export(release_commit));
    let mut text = format!(
        "release notes {} at {}: {} entries, {} bytes, identity {}",
        notes.release_version,
        short(&notes.release_commit),
        notes.entries.len(),
        notes.document.len(),
        notes.identity
    );
    match (&notes.previous_tag, notes.bounded_recent_window) {
        (Some(tag), _) => {
            let _ = write!(text, "\nsince {tag}");
        }
        (None, true) => text.push_str("\nno prior release tag; recent-history window"),
        (None, false) => text.push_str("\nno prior release tag"),
    }
    for row in &entries.rows {
        let _ = write!(text, "\n{row}");
    }
    match &entries.omission {
        Some(omission) => {
            let _ = write!(text, "\n{omission}");
        }
        None => {
            let _ = write!(
                text,
                "\ncomplete document: {}",
                notes_export(release_commit)
            );
        }
    }
    NotesPreview {
        version: notes.version,
        identity: notes.identity.clone(),
        release_version: notes.release_version.clone(),
        release_commit: notes.release_commit.clone(),
        previous_tag: notes.previous_tag.clone(),
        bounded_recent_window: notes.bounded_recent_window,
        document_bytes: notes.document.len(),
        entries,
        text,
    }
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[cfg(test)]
mod tests;
