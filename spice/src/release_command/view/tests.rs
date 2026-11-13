use wire::manifest::ProductVersion;
use wire::output::Exhausted;
use wire::release::{Operation, ReleaseObservations};
use wire::{Domain, ReleaseNotesEntry, body::PublishedTarget};

use super::*;

const LANDINGS: usize = 3_000;
const OVERSIZED: usize = 200_000;

fn notes(entries: Vec<ReleaseNotesEntry>) -> ReleaseNotes {
    let document = entries
        .iter()
        .map(|entry| format!("- {}\n", entry.subject))
        .collect::<String>();
    ReleaseNotes {
        version: 1,
        identity: Digest::compute(Domain::DistReleaseNotes, document.as_bytes()),
        release_version: "0.40.0".to_owned(),
        release_commit: "a".repeat(40),
        previous_tag: Some("v0.39.0".to_owned()),
        bounded_recent_window: false,
        entries,
        document,
    }
}

fn entry(index: usize, landings: usize) -> ReleaseNotesEntry {
    ReleaseNotesEntry {
        topic: "/cli".parse().expect("fixture topic"),
        subject: format!("Change {index}"),
        task: Some(format!("CLI-{index}")),
        references: (0..landings)
            .map(|landing| format!("{index}.{landing}"))
            .collect(),
    }
}

fn default_budget() -> Budget {
    INTERACTIVE
        .budget(Request::default())
        .expect("default limits")
}

