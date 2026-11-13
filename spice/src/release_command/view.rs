//! What a person reads from `release notes` and a `release publish` preview.
//!
//! Both documents are complete only as files. The terminal gets bounded
//! metadata and a relevance-ordered page; the complete note Markdown is an
//! explicit `--output` export, and every publish preview retains the complete
//! plan, named by the key `--apply` must re-present, before it prints a line.

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use wire::output::{Admission, Bounded, Budget, ClippedText, Export, INTERACTIVE, Request};
use wire::release::ReleasePlan;
use wire::{Digest, Outcome, Refusal, ReleaseNotes, SpiceCommand};

/// The file a complete note export names by default.
const NOTES_FILE: &str = "release-notes.md";
/// Where publish previews retain their complete plans under the Git-common
/// state root.
const PLAN_DIRECTORY: &str = "output/releases";
/// Curated note lines a publish preview shows before naming the plan file.
const NOTE_LINES: usize = 8;
/// Bytes of one curated note line a preview carries.
const NOTE_LINE_BYTES: usize = 160;

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

/// One publish preview: every authorizing plan field, a bounded excerpt of
/// its curated notes, and the file retaining the complete plan.
#[derive(Debug, Serialize)]
pub(crate) struct PlanPreview {
    /// The plan's encoded fields except `notes_document`, which only the
    /// retained plan file carries whole.
    #[serde(flatten)]
    plan: serde_json::Map<String, serde_json::Value>,
    pub(crate) notes_preview: NotesExcerpt,
    /// The retained complete plan, named by its key.
    pub(crate) export: Export,
    #[serde(skip)]
    text: String,
}

impl PlanPreview {
    /// Project one retained plan for authorization.
    pub(super) fn new(plan: &ReleasePlan, export: Export, command: &[String]) -> Self {
        let notes_preview = NotesExcerpt::new(&plan.notes_document);
        let text = plan_text(plan, &notes_preview, &export, command);
        let mut fields = match serde_json::to_value(plan) {
            Ok(serde_json::Value::Object(fields)) => fields,
            _ => unreachable!("a release plan encodes as an object"),
        };
        fields.remove("notes_document");
        Self {
            plan: fields,
            notes_preview,
            export,
            text,
        }
    }

    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

/// The first curated note lines and what the plan file holds beyond them.
#[derive(Debug, Serialize)]
pub(crate) struct NotesExcerpt {
    /// Byte length of the complete curated document.
    pub(crate) bytes: usize,
    /// Lines in the complete curated document.
    pub(crate) lines: usize,
    /// The first lines, each clipped to a bounded width.
    pub(crate) shown: Vec<ClippedText>,
    /// Lines only the retained plan file carries.
    pub(crate) withheld_lines: usize,
}

impl NotesExcerpt {
    fn new(document: &str) -> Self {
        let lines = document.lines().count();
        let shown = document
            .lines()
            .take(NOTE_LINES)
            .map(|line| wire::output::clip(line, NOTE_LINE_BYTES))
            .collect::<Vec<_>>();
        Self {
            bytes: document.len(),
            lines,
            withheld_lines: lines - shown.len(),
            shown,
        }
    }
}

/// The file one plan is retained in.
pub(super) fn plan_path(common_git: &Path, key: &Digest) -> PathBuf {
    wire::state_root(common_git)
        .join(PLAN_DIRECTORY)
        .join(format!("release-plan-{key}.json"))
}

/// Retain the complete plan, then describe it for authorization.
pub(super) fn plan_preview(
    plan: ReleasePlan,
    common_git: &Path,
    command: &[String],
) -> Outcome<PlanPreview> {
    let path = plan_path(common_git, &plan.key);
    if let Err(error) = path.parent().map_or(Ok(()), std::fs::create_dir_all) {
        return super::refusal(
            "release::plan-unretained",
            &format!("{}: {error}", path.display()),
        );
    }
    let bytes = match wire::encode(&plan) {
        Ok(bytes) => bytes,
        Err(error) => return super::refusal("release::plan-unencodable", &error),
    };
    match spice::export::export(&path, &bytes) {
        Ok(export) => Outcome::Reply(wire::Reply {
            position: wire::ReplyPosition::Direct {},
            body: PlanPreview::new(&plan, export, command),
        }),
        Err(refusal) => Outcome::Refusal(refusal),
    }
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

fn present(observed: Option<&String>) -> String {
    observed.map_or_else(
        || "absent".to_owned(),
        |object| format!("UNEXPECTED {object}"),
    )
}

/// Every plan field authorization rests on; only the curated note bytes are
/// bounded, and the line that bounds them names the file holding them whole.
fn plan_text(
    plan: &ReleasePlan,
    notes: &NotesExcerpt,
    export: &Export,
    command: &[String],
) -> String {
    let observations = &plan.observations;
    let mut text = format!(
        "release publish preview: {} (nothing published)\n\
         key                {}\n\
         repository         {}\n\
         source commit      {}\n\
         source archive     {}\n\
         release commit     {}\n\
         manifest           {}\n\
         package            {} object {}\n\
         installed artifact {}",
        plan.requested_version,
        plan.key,
        plan.repository,
        plan.source_commit,
        plan.source_archive,
        plan.release_commit,
        plan.manifest,
        plan.package,
        plan.package_object,
        plan.installed_artifact,
    );
    for target in &plan.targets {
        let _ = write!(
            text,
            "\ntarget             {} artifact {} receipt {}",
            target.target, target.artifact, target.receipt
        );
    }
    let _ = write!(text, "\noperations ({}):", plan.operations.len());
    for (number, operation) in plan.operations.iter().enumerate() {
        let _ = write!(text, "\n  {}. {operation}", number + 1);
    }
    let _ = write!(
        text,
        "\nobservations:\n  head {}; tracked worktree {}; {} unrecorded local commits\n  \
         source version {}; release version {}\n  credentials {}\n  \
         package ref {}; tag ref {}; announcement ref {}",
        observations.head,
        if observations.clean { "clean" } else { "DIRTY" },
        observations.unrecorded_local_commits,
        observations.source_version,
        observations.release_version,
        if observations.credentials_valid {
            "valid"
        } else {
            "INVALID"
        },
        present(observations.package_ref.as_ref()),
        present(observations.tag_ref.as_ref()),
        present(observations.announcement_ref.as_ref()),
    );
    let _ = write!(
        text,
        "\nnotes {} ({} bytes, {} lines):",
        plan.notes, notes.bytes, notes.lines
    );
    for line in &notes.shown {
        let _ = write!(text, "\n  | {line}");
    }
    if notes.withheld_lines > 0 {
        let _ = write!(
            text,
            "\n  {} more lines in the plan file",
            notes.withheld_lines
        );
    }
    let _ = write!(
        text,
        "\nplan: {} ({} bytes, digest {})\nnext: spice {} --apply={}",
        export.path,
        export.bytes,
        export.digest,
        command.join(" "),
        plan.key
    );
    text
}

#[cfg(test)]
mod tests;
