use wire::output::Exhausted;
use wire::{Domain, ReleaseNotesEntry};

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
