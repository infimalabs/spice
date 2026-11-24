//! Release-note folding from first-parent Git history and typed task authority.

use std::collections::{BTreeMap, BTreeSet};

use wire::manifest::RELEASE_NOTES_VERSION;
use wire::{Digest, Domain, ReleaseNotes, ReleaseNotesEntry};

/// Marker separating curator-owned prose from the regenerated inventory.
const INVENTORY_OPEN: &str = "<details>";
/// Placeholder that makes an untouched draft unpublishable.
const HIGHLIGHTS_PLACEHOLDER: &str = concat!(
    "_Replace this line with a short, curated set of highlights folded from ",
    "the changes below._",
);

/// Review standing carried by one durable task landing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewStanding {
    /// Review accepted the task.
    Clean,
    /// Review required a correction.
    Finding,
}

/// One typed task landing projected from durable journal authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Landing {
    /// Journal sequence establishing this landing.
    pub sequence: u64,
    /// Integrated Git commit named by the task boundary.
    pub commit: String,
    /// Stable task identity.
    pub task: String,
    /// Current topic path of the task.
    pub topic: String,
    /// Current typed task title.
    pub title: String,
    /// Whether this task boundary contributed the integrated commit.
    pub contributes: bool,
    /// Review standing when this is a review landing.
    pub verdict: Option<ReviewStanding>,
}

/// One correction task assigned by a parent task review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Correction {
    /// Parent task whose finding required correction.
    pub parent: String,
    /// Task created to make the correction.
    pub correction: String,
}

/// One first-parent Git commit selected for the release inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCommit {
    /// Full object identity.
    pub commit: String,
    /// Committer timestamp in whole Unix seconds.
    pub committed_at: i64,
    /// Commit subject exactly as Git stores it.
    pub subject: String,
}

/// Complete observations needed to fold one release-note document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotesInput {
    /// Version named by the release page.
    pub release_version: String,
    /// Fully resolved release commit.
    pub release_commit: String,
    /// Highest-versioned reachable prior release tag.
    pub previous_tag: Option<String>,
    /// Ordered first-parent commits in the exact selected release window.
    pub window: Vec<ReleaseCommit>,
    /// Typed landings reachable in the selected first-parent ancestry.
    pub landings: Vec<Landing>,
    /// Typed correction relations among those tasks.
    pub corrections: Vec<Correction>,
}

/// Fold a release-note draft from Git history and typed task authority.
///
/// # Errors
///
/// Refuses ambiguous landing or correction authority and correction cycles.
pub fn release_notes(input: NotesInput) -> Result<ReleaseNotes, NotesError> {
    let entries = fold_entries(&input)?;
    let document = render(&input, &entries);
    Ok(ReleaseNotes {
        version: RELEASE_NOTES_VERSION,
        identity: Digest::compute(Domain::DistReleaseNotes, document.as_bytes()),
        release_version: input.release_version,
        release_commit: input.release_commit,
        previous_tag: input.previous_tag.clone(),
        bounded_recent_window: input.previous_tag.is_none(),
        entries,
        document,
    })
}

/// Keep curator-owned prose and regenerate the inventory from `canonical`.
///
/// # Errors
///
/// Refuses the untouched generated draft, any surviving placeholder, a
/// curator region containing only generated banner/headings, or a canonical
/// document with no inventory boundary.
pub fn curated_release_notes(
    candidate: &str,
    canonical: &ReleaseNotes,
) -> Result<ReleaseNotes, NotesError> {
    if candidate.as_bytes() == canonical.document.as_bytes() {
        return Err(NotesError::UntouchedDraft);
    }
    if candidate.contains(HIGHLIGHTS_PLACEHOLDER) {
        return Err(NotesError::HighlightsPlaceholder);
    }
    let curated = curated_region(candidate);
    if !carries_highlights(curated) {
        return Err(NotesError::HighlightsAbsent);
    }
    let inventory = canonical
        .document
        .find(INVENTORY_OPEN)
        .ok_or(NotesError::InventoryMissing)?;
    let document = format!(
        "{}\n\n{}",
        curated.trim_end(),
        &canonical.document[inventory..]
    );
    let mut notes = canonical.clone();
    notes.identity = Digest::compute(Domain::DistReleaseNotes, document.as_bytes());
    notes.document = document;
    Ok(notes)
}