#[test]
fn thousands_of_landings_preview_the_most_landed_first_with_omission_and_export() {
    let mut entries = (0..LANDINGS)
        .map(|index| entry(index, 1))
        .collect::<Vec<_>>();
    entries[2_500] = entry(2_500, 4);
    entries[7] = entry(7, 2);
    entries[9] = entry(9, 2);
    entries[11].subject = "x".repeat(OVERSIZED);
    let notes = notes(entries);
    let preview = notes_preview(&notes, default_budget(), Some("v0.40.0"));

    let order = preview
        .entries
        .rows
        .iter()
        .map(|row| row.task.as_deref().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(order[..4], ["CLI-2500", "CLI-7", "CLI-9", "CLI-0"]);
    let oversized = preview
        .entries
        .rows
        .iter()
        .find(|row| row.task.as_deref() == Some("CLI-11"))
        .expect("the oversized subject is still listed, clipped");
    let limit = default_budget().limits().text;
    assert_eq!(oversized.subject.text.len(), limit);
    assert_eq!(
        oversized.subject.withheld_bytes.map(usize::from),
        Some(OVERSIZED - limit)
    );

    let omission = preview.entries.omission.as_ref().expect("entries withheld");
    assert_eq!(omission.population, LANDINGS);
    assert_eq!(omission.shown, preview.entries.rows.len());
    assert_eq!(omission.withheld, LANDINGS - omission.shown);
    assert_eq!(omission.exhausted, Exhausted::Rows);
    assert_eq!(
        omission.continuation.to_string(),
        "spice release notes --release-commit v0.40.0 --output release-notes.md"
    );
    assert_eq!(preview.document_bytes, notes.document.len());
    // The page is bounded however long the history or a subject runs.
    assert!(
        preview.text.len() < default_budget().limits().bytes * 2,
        "{} preview bytes",
        preview.text.len()
    );
    assert!(
        preview
            .text
            .contains(&format!("identity {}", notes.identity))
    );
    assert!(preview.text.ends_with(&omission.to_string()));

    // The JSON form carries the same identity and truncation facts.
    let json = serde_json::to_value(&preview).expect("preview encodes");
    assert_eq!(json["identity"], serde_json::json!(notes.identity));
    assert_eq!(json["entries"]["omission"]["withheld"], omission.withheld);
    let rows = json["entries"]["rows"].as_array().expect("rows");
    let clipped = rows
        .iter()
        .find(|row| row["task"] == "CLI-11")
        .expect("clipped row");
    assert_eq!(clipped["subject"]["withheld_bytes"], OVERSIZED - limit);
    assert!(json.get("document").is_none(), "the document only exports");
}

#[test]
fn output_writes_the_complete_document_and_reports_its_identity() {
    let notes = notes((0..LANDINGS).map(|index| entry(index, 1)).collect());
    let root = std::env::temp_dir().join(format!("spice-release-notes-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("fixture directory");
    let path = root.join("notes.md");

    let NotesView::Export(export) =
        notes_view(&notes, Some(path.to_str().expect("utf-8 path")), None).expect("export")
    else {
        panic!("--output exports");
    };
    assert_eq!(
        std::fs::read_to_string(&path).expect("exported"),
        notes.document
    );
    assert_eq!(export.identity, notes.identity);
    assert_eq!(export.export.bytes, notes.document.len() as u64);
    assert_eq!(export.entries, LANDINGS);
    assert!(export.text.contains(&notes.identity.to_string()));
    assert!(export.text.contains(&export.export.path));
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_complete_preview_names_the_export_instead_of_an_omission() {
    let notes = notes((0..3).map(|index| entry(index, 1)).collect());
    let preview = notes_preview(&notes, default_budget(), None);
    assert!(preview.entries.omission.is_none());
    assert!(
        preview
            .text
            .ends_with("complete document: spice release notes --output release-notes.md")
    );
}

fn plan(notes_document: String) -> ReleasePlan {
    let digest = |bytes: &[u8]| Digest::compute(Domain::DistReleasePlan, bytes);
    let version: ProductVersion = "0.40.0".parse().expect("version");
    ReleasePlan {
        key: digest(b"key"),
        repository: digest(b"repository"),
        requested_version: version.clone(),
        source_commit: "b".repeat(40),
        source_archive: digest(b"source archive"),
        release_commit: "c".repeat(40),
        manifest: digest(b"manifest"),
        notes: digest(notes_document.as_bytes()),
        notes_document,
        package: digest(b"package"),
        package_object: "d".repeat(40),
        installed_artifact: digest(b"installed"),
        targets: vec![PublishedTarget {
            target: "aarch64-apple-darwin".to_owned(),
            artifact: digest(b"artifact"),
            receipt: digest(b"receipt"),
        }],
        operations: vec![Operation::UploadPackage, Operation::CreateImmutableTag],
        observations: ReleaseObservations {
            head: "c".repeat(40),
            clean: false,
            unrecorded_local_commits: 2,
            source_version: version.clone(),
            release_version: version,
            credentials_valid: true,
            package_ref: None,
            tag_ref: Some("e".repeat(40)),
            announcement_ref: None,
        },
    }
}

#[test]
fn a_publish_preview_names_every_authorizing_field_and_bounds_only_the_notes() {
    let document = (0..LANDINGS)
        .map(|line| format!("- curated line {line}\n"))
        .collect::<String>();
    let plan = plan(document);
    let export = Export {
        path: "/common/.spice/output/releases/plan.json".to_owned(),
        bytes: 1,
        digest: Digest::compute(Domain::CliExport, b"plan"),
    };
    let words = ["release", "publish", "--version", "0.40.0"].map(str::to_owned);
    let preview = PlanPreview::new(&plan, export.clone(), &words);
    let text = preview.text().expect("a publish preview has text");

    for field in [
        plan.key.to_string(),
        plan.repository.to_string(),
        plan.source_commit.clone(),
        plan.source_archive.to_string(),
        plan.release_commit.clone(),
        plan.manifest.to_string(),
        plan.package.to_string(),
        plan.package_object.clone(),
        plan.installed_artifact.to_string(),
        plan.notes.to_string(),
        plan.targets[0].artifact.to_string(),
        plan.targets[0].receipt.to_string(),
        "1. upload-package".to_owned(),
        "2. create-immutable-tag".to_owned(),
        "tracked worktree DIRTY; 2 unrecorded local commits".to_owned(),
        "credentials valid".to_owned(),
        format!("tag ref UNEXPECTED {}", "e".repeat(40)),
        export.path.clone(),
    ] {
        assert!(text.contains(&field), "preview omits {field}:\n{text}");
    }
    assert!(text.contains(&format!(
        "{} more lines in the plan file",
        LANDINGS - NOTE_LINES
    )));
    assert!(!text.contains(&format!("curated line {NOTE_LINES}\n")));
    assert!(text.ends_with(&format!(
        "next: spice release publish --version 0.40.0 --apply={}",
        plan.key
    )));
    assert!(
        text.len() < plan.notes_document.len() / 10,
        "{} bytes",
        text.len()
    );

    let json = serde_json::to_value(&preview).expect("preview encodes");
    let encoded = json.to_string();
    assert!(
        encoded.len() < plan.notes_document.len() / 10,
        "{} JSON bytes",
        encoded.len()
    );
    assert!(json.get("notes_document").is_none());
    assert_eq!(json["key"], plan.key.to_string());
    assert_eq!(json["notes"], plan.notes.to_string());
    assert_eq!(json["operations"][1], "create-immutable-tag");
    assert_eq!(json["observations"]["unrecorded_local_commits"], 2);
    assert_eq!(json["export"]["path"], export.path);
    let excerpt = &json["notes_preview"];
    assert_eq!(excerpt["lines"], LANDINGS);
    assert_eq!(excerpt["bytes"], plan.notes_document.len());
    assert_eq!(excerpt["withheld_lines"], LANDINGS - NOTE_LINES);
    assert_eq!(excerpt["shown"].as_array().map(Vec::len), Some(NOTE_LINES));
}

#[test]
fn a_publish_preview_clips_an_oversized_note_line_in_text_and_json() {
    let plan = plan(format!("{}\n", "x".repeat(OVERSIZED)));
    let export = Export {
        path: "plan.json".to_owned(),
        bytes: 1,
        digest: Digest::compute(Domain::CliExport, b"plan"),
    };
    let preview = PlanPreview::new(&plan, export, &["release".to_owned()]);
    let json = serde_json::to_value(&preview).expect("preview encodes");
    let shown = &json["notes_preview"]["shown"][0];
    assert_eq!(shown["text"].as_str().map(str::len), Some(NOTE_LINE_BYTES));
    assert_eq!(shown["withheld_bytes"], OVERSIZED - NOTE_LINE_BYTES);
    assert_eq!(json["notes_preview"]["withheld_lines"], 0);
    assert!(preview.text().expect("text").len() < OVERSIZED / 10);
}
