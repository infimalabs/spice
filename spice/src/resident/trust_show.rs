//! Read one trust digest's recorded configuration as a line differential
//! against the most recently granted other digest.

use bus::Bus;
use journal::{FactPageStart, FactReadBudget, GuardError, JournalHead, JournalRead};
use wire::body::{TrustDocuments, TrustPended};
use wire::intent::{
    CommandAnswer, TrustBaseline, TrustDiffLine, TrustDocumentDiff, TrustLineEnding, TrustRef,
    TrustShowAnswer,
};
use wire::{ConfigSource, Digest, Outcome, Refusal, Reply, ReplyPosition, TrustHead, TrustState};

use super::store_fault;

/// Rulings a cause walk follows back to a digest's pend before it gives up.
const PEND_WALK_LIMIT: usize = 64;
/// Trust heads one page decodes while finding the latest grant.
const TRUST_PAGE_ROWS: usize = 256;
/// Unchanged lines kept on each side of a change.
const CONTEXT_LINES: usize = 3;

/// Answer `trust::show` from one journal snapshot.
pub(super) fn read(bus: &Bus, reference: &TrustRef) -> Outcome<CommandAnswer> {
    if reference.worktree.is_some() {
        return Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "trust::worktree-unexpected",
            reference.worktree.clone(),
            Some("the exact digest alone".to_owned()),
            None,
        )));
    }
    let digest = reference.digest.clone();
    let snapshot = match bus.read(move |read| Snapshot::read(read, &digest)) {
        Ok(snapshot) => snapshot,
        Err(error) => return store_fault("trust::journal-unreadable", &error.to_string()),
    };
    let Some(Snapshot {
        cursor,
        standing,
        documents,
        baseline,
    }) = snapshot
    else {
        return Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "trust::digest-unknown",
            Some(reference.digest.to_string()),
            Some("a pended, granted, or revoked digest".to_owned()),
            None,
        )));
    };
    for (digest, recorded) in std::iter::once((&reference.digest, documents.as_ref())).chain(
        baseline
            .as_ref()
            .map(|(digest, documents)| (digest, documents.as_ref())),
    ) {
        if let Some(recorded) = recorded
            && crate::trust_command::documents_digest(recorded) != *digest
        {
            return store_fault(
                "trust::documents-corrupt",
                &format!("the documents recorded for {digest} hash to another digest"),
            );
        }
    }
    let answer = answer(reference.digest.clone(), standing, documents, baseline);
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::TrustShow(Box::new(answer)),
    })
}

struct Snapshot {
    cursor: wire::Seq,
    standing: TrustState,
    documents: Option<TrustDocuments>,
    baseline: Option<(Digest, Option<TrustDocuments>)>,
}

impl Snapshot {
    fn read(read: &JournalRead<'_>, digest: &Digest) -> Result<Option<Self>, GuardError> {
        let Some(head) = read.trust_head(digest)? else {
            return Ok(None);
        };
        let cursor = read
            .cursor()?
            .ok_or_else(|| GuardError::Invalid("a trust head exists in an empty journal".into()))?;
        let documents = recorded_documents(read, head.position())?;
        let baseline = match latest_other_grant(read, digest)? {
            Some(granted) => Some((
                granted.body().digest.clone(),
                recorded_documents(read, granted.position())?,
            )),
            None => None,
        };
        Ok(Some(Self {
            cursor,
            standing: head.body().state,
            documents,
            baseline,
        }))
    }
}

/// The granted head committed last, other than `digest`'s own.
fn latest_other_grant(
    read: &JournalRead<'_>,
    digest: &Digest,
) -> Result<Option<JournalHead<TrustHead>>, GuardError> {
    let budget = FactReadBudget::new(TRUST_PAGE_ROWS, journal::MAX_RETAINED_FACT_PAGE_BYTES)
        .expect("a bounded trust head page is a repository budget");
    let mut latest: Option<JournalHead<TrustHead>> = None;
    let mut after = None;
    loop {
        let page = read.trust_head_page(after.as_ref(), budget)?;
        for head in page.rows {
            if head.body().state == TrustState::Granted
                && head.body().digest != *digest
                && latest
                    .as_ref()
                    .is_none_or(|latest| head.position() > latest.position())
            {
                latest = Some(head);
            }
        }
        match page.continuation {
            Some(next) => after = Some(next),
            None => return Ok(latest),
        }
    }
}

/// The documents a digest's pend recorded, found by following its current
/// head's ruling causes back to that pend; absent for a pend that recorded
/// none.
fn recorded_documents(
    read: &JournalRead<'_>,
    mut position: wire::Seq,
) -> Result<Option<TrustDocuments>, GuardError> {
    let budget = FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)
        .expect("one fact is a repository budget");
    for _ in 0..PEND_WALK_LIMIT {
        let page = read.fact_page(FactPageStart::At(position), position, budget)?;
        let Some(envelope) = page
            .rows
            .into_iter()
            .next()
            .filter(|fact| fact.seq == position)
        else {
            return Err(GuardError::Invalid(format!(
                "trust fact {position} is absent from its exact page"
            )));
        };
        if let Some(pended) = envelope.fact.read::<TrustPended>() {
            return Ok(pended.documents);
        }
        let Some(cause) = envelope.cause else {
            return Ok(None);
        };
        position = cause;
    }
    Ok(None)
}