fn fold_entries(input: &NotesInput) -> Result<Vec<ReleaseNotesEntry>, NotesError> {
    let parents = correction_parents(&input.corrections)?;
    let roots = input
        .landings
        .iter()
        .map(|landing| {
            correction_root(&landing.task, &parents)
                .map(|root| (landing.task.clone(), root.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut contributors: BTreeMap<&str, &Landing> = BTreeMap::new();
    for landing in input.landings.iter().filter(|landing| landing.contributes) {
        if contributors
            .insert(landing.commit.as_str(), landing)
            .is_some()
        {
            return Err(NotesError::AmbiguousLanding {
                commit: landing.commit.clone(),
            });
        }
    }

    let mut metadata: BTreeMap<&str, &Landing> = BTreeMap::new();
    let mut standing: BTreeMap<&str, (u64, ReviewStanding)> = BTreeMap::new();
    for landing in &input.landings {
        let root = roots
            .get(&landing.task)
            .map(String::as_str)
            .unwrap_or(landing.task.as_str());
        if root == landing.task {
            let current = metadata.entry(root).or_insert(landing);
            if landing.sequence > current.sequence {
                *current = landing;
            }
            if let Some(verdict) = landing.verdict {
                let current = standing.entry(root).or_insert((landing.sequence, verdict));
                if landing.sequence > current.0 {
                    *current = (landing.sequence, verdict);
                }
            }
        }
    }

    let mut entries = Vec::<ReleaseNotesEntry>::new();
    let mut positions = BTreeMap::<String, usize>::new();
    for commit in &input.window {
        let Some(landing) = contributors.get(commit.commit.as_str()).copied() else {
            entries.push(ReleaseNotesEntry {
                topic: "/general".to_owned(),
                subject: edited_release_highlight(&commit.subject),
                task: None,
                references: vec![commit.commit.clone()],
            });
            continue;
        };
        let root = roots
            .get(&landing.task)
            .map(String::as_str)
            .unwrap_or(landing.task.as_str());
        if standing
            .get(root)
            .is_some_and(|(_, verdict)| *verdict == ReviewStanding::Finding)
        {
            continue;
        }
        let authority =
            metadata
                .get(root)
                .copied()
                .ok_or_else(|| NotesError::MissingTaskAuthority {
                    task: root.to_owned(),
                })?;
        if let Some(position) = positions.get(root).copied() {
            entries[position].references.push(commit.commit.clone());
            continue;
        }
        positions.insert(root.to_owned(), entries.len());
        entries.push(ReleaseNotesEntry {
            topic: authority.topic.clone(),
            subject: edited_release_highlight(&authority.title),
            task: Some(root.to_owned()),
            references: vec![commit.commit.clone()],
        });
    }
    Ok(entries)
}

fn correction_parents(corrections: &[Correction]) -> Result<BTreeMap<String, String>, NotesError> {
    let mut parents = BTreeMap::new();
    for correction in corrections {
        if let Some(previous) =
            parents.insert(correction.correction.clone(), correction.parent.clone())
            && previous != correction.parent
        {
            return Err(NotesError::AmbiguousCorrection {
                task: correction.correction.clone(),
            });
        }
    }
    Ok(parents)
}

fn correction_root<'a>(
    task: &'a str,
    parents: &'a BTreeMap<String, String>,
) -> Result<&'a str, NotesError> {
    let mut current = task;
    let mut seen = BTreeSet::new();
    while let Some(parent) = parents.get(current) {
        if !seen.insert(current) {
            return Err(NotesError::CorrectionCycle {
                task: task.to_owned(),
            });
        }
        current = parent;
    }
    Ok(current)
}

fn render(input: &NotesInput, entries: &[ReleaseNotesEntry]) -> String {
    let commits = input
        .window
        .iter()
        .map(|commit| (commit.commit.as_str(), commit))
        .collect::<BTreeMap<_, _>>();
    let mut groups: BTreeMap<&str, Vec<TopicHighlight<'_>>> = BTreeMap::new();
    for entry in entries {
        let group = groups.entry(&entry.topic).or_default();
        let references = entry.references.iter().map(|reference| {
            commits
                .get(reference.as_str())
                .copied()
                .expect("release entry references its input commit")
        });
        if let Some(highlight) = group
            .iter_mut()
            .find(|highlight| highlight.text == entry.subject)
        {
            highlight.references.extend(references);
        } else {
            group.push(TopicHighlight {
                text: &entry.subject,
                references: references.collect(),
            });
        }
    }
    for highlights in groups.values_mut() {
        for highlight in highlights.iter_mut() {
            highlight.references.sort_by(|left, right| {
                left.committed_at
                    .cmp(&right.committed_at)
                    .then_with(|| left.commit.cmp(&right.commit))
            });
        }
        highlights.sort_by(|left, right| {
            let left_commit = left
                .references
                .first()
                .expect("release highlight has a contributing commit");
            let right_commit = right
                .references
                .first()
                .expect("release highlight has a contributing commit");
            left_commit
                .committed_at
                .cmp(&right_commit.committed_at)
                .then_with(|| left.text.cmp(right.text))
                .then_with(|| left_commit.commit.cmp(&right_commit.commit))
        });
    }

    let mut lines = vec![
        "> [!IMPORTANT]".to_owned(),
        "> **Draft release notes — curate Highlights before publishing.** Replace".to_owned(),
        "> the placeholder under _Highlights_ with a short summary, then delete this".to_owned(),
        "> banner. Everything from the collapsed _Commit-level changes_ section down".to_owned(),
        "> is regenerated at publication against the real release commit.".to_owned(),
        String::new(),
        "## Highlights".to_owned(),
        String::new(),
        HIGHLIGHTS_PLACEHOLDER.to_owned(),
        String::new(),
        INVENTORY_OPEN.to_owned(),
        "<summary>Commit-level changes</summary>".to_owned(),
        String::new(),
        "## Changes by topic".to_owned(),
        String::new(),
    ];
    if groups.is_empty() {
        lines.extend(["- No selected commits found.".to_owned(), String::new()]);
    } else {
        for (topic, highlights) in groups {
            lines.extend([format!("### {}", topic_heading(topic)), String::new()]);
            for highlight in highlights {
                let references = highlight
                    .references
                    .iter()
                    .map(|reference| short_commit(&reference.commit))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("- {} ({references})", highlight.text));
            }
            lines.push(String::new());
        }
    }
    let release_short = short_commit(&input.release_commit);
    lines.extend([
        "</details>".to_owned(),
        String::new(),
        "## Package Notes".to_owned(),
        String::new(),
        format!("- Spice release: `{}`", input.release_version),
        format!("- Release commit: `{release_short}`"),
    ]);
    if let Some(previous) = &input.previous_tag {
        lines.push(format!(
            "- Commit range: `refs/tags/{previous}..{release_short}`"
        ));
    } else {
        lines.push(format!(
            "- Commit range: bounded latest first-parent commits ending at `{release_short}`"
        ));
    }
    lines.push(
        "- Commit source: first-parent Git bounds with durable task landing authority".to_owned(),
    );
    lines.push(String::new());
    lines.join("\n")
}

struct TopicHighlight<'a> {
    text: &'a str,
    references: Vec<&'a ReleaseCommit>,
}

