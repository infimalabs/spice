//! Predictive warnings for statements shared by live claims.

use std::collections::{BTreeMap, BTreeSet};

use wire::{CanonicalUuid, TaskHandle, TaskHead};

use crate::Entry;
use crate::similarity::{CLAIM_OVERLAP_FLOOR, MatchCache};

/// A shared identifier is exact evidence only while at most one in this many
/// claim-time statements names it, and never fewer than the pair itself.
const IDENTIFIER_RARITY: usize = 32;

/// One active claim whose authored statement overlaps the selected one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimOverlap {
    /// Other in-flight task.
    pub task: TaskHandle,
    /// Lane holding the other claim.
    pub lane: CanonicalUuid,
    /// Rare exact identifiers both statements name, then their strongest
    /// shared weighted terms.
    pub surface: Vec<String>,
}

/// Find active claims whose statement overlaps the selected one.
///
/// Every statement is weighted under the shared fit of the recent window of
/// `entries`, with the conversation background that fit already carries. A
/// claim warns when its cosine with the selected
/// statement reaches [`CLAIM_OVERLAP_FLOOR`], or when both name one exact
/// code-shaped identifier few of `entries` carry, so board-wide boilerplate
/// never qualifies. Topic names are deliberately absent: a
/// topic is a scheduling route, not evidence that two tasks touch the same
/// product or code surface.
#[must_use]
pub fn claim_overlaps(selected: &Entry, entries: &[Entry]) -> Vec<ClaimOverlap> {
    let matcher = MatchCache::shared();
    let frequency = matcher.fit_carried(entries.iter().map(Entry::statement));
    let mut identifier_frequency = BTreeMap::<String, usize>::new();
    for identifier in entries
        .iter()
        .flat_map(|entry| identifiers(entry.statement()))
    {
        *identifier_frequency.entry(identifier).or_default() += 1;
    }
    let rare_limit = (entries.len() / IDENTIFIER_RARITY).max(2);
    let selected_vector = matcher.prepare(&frequency, selected.statement());
    let selected_identifiers = identifiers(selected.statement());
    let mut overlaps = entries
        .iter()
        .filter(|entry| entry.is_held() && entry.born() != selected.born())
        .filter_map(|entry| {
            let mut surface = selected_identifiers
                .intersection(&identifiers(entry.statement()))
                .filter(|identifier| {
                    identifier_frequency
                        .get(*identifier)
                        .is_none_or(|count| *count <= rare_limit)
                })
                .cloned()
                .collect::<Vec<_>>();
            let vector = matcher.prepare(&frequency, entry.statement());
            let (similar, explanation) = selected_vector.as_ref().zip(vector.as_ref()).map_or(
                (false, Vec::new()),
                |(left, right)| {
                    (
                        left.cosine(right)
                            .is_ok_and(|score| score >= CLAIM_OVERLAP_FLOOR),
                        left.explanation(right),
                    )
                },
            );
            if !similar && surface.is_empty() {
                return None;
            }
            surface.extend(explanation);
            Some((entry, surface))
        })
        .flat_map(|(entry, surface)| {
            entry.active_claims().map(move |claim| ClaimOverlap {
                task: entry.handle().clone(),
                lane: claim.claim().lane,
                surface: surface.clone(),
            })
        })
        .collect::<Vec<_>>();
    overlaps.sort_by(|left, right| {
        left.task
            .cmp(&right.task)
            .then_with(|| left.lane.cmp(&right.lane))
    });
    overlaps
}

/// Exact code-shaped identifiers in a statement's authored fields.
fn identifiers(statement: &TaskHead) -> BTreeSet<String> {
    std::iter::once(statement.title.as_str())
        .chain(statement.description.as_deref())
        .chain(statement.term.iter().map(String::as_str))
        .flat_map(|text| {
            text.split(|c: char| {
                !(c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | ':' | '-'))
            })
        })
        .map(|raw| raw.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|raw| code_shaped(raw))
        .map(str::to_owned)
        .collect()
}

/// A path or a dotted, namespaced, snake_case, or CamelCase name. All-caps
/// words are command placeholders and protocol vocabulary here, not names.
fn code_shaped(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let joined = raw.contains("::")
        || bytes.windows(3).any(|window| {
            window[0].is_ascii_alphanumeric()
                && matches!(window[1], b'/' | b'_' | b'.')
                && window[2].is_ascii_alphanumeric()
        });
    let camel = bytes
        .windows(2)
        .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase());
    bytes.len() >= 4 && bytes.iter().any(u8::is_ascii_alphabetic) && (joined || camel)
}

#[cfg(test)]
mod tests {
    use wire::{
        ClaimDisposition, ClaimHead, CompletionHead, LeaseHead, Phase, Seq, TaskStamp,
        WorkflowHead, body,
    };

    use super::*;

    const PEER: &str = "019fbbde-5105-75a3-a76a-722afde1e408";

    fn seq(value: u64) -> Seq {
        Seq::new(value).expect("non-zero sequence")
    }

    fn phase(text: &str) -> Phase {
        text.parse().expect("phase")
    }