fn answer(
    digest: Digest,
    standing: TrustState,
    documents: Option<TrustDocuments>,
    baseline: Option<(Digest, Option<TrustDocuments>)>,
) -> TrustShowAnswer {
    let empty = TrustDocuments {
        tracked: None,
        local: None,
    };
    let documents = documents.map(|documents| {
        let before = baseline
            .as_ref()
            .and_then(|(_, documents)| documents.as_ref())
            .unwrap_or(&empty);
        vec![
            document(
                ConfigSource::Tracked,
                before.tracked.as_deref(),
                documents.tracked.as_deref(),
            ),
            document(
                ConfigSource::Local,
                before.local.as_deref(),
                documents.local.as_deref(),
            ),
        ]
    });
    TrustShowAnswer {
        digest,
        standing,
        baseline: baseline.map(|(digest, documents)| TrustBaseline {
            digest,
            recorded: documents.is_some(),
        }),
        documents,
    }
}

fn document(source: ConfigSource, before: Option<&str>, after: Option<&str>) -> TrustDocumentDiff {
    TrustDocumentDiff {
        source,
        present: after.is_some(),
        lines: differential(before.unwrap_or_default(), after.unwrap_or_default()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Change {
    Kept,
    Added,
    Removed,
}

/// Line changes from `before` to `after` by Git's histogram diff with its
/// slider heuristics, keeping [`CONTEXT_LINES`] unchanged lines around each
/// change and eliding the rest.
fn differential(before: &str, after: &str) -> Vec<TrustDiffLine> {
    use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};

    let input = InternedInput::new(before, after);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    let text = |token| TrustLineEnding::split(input.interner[token]);
    let mut changes = Vec::with_capacity(input.before.len().max(input.after.len()));
    let mut kept = 0;
    for hunk in diff.hunks() {
        let (start, end) = (hunk.before.start as usize, hunk.before.end as usize);
        changes.extend(
            input.before[kept..start]
                .iter()
                .map(|token| (Change::Kept, text(*token))),
        );
        changes.extend(
            input.before[start..end]
                .iter()
                .map(|token| (Change::Removed, text(*token))),
        );
        changes.extend(
            input.after[hunk.after.start as usize..hunk.after.end as usize]
                .iter()
                .map(|token| (Change::Added, text(*token))),
        );
        kept = end;
    }
    changes.extend(
        input.before[kept..]
            .iter()
            .map(|token| (Change::Kept, text(*token))),
    );
    let near_change = |index: usize| {
        let low = index.saturating_sub(CONTEXT_LINES);
        let high = (index + CONTEXT_LINES + 1).min(changes.len());
        changes[low..high]
            .iter()
            .any(|(change, _)| *change != Change::Kept)
    };
    let mut lines = Vec::new();
    let mut elided = 0u64;
    for (index, (change, text)) in changes.iter().enumerate() {
        if *change == Change::Kept && !near_change(index) {
            elided += 1;
            continue;
        }
        if elided > 0 {
            lines.push(TrustDiffLine::Elided { count: elided });
            elided = 0;
        }
        let (text, ending) = (text.0.to_owned(), text.1);
        lines.push(match change {
            Change::Kept => TrustDiffLine::Kept { text, ending },
            Change::Added => TrustDiffLine::Added { text, ending },
            Change::Removed => TrustDiffLine::Removed { text, ending },
        });
    }
    if elided > 0 {
        lines.push(TrustDiffLine::Elided { count: elided });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(lines: &[TrustDiffLine]) -> Vec<String> {
        let marked = |marker, text, ending: &TrustLineEnding| match ending.note() {
            Some(note) => format!("{marker}{text} ({note})"),
            None => format!("{marker}{text}"),
        };
        lines
            .iter()
            .map(|line| match line {
                TrustDiffLine::Kept { text, ending } => marked(' ', text, ending),
                TrustDiffLine::Added { text, ending } => marked('+', text, ending),
                TrustDiffLine::Removed { text, ending } => marked('-', text, ending),
                TrustDiffLine::Elided { count } => format!("@{count}"),
            })
            .collect()
    }

    #[test]
    fn a_differential_marks_changes_and_elides_distant_unchanged_lines() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let after = "a\nb\nc\nd\ne\nF\ng\nh\ni\nj\nk\n";
        assert_eq!(
            rendered(&differential(before, after)),
            [
                "@2", " c", " d", " e", "-f", "+F", " g", " h", " i", " j", "+k"
            ]
        );
        assert_eq!(
            rendered(&differential("", "x\ny\n")),
            ["+x", "+y"],
            "with no baseline every line is an addition"
        );
        assert_eq!(rendered(&differential("x\ny\n", "x\ny\n")), ["@2"]);
    }

    #[test]
    fn interleaved_edits_align_on_their_common_lines() {
        let before = "keep\nold one\nshared\nold two\nend\n";
        let after = "keep\nshared\nnew\nend\n";
        assert_eq!(
            rendered(&differential(before, after)),
            [" keep", "-old one", " shared", "-old two", "+new", " end"]
        );
    }

    #[test]
    fn a_change_to_line_endings_alone_is_marked() {
        assert_eq!(
            rendered(&differential("a\nb\n", "a\nb")),
            [" a", "-b", "+b (no newline at end of file)"]
        );
        assert_eq!(
            rendered(&differential("a\nb\n", "a\r\nb\n")),
            ["-a", "+a (line ends with CRLF)", " b"]
        );
    }
}