fn curated_region(candidate: &str) -> &str {
    candidate
        .split_once(INVENTORY_OPEN)
        .map_or(candidate, |(curated, _)| curated)
}

fn carries_highlights(curated: &str) -> bool {
    curated.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with(['>', '#'])
    })
}

fn edited_release_highlight(subject: &str) -> String {
    let raw = subject.trim();
    if raw.is_empty() {
        return "Updated the release.".to_owned();
    }
    for (prefix, replacement) in [
        ("fix ", "Fixed "),
        ("prefer ", "Improved "),
        ("add ", "Added "),
        ("expose ", "Added "),
        ("remove ", "Removed "),
        ("update ", "Updated "),
        ("track ", "Tracked "),
        ("document ", "Documented "),
        ("restore ", "Restored "),
        ("clean ", "Cleaned "),
        ("wire ", "Wired "),
        ("make ", "Made "),
    ] {
        if raw
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            return punctuate(format!("{replacement}{}", &raw[prefix.len()..]));
        }
    }
    punctuate(capitalize_first(raw))
}

fn punctuate(mut text: String) -> String {
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

fn topic_heading(topic: &str) -> String {
    let words = topic
        .split(['/', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| match word {
            "cli" => "CLI".to_owned(),
            "ui" => "UI".to_owned(),
            _ => capitalize_first(word),
        })
        .collect::<Vec<_>>();
    if words.is_empty() {
        "General".to_owned()
    } else {
        words.join(" ")
    }
}

fn capitalize_first(text: &str) -> String {
    let mut characters = text.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().chain(characters).collect()
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// Why typed landing authority could not become publishable release notes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NotesError {
    /// More than one task claims the same integrated commit.
    #[error("integrated commit {commit} has ambiguous typed release-note landings")]
    AmbiguousLanding { commit: String },
    /// One correction task names multiple parents.
    #[error("correction task {task} has ambiguous parent authority")]
    AmbiguousCorrection { task: String },
    /// Correction relations contain a cycle.
    #[error("correction task {task} belongs to a correction cycle")]
    CorrectionCycle { task: String },
    /// A folded root task has no typed metadata.
    #[error("task {task} has no typed release-note metadata")]
    MissingTaskAuthority { task: String },
    /// The operator supplied the generated draft unchanged.
    #[error("release notes still exactly match the generated draft")]
    UntouchedDraft,
    /// The generated curation placeholder remains anywhere in the candidate.
    #[error("release notes still carry the Highlights placeholder")]
    HighlightsPlaceholder,
    /// The curator-owned region contains no actual prose or list entry.
    #[error("the curated release-note region contains only banner text and headings")]
    HighlightsAbsent,
    /// The canonical renderer failed to provide its generated-tail boundary.
    #[error("the canonical release-note document has no task inventory boundary")]
    InventoryMissing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_landings_and_corrections_fold_under_typed_root_authority() {
        let input = fixture(
            vec!["1111111111", "2222222222", "3333333333"],
            vec![
                landing(1, "1111111111", "root", "/spice/app", "Show presence"),
                landing(2, "2222222222", "root", "/spice/app", "Show presence"),
                landing(3, "3333333333", "fix", "/spice/fix", "Repair presence"),
            ],
            vec![Correction {
                parent: "root".to_owned(),
                correction: "fix".to_owned(),
            }],
        );

        let notes = release_notes(input).expect("typed authority folds");

        assert_eq!(notes.entries.len(), 1);
        assert_eq!(notes.entries[0].task.as_deref(), Some("root"));
        assert_eq!(notes.entries[0].topic, "/spice/app");
        assert_eq!(notes.entries[0].subject, "Show presence.");
        assert_eq!(
            notes.entries[0].references,
            ["1111111111", "2222222222", "3333333333"]
        );
    }

    #[test]
    fn latest_parent_review_controls_correction_visibility() {
        let mut landings = vec![
            landing(1, "1111111111", "root", "/spice/app", "Show presence"),
            landing(3, "3333333333", "fix", "/spice/fix", "Repair presence"),
        ];
        landings.push(review(2, "root", ReviewStanding::Finding));
        let corrections = vec![Correction {
            parent: "root".to_owned(),
            correction: "fix".to_owned(),
        }];
        let hidden = release_notes(fixture(
            vec!["1111111111", "3333333333"],
            landings.clone(),
            corrections.clone(),
        ))
        .expect("finding folds");
        assert!(hidden.entries.is_empty());

        landings.push(review(4, "root", ReviewStanding::Clean));
        let visible = release_notes(fixture(
            vec!["1111111111", "3333333333"],
            landings,
            corrections,
        ))
        .expect("clean review folds");
        assert_eq!(visible.entries.len(), 1);
    }

    #[test]
    fn latest_root_metadata_wins_independent_of_landing_order() {
        for landings in [
            vec![
                landing(1, "1111111111", "root", "/spice/old", "Old title"),
                landing(2, "2222222222", "root", "/spice/new", "New title"),
            ],
            vec![
                landing(2, "2222222222", "root", "/spice/new", "New title"),
                landing(1, "1111111111", "root", "/spice/old", "Old title"),
            ],
        ] {
            let notes = release_notes(fixture(
                vec!["1111111111", "2222222222"],
                landings,
                Vec::new(),
            ))
            .expect("latest typed metadata folds");

            assert_eq!(notes.entries.len(), 1);
            assert_eq!(notes.entries[0].topic, "/spice/new");
            assert_eq!(notes.entries[0].subject, "New title.");
        }
    }

    #[test]
    fn commit_inventory_orders_topics_then_mentions_by_time() {
        let mut input = fixture(
            vec![
                "1111111111",
                "2222222222",
                "3333333333",
                "4444444444",
                "5555555555",
            ],
            vec![
                landing(1, "1111111111", "zed", "/app/z", "Zed change"),
                landing(2, "2222222222", "later", "/app/a", "Later change"),
                landing(3, "3333333333", "deep", "/app/a/deep", "Deep change"),
                landing(4, "4444444444", "earlier", "/app/a", "Earlier change"),
                landing(5, "5555555555", "alpha", "/app/a", "Alpha change"),
            ],
            Vec::new(),
        );
        for (commit, committed_at) in input.window.iter_mut().zip([5, 30, 20, 10, 10]) {
            commit.committed_at = committed_at;
        }

        let notes = release_notes(input).expect("ordered history folds");
        let inventory = notes
            .document
            .split_once("<summary>Commit-level changes</summary>")
            .expect("inventory opens")
            .1
            .split_once("</details>")
            .expect("inventory closes")
            .0;
        let headings_and_mentions = inventory
            .lines()
            .filter(|line| line.starts_with("### ") || line.starts_with("- "))
            .collect::<Vec<_>>();

        assert_eq!(
            headings_and_mentions,
            [
                "### App A",
                "- Alpha change. (5555555)",
                "- Earlier change. (4444444)",
                "- Later change. (2222222)",
                "### App A Deep",
                "- Deep change. (3333333)",
                "### App Z",
                "- Zed change. (1111111)",
            ]
        );
    }

    #[test]
    fn repeated_identical_correction_authority_is_idempotent() {
        let correction = Correction {
            parent: "root".to_owned(),
            correction: "fix".to_owned(),
        };
        let notes = release_notes(fixture(
            vec!["1111111111", "2222222222"],
            vec![
                landing(1, "1111111111", "root", "/spice/app", "Show presence"),
                landing(2, "2222222222", "fix", "/spice/fix", "Repair presence"),
            ],
            vec![correction.clone(), correction],
        ))
        .expect("identical correction authority folds once");

        assert_eq!(notes.entries.len(), 1);
        assert_eq!(notes.entries[0].task.as_deref(), Some("root"));
        assert_eq!(notes.entries[0].references, ["1111111111", "2222222222"]);
    }

    #[test]
    fn a_selected_commit_without_task_authority_is_formatted_from_git() {
        let notes = release_notes(fixture(
            vec!["1111111111", "2222222222"],
            vec![landing(
                1,
                "1111111111",
                "root",
                "/spice/app",
                "Show presence",
            )],
            Vec::new(),
        ))
        .expect("untyped commit falls back to Git metadata");

        assert_eq!(notes.entries.len(), 2);
        assert_eq!(notes.entries[1].topic, "/general");
        assert_eq!(notes.entries[1].subject, "Updated 2222222222.");
        assert_eq!(notes.entries[1].task, None);
        assert_eq!(notes.entries[1].references, ["2222222222"]);
    }

    #[test]
    fn no_tag_is_a_bounded_and_explicit_window() {
        let notes = release_notes(fixture(Vec::new(), Vec::new(), Vec::new()))
            .expect("empty history folds");

        assert!(notes.bounded_recent_window);
        assert!(notes.previous_tag.is_none());
    }

    #[test]
    fn curation_is_required_and_only_the_canonical_tail_survives() {
        let canonical = release_notes(NotesInput {
            previous_tag: Some("v1.2.3".to_owned()),
            ..fixture(
                vec!["1234567890"],
                vec![landing(
                    1,
                    "1234567890",
                    "note",
                    "/spice/dist",
                    "Derive release notes from durable landings",
                )],
                Vec::new(),
            )
        })
        .expect("history folds");

        assert_eq!(
            curated_release_notes(&canonical.document, &canonical),
            Err(NotesError::UntouchedDraft)
        );
        assert_eq!(
            curated_release_notes(
                concat!(
                    "## Highlights\n\n_Replace this line with a short, curated set of ",
                    "highlights folded from the changes below._",
                ),
                &canonical
            ),
            Err(NotesError::HighlightsPlaceholder)
        );
        assert_eq!(
            curated_release_notes("> banner\n\n## Highlights\n", &canonical),
            Err(NotesError::HighlightsAbsent)
        );

        let candidate = concat!(
            "## Highlights\r\n\r\n- Release notes now follow durable task authority.",
            "\r\n\r\n<details>\r\nold inventory",
        );
        let curated = curated_release_notes(candidate, &canonical).expect("highlights curate");
        assert!(curated.document.starts_with(
            "## Highlights\r\n\r\n- Release notes now follow durable task authority."
        ));
        assert!(
            curated
                .document
                .contains("Derive release notes from durable landings")
        );
        assert!(!curated.document.contains("old inventory"));
        assert_ne!(curated.identity, canonical.identity);
    }

    fn fixture(
        window: Vec<&str>,
        landings: Vec<Landing>,
        corrections: Vec<Correction>,
    ) -> NotesInput {
        NotesInput {
            release_version: "1.2.4".to_owned(),
            release_commit: "abcdef0123456789".to_owned(),
            previous_tag: None,
            window: window
                .into_iter()
                .enumerate()
                .map(|(index, commit)| ReleaseCommit {
                    commit: commit.to_owned(),
                    committed_at: i64::try_from(index).expect("fixture index fits in i64"),
                    subject: format!("Update {commit}"),
                })
                .collect(),
            landings,
            corrections,
        }
    }

    fn landing(sequence: u64, commit: &str, task: &str, topic: &str, title: &str) -> Landing {
        Landing {
            sequence,
            commit: commit.to_owned(),
            task: task.to_owned(),
            topic: topic.to_owned(),
            title: title.to_owned(),
            contributes: true,
            verdict: None,
        }
    }

    fn review(sequence: u64, task: &str, verdict: ReviewStanding) -> Landing {
        Landing {
            sequence,
            commit: format!("review-{sequence}"),
            task: task.to_owned(),
            topic: "/spice/app".to_owned(),
            title: "Show presence".to_owned(),
            contributes: false,
            verdict: Some(verdict),
        }
    }
}