    fn uuid(text: &str) -> CanonicalUuid {
        text.parse().expect("canonical UUID")
    }

    fn word(index: u64) -> String {
        let letter = |place: u64| char::from(b'a' + ((index / place) % 26) as u8);
        format!("word{}{}", letter(26), letter(1))
    }

    fn entry(index: u64, title: &str, term: &[&str], claimed: bool) -> Entry {
        let born = TaskStamp::for_micros(index as i64 + 1).expect("fixture stamp");
        let statement = TaskHead {
            preparation: None,
            intent: None,
            born: born.clone(),
            topic_origin: seq(1),
            title: title.to_owned(),
            pitch: "Overlapping work must be noticed.".to_owned(),
            description: None,
            term: term.iter().map(|item| (*item).to_owned()).collect(),
            requirements: wire::EnvironmentRequirements::default(),
            resources: wire::ResourceRequirements::default(),
            topic: "/board/kernel".parse().expect("fixture topic"),
            priority: None,
            taste: None,
            dependencies: Vec::new(),
            provenance: wire::TaskProvenance::Task(born),
            creation: body::TaskCreationContext::unavailable(
                body::TaskCreationUnavailable::SpiceActor,
            ),
            revision: seq(index + 1),
        };
        let workflow = WorkflowHead {
            flow: vec![phase("draft"), phase("todo"), phase("review")],
            phase: phase("todo"),
            status: wire::WorkflowStatus::pending(),
            deferral: None,
            submitted: false,
            generation: 0,
        };
        let epoch = uuid("019fbbde-5105-75a3-a76a-722afde1e410");
        let claims = if claimed {
            let instant = |text: &str| text.parse().expect("canonical instant");
            vec![(
                ClaimHead {
                    actor: wire::Actor::Spice,
                    fact: seq(2),
                    claimed: seq(2),
                    cell: wire::ClaimCell {
                        topic: Some("/board/kernel".parse().expect("fixture topic")),
                        phase: phase("todo"),
                    },
                    lane: uuid(PEER),
                    occupant: uuid("019fbbde-5105-75a3-a76a-722afde1e409"),
                    epoch,
                    recovery: None,
                    disposition: ClaimDisposition::Active,
                    acknowledged: 0,
                },
                Some(LeaseHead {
                    claimed: seq(2),
                    start: instant("2026-08-01T06:00:00.000Z"),
                    renewal: instant("2026-08-01T06:01:00.000Z"),
                    expiry: instant("2026-08-01T06:10:00.000Z"),
                    maximum: instant("2026-08-01T06:20:00.000Z"),
                    epoch,
                }),
            )]
        } else {
            Vec::new()
        };
        Entry::new(statement, workflow, CompletionHead::opened(seq(1)), claims)
            .expect("fixture entry")
    }

    /// Selected and peer statements over a board where every statement says
    /// design, lane, and team, carries `boilerplate`, and adds one own word.
    fn overlaps(selected: &[&str], peer: &[&str], boilerplate: &str) -> Vec<ClaimOverlap> {
        let mut entries = (10..34)
            .map(|index| {
                entry(
                    index,
                    &format!("Design lane team {}", word(index)),
                    &[boilerplate],
                    false,
                )
            })
            .collect::<Vec<_>>();
        let selected = entry(1, "Design lane team after", selected, false);
        entries.push(entry(2, "Design lane team after", peer, true));
        entries.push(selected.clone());
        claim_overlaps(&selected, &entries)
    }

    const BOILERPLATE: &str = "Permanent tests stay in-process under spice::gate";

    #[test]
    fn shared_plain_words_and_board_wide_identifiers_stay_silent() {
        assert!(
            overlaps(
                &[BOILERPLATE, "Rotate handoff review slots weekly"],
                &[BOILERPLATE, "Compact ledger receipts nightly"],
                BOILERPLATE,
            )
            .is_empty()
        );
    }

    #[test]
    fn a_rare_exact_path_or_symbol_warns_by_itself() {
        let found = overlaps(
            &[BOILERPLATE, "Rotate handoff review slots in src/db.rs"],
            &[BOILERPLATE, "Compact ledger receipts through Db::tx"],
            BOILERPLATE,
        );
        assert!(found.is_empty(), "different identifiers are no overlap");

        let found = overlaps(
            &[BOILERPLATE, "Rotate handoff review slots through Db::tx"],
            &[
                BOILERPLATE,
                "Compact ledger receipts in src/db.rs via Db::tx",
            ],
            BOILERPLATE,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lane, uuid(PEER));
        assert_eq!(found[0].surface[0], "Db::tx");
        assert!(!found[0].surface.contains(&"src/db.rs".to_owned()));
    }

    #[test]
    fn near_duplicate_statements_warn_with_their_strongest_shared_terms() {
        let found = overlaps(
            &["Rebuild the watchband mosaic so focused agents stay visible"],
            &["Rebuild the watchband mosaic keeping focused agents visible"],
            BOILERPLATE,
        );
        assert_eq!(found.len(), 1);
        for term in ["mosaic", "watchband"] {
            assert!(found[0].surface.contains(&term.to_owned()), "{found:?}");
        }
    }
}
