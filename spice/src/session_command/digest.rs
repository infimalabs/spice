//! The briefing as one folded page: what binds the lane first, then the
//! least working set that lets it resume, under one fixed byte ceiling.
//!
//! Sections are ranked once, here. Each keeps whole rows while they fit and
//! then names its continuation, so what folds away is always one exact
//! command from the page. A section that cannot start at all is named in the
//! closing lines. The held claim alone is never cut: its holder works from its
//! whole statement, so the sections below it fold instead. Text rows are
//! whitespace-collapsed before they arrive, so every row is one line and the
//! ceiling is a byte count of what the agent reads.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::model::{
    BoardGlanceView, HeldWorkView, LiveView, OutstandingAskView, PeerView, PostureView, RecallView,
    RepositoryGlanceView, RetiredDirectionView,
};
use super::project::{HistorySource, session_command};
use crate::gate_command::LatchDebtView;

/// Ceiling on one folded briefing, in bytes.
pub(crate) const BRIEFING_BYTES: usize = 9_600;
/// Room every section keeps so a cut can still name its continuation.
const FOLD_BYTES: usize = 320;
/// Bytes one row keeps before it is cut.
const ROW_BYTES: usize = 1_100;

const REFRESH: &str = "spice session briefing";
const STATUS: &str = "spice task status";
const INBOX: &str = "spice inbox";
const LANES: &str = "spice lane list";

/// Fold one briefing into its page.
pub(super) fn fold(report: &RecallView, files: &[String]) -> String {
    fold_history(report, &HistorySource::Files(files.to_vec()))
}

pub(super) fn fold_history(report: &RecallView, history_source: &HistorySource) -> String {
    let RecallView {
        asks,
        held_work,
        posture,
        peers,
        directions,
        board,
        latch_debt,
        repository,
        unknowns,
        live,
        residues,
        ..
    } = report;
    let history = |view: &str| -> String { session_command(view, history_source, []).to_string() };
    let refresh = history("briefing");
    let typed = format!("{refresh} --json");
    let sweep = format!("{} --limit 4", history("sweep"));
    let mut page = Page::new(
        header(held_work, live, history_source, *residues),
        &sweep,
        &refresh,
        &typed,
    );

    // An empty inbox says nothing; an unread one says so.
    match asks {
        Some(asks) => {
            let more = asks.omission.as_ref().map_or_else(
                || INBOX.to_owned(),
                |omission| omission.continuation.to_string(),
            );
            let total = asks
                .omission
                .as_ref()
                .map_or(asks.rows.len(), |omission| omission.population);
            page.section(
                PENDING_TITLE,
                asks.rows.iter().map(ask).collect(),
                total,
                &more,
            );
        }
        None => page.whole(
            "Pending steering",
            vec!["unread; see Unknown".to_owned()],
            INBOX,
        ),
    }
    page.exempt("Held claim", held(held_work, repository.as_ref()));
    page.whole(
        "Unknown",
        unknowns
            .iter()
            .map(|row| format!("{}/{}: {}", row.authority, row.probe, row.detail))
            .collect(),
        &typed,
    );
    if let Some(board) = board {
        page.whole(
            "Board",
            vec![board_row(board, held_work.peer_claims, latch_debt.as_ref())],
            STATUS,
        );
    }
    page.whole("Peers", peers.iter().map(peer).collect(), LANES);
    page.whole("Posture", posture_rows(posture.as_ref()), &typed);
    if let Some(repository) = repository {
        let (dirty, total) = uncommitted(repository);
        page.section("Uncommitted", dirty, total, "git status --short");
        if let Some(baseline) = &repository.unlanded_baseline {
            page.section(
                "Unlanded commits",
                repository.unlanded.clone(),
                repository.unlanded_total,
                &format!("git log --oneline {baseline}..HEAD"),
            );
        }
        page.whole("Commits", repository.commits.clone(), "git log --oneline");
    }
    let mut seen = BTreeSet::new();
    page.whole("Latest turn", latest(live, &mut seen), &history("turns"));
    page.whole(
        "Recent turns, newest first",
        recent(live, &mut seen),
        &sweep,
    );
    page.whole(
        "Recently answered steering, newest first",
        directions.iter().map(direction).collect(),
        INBOX,
    );
    page.whole(
        "Recently touched paths",
        live.touched.clone(),
        &history("turns"),
    );
    page.finish()
}

/// The page a supervisor hands its agent when the briefing itself failed.
pub(crate) fn refused(refusal: &wire::Refusal) -> String {
    let detail = refusal
        .diagnostic()
        .and_then(wire::RefusalDiagnostic::detail)
        .map_or_else(String::new, |detail| format!(": {detail}"));
    clip_row(format!(
        "# Spice briefing unavailable\n{}{detail}\nRead the same ground with `{REFRESH}`, `{STATUS}`, and `{INBOX}`.\n",
        refusal.reason()
    ))
}

/// The section listing pending steering, one row per key, key first.
const PENDING_TITLE: &str = "Pending steering (answer every key)";

/// The page without the pending-steering rows whose key `answered` holds.
///
/// A section left with no row is dropped whole, as an empty inbox says
/// nothing; a withheld-rows line still owes answers, so it keeps the section.
pub(crate) fn without_answered(page: &str, answered: impl Fn(&str) -> bool) -> String {
    let heading = format!("\n## {PENDING_TITLE}\n");
    let Some(start) = page.find(&heading) else {
        return page.to_owned();
    };
    let rows = start + heading.len();
    let end = page[rows..]
        .find("\n## ")
        .map_or(page.len(), |offset| rows + offset);
    let kept = page[rows..end]
        .split_inclusive('\n')
        .filter(|row| {
            row.strip_prefix("- ")
                .and_then(|row| row.split(' ').next())
                .is_none_or(|key| !answered(key))
        })
        .collect::<String>();
    let mut filtered = String::with_capacity(page.len());
    filtered.push_str(&page[..start]);
    if !kept.is_empty() {
        filtered.push_str(&heading);
        filtered.push_str(&kept);
    }
    filtered.push_str(&page[end..]);
    filtered
}

struct Page {
    text: String,
    /// Closing lines naming the refresh commands, always kept whole.
    refresh: String,
    limit: usize,
    folded: Vec<&'static str>,
    typed: String,
}

impl Page {
    fn new(header: String, sweep: &str, refresh_command: &str, typed: &str) -> Self {
        let refresh = format!(
            "\n## Refresh\n- this page: `{refresh_command}`; typed rows: `{typed}`\n- board and claim: `{STATUS}`; steering: `{INBOX}`\n- older turns: `{sweep}`\n"
        );
        Self {
            limit: BRIEFING_BYTES.saturating_sub(refresh.len() + FOLD_BYTES),
            text: header,
            refresh,
            typed: typed.to_owned(),
            folded: Vec::new(),
        }
    }

    /// Every row of a section whose rows are its whole population.
    fn whole(&mut self, title: &'static str, rows: Vec<String>, more: &str) {
        let total = rows.len();
        self.section(title, rows, total, more);
    }

    /// Every row whole, past the ceiling: the held claim is what its holder
    /// works from, so lower sections fold to make room rather than it.
    fn exempt(&mut self, title: &'static str, rows: Vec<String>) {
        let _ = write!(self.text, "\n## {title}\n");
        for row in rows {
            let _ = writeln!(self.text, "- {row}");
        }
    }

    /// Rows while they fit, then one line naming what was withheld and the
    /// command that reads it. `total` counts rows a source already withheld.
    fn section(&mut self, title: &'static str, rows: Vec<String>, total: usize, more: &str) {
        if rows.is_empty() {
            return;
        }
        let heading = format!("\n## {title}\n");
        if self.text.len() + heading.len() + FOLD_BYTES > self.limit {
            self.folded.push(title);
            return;
        }
        self.text.push_str(&heading);
        let mut shown = 0;
        for row in rows {
            let row = clip_row(row);
            if self.text.len() + row.len() + 3 + FOLD_BYTES > self.limit {
                break;
            }
            let _ = writeln!(self.text, "- {row}");
            shown += 1;
        }
        if shown < total {
            let fold = clip_fold(format!("- … {} more: `{more}`", total - shown));
            let _ = writeln!(self.text, "{fold}");
        }
    }

    fn finish(mut self) -> String {
        self.text.push_str(&self.refresh);
        if !self.folded.is_empty() {
            let line = clip_fold(format!(
                "- folded away: {} — `{}`",
                self.folded.join("; "),
                self.typed,
            ));
            let _ = writeln!(self.text, "{line}");
        }
        self.text
    }
}

fn header(
    held_work: &HeldWorkView,
    live: &LiveView,
    history: &HistorySource,
    residues: usize,
) -> String {
    let mut header = String::from("# Spice briefing\n");
    let mut facts = Vec::new();
    if let Some(lane) = &held_work.lane {
        facts.push(format!("lane {lane}"));
    }
    if let Some(address) = &held_work.address {
        facts.push(format!("address {address}"));
    }
    if let Some(thread) = &held_work.thread {
        facts.push(format!("thread {thread}"));
    }
    if let Some(compacted) = &live.compacted {
        facts.push(format!("last compaction {compacted}"));
    }
    if residues > 0 {
        facts.push(format!("{residues} unreadable transcript records"));
    }
    if let Some(label) = history.label() {
        facts.push(label);
    }
    if !facts.is_empty() {
        let _ = writeln!(header, "{}", clip_row(facts.join(" · ")));
    }
    header
}

fn peer(row: &PeerView) -> String {
    let mut words = vec![row.address.to_string(), format!("lane {}", row.lane)];
    words.extend(row.name.iter().map(|name| format!("\"{name}\"")));
    words.push(row.vendor.clone());
    words.extend(row.model.clone());
    clip_row(words.join(" "))
}

fn ask(row: &OutstandingAskView) -> String {
    let attachments = if row.attachments.is_empty() {
        String::new()
    } else {
        format!(" [{} attachments]", row.attachments.len())
    };
    format!(
        "{} ({}, authored {}){attachments}: {}",
        row.key, row.stage, row.authored, row.text
    )
}

fn held(held_work: &HeldWorkView, repository: Option<&RepositoryGlanceView>) -> Vec<String> {
    let Some(claim) = held_work.own_claim.as_ref() else {
        if let Some(refusal) = &held_work.own_claim_unread {
            return vec![format!("unknown, not none: {refusal}")];
        }
        return match repository.filter(|repository| repository.is_loose()) {
            Some(repository) => loose(repository),
            None => vec!["none; take work with `spice task next`".to_owned()],
        };
    };
    let mut rows = vec![format!(
        "{} [{}] {}: {}",
        claim.handle, claim.topic, claim.phase, claim.title
    )];
    let lease = claim
        .lease_expiry
        .as_ref()
        .map_or_else(String::new, |expiry| format!("; lease until {expiry}"));
    rows.push(format!(
        "{} in flow {}{lease}",
        claim.status,
        claim.flow.join(" → ")
    ));
    rows.push(format!("pitch: {}", claim.pitch));
    rows.push(claim.completion_contract.clone());
    rows.extend(claim.design_research.iter().cloned());
    if let Some(description) = &claim.description {
        rows.push(format!("description: {description}"));
    }
    rows.extend(claim.term.iter().map(|row| format!("term: {row}")));
    rows
}

/// The claimless lane's implicit task: settle the work its worktree holds
/// before it takes any other.
fn loose(repository: &RepositoryGlanceView) -> Vec<String> {
    let mut held = Vec::new();
    if repository.dirty_total > 0 {
        held.push(format!("{} uncommitted paths", repository.dirty_total));
    }
    if repository.unlanded_total > 0 {
        held.push(format!("{} unlanded commits", repository.unlanded_total));
    }
    if let Some(operation) = &repository.operation {
        held.push(format!("an unfinished {operation}"));
    }
    let mut rows = vec![
        format!(
            "none, but this worktree holds loose work: {}; settle it before taking other work",
            held.join(", ")
        ),
        format!("keep it: {}", spice::task_git::UNOWNED_WORK_REMEDY),
        "uncommitted changes need `spice gate prove` before they are committed; unlanded commits already passed the commit gates".to_owned(),
        "discard only abandoned work, with `git restore` and `git clean`".to_owned(),
    ];
    if let Some(operation) = &repository.operation {
        rows.push(format!(
            "first finish the {operation} with `git {operation} --continue` or abandon it with `git {operation} --abort`"
        ));
    }
    rows
}

fn posture_rows(posture: Option<&PostureView>) -> Vec<String> {
    let Some(posture) = posture else {
        return Vec::new();
    };
    let mut rows = vec![format!(
        "worktree {} on {}",
        posture.worktree,
        posture.head.as_deref().unwrap_or("an unreadable HEAD")
    )];
    if posture.cwd != posture.worktree {
        rows.push(format!("cwd {}", posture.cwd));
    }
    if let Some(lane) = &posture.lane {
        rows.push(format!(
            "lane {} {}, {} {}",
            lane.disposition,
            lane.life,
            lane.vendor,
            lane.model.as_deref().unwrap_or("default model")
        ));
        if let Some(hold) = &lane.hold {
            rows.push(format!("hold until {}: {}", hold.until, hold.evidence));
        }
    }
    rows
}

fn uncommitted(repository: &RepositoryGlanceView) -> (Vec<String>, usize) {
    if let Some(error) = &repository.unreadable {
        return (vec![format!("unreadable: {error}")], 1);
    }
    if repository.dirty_total == 0 {
        return (vec!["clean".to_owned()], 1);
    }
    (repository.dirty.clone(), repository.dirty_total)
}

fn latest(live: &LiveView, seen: &mut BTreeSet<String>) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(turn) = &live.turn {
        rows.push(format!(
            "{} {}: {}",
            turn.start,
            if turn.completed { "completed" } else { "open" },
            counts(turn.commands, turn.patches, turn.errors)
        ));
        if let Some(prompt) = &turn.prompt {
            rows.push(format!("prompt: {prompt}"));
        }
    }
    if let Some(answer) = &live.latest_final {
        seen.insert(normalized(&answer.text));
        rows.push(format!("last answer: {answer}"));
    }
    rows
}

fn recent(live: &LiveView, seen: &mut BTreeSet<String>) -> Vec<String> {
    live.recent
        .iter()
        .skip(usize::from(live.turn.is_some()))
        .map(|turn| {
            let answer = turn
                .final_answer
                .as_ref()
                .filter(|answer| seen.insert(normalized(&answer.text)))
                .map_or_else(String::new, |answer| format!(" — {answer}"));
            format!(
                "{} {}{answer}",
                turn.start,
                counts(turn.commands, turn.patches, turn.errors)
            )
        })
        .collect()
}

fn counts(commands: u64, patches: u64, errors: u64) -> String {
    format!("{commands} commands, {patches} patches, {errors} errors")
}

fn direction(row: &RetiredDirectionView) -> String {
    format!(
        "{} {} (authored {}): {}",
        row.key, row.disposition, row.published, row.text
    )
}

fn board_row(
    board: &BoardGlanceView,
    peer_claims: Option<usize>,
    latch_debt: Option<&LatchDebtView>,
) -> String {
    let unclaimed = if board.unclaimed.is_empty() {
        "no unclaimed pending work".to_owned()
    } else {
        format!(
            "unclaimed pending: {}",
            board
                .unclaimed
                .iter()
                .map(|(phase, count)| format!("{phase} {count}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let peers = peer_claims.map_or_else(
        || "peer claims unknown".to_owned(),
        |count| format!("{count} peer claims"),
    );
    let partial = if board.partial {
        "; scan stopped early"
    } else {
        ""
    };
    let debt = latch_debt.map_or_else(
        || "bound debt unknown".to_owned(),
        |debt| format!("bound debt {debt}"),
    );
    format!("{unclaimed}; {peers}; {debt}{partial}")
}

/// Casefolded, whitespace-collapsed text, so one answer is shown once.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn clip_row(row: String) -> String {
    clip(row, ROW_BYTES)
}

fn clip_fold(row: String) -> String {
    clip(row, FOLD_BYTES - 1)
}

fn clip(mut row: String, bytes: usize) -> String {
    if row.len() > bytes {
        let end = row.floor_char_boundary(bytes - '…'.len_utf8());
        row.truncate(end);
        row.push('…');
    }
    row
}

#[cfg(test)]
mod tests {
    use wire::output::{Bounded, clip};

    use super::super::model::{
        BoardGlanceView, HeldClaimView, HeldWorkView, LanePostureView, LiveView,
        OutstandingAskView, PostureView, RecallView, RecentTurnView, RepositoryGlanceView,
        RetiredDirectionView,
    };
    use super::{BRIEFING_BYTES, LatchDebtView, fold, without_answered};

    fn text(bytes: usize) -> wire::output::ClippedText {
        clip(&"word ".repeat(bytes / 5), bytes)
    }

    /// A briefing whose every section holds `rows` rows of about `bytes`.
    fn recall(rows: usize, bytes: usize) -> RecallView {
        RecallView {
            asks: Some(Bounded {
                rows: (0..rows)
                    .map(|index| OutstandingAskView {
                        directive: format!("d{index}"),
                        key: format!("key{index}"),
                        stage: "new".to_owned(),
                        text: text(bytes),
                        attachments: Vec::new(),
                        authored: "2026-01-01T00:00:00Z".to_owned(),
                        expires: "never".to_owned(),
                    })
                    .collect(),
                omission: None,
            }),
            held_work: HeldWorkView {
                thread: Some("thread".to_owned()),
                lane: Some("lane".to_owned()),
                own_claim: Some(HeldClaimView {
                    completion_contract: "Completion contract: own AC.".to_owned(),
                    design_research: None,
                    handle: "TASK-held".to_owned(),
                    topic: "/p".parse().expect("fixture topic"),
                    phase: "todo".to_owned(),
                    status: "pending".to_owned(),
                    title: "held title".to_owned(),
                    pitch: "held pitch".to_owned(),
                    lease_expiry: None,
                    flow: vec!["todo".to_owned(), "review".to_owned()],
                    description: Some("word ".repeat(bytes / 5)),
                    term: (0..rows)
                        .map(|index| format!("criterion {index} {}", "word ".repeat(bytes / 5)))
                        .collect(),
                }),
                own_claim_unread: None,
                peer_claims: Some(rows),
                renewed_from: None,
                address: None,
            },
            posture: None,
            peers: Vec::new(),
            directions: (0..rows)
                .map(|index| RetiredDirectionView {
                    key: format!("old{index}"),
                    disposition: "acked".to_owned(),
                    published: "2026-01-01T00:00:00Z".to_owned(),
                    text: text(bytes),
                })
                .collect(),
            board: Some(BoardGlanceView {
                unclaimed: vec![("todo".to_owned(), rows)],
                partial: false,
            }),
            latch_debt: Some(LatchDebtView {
                latched: rows,
                oldest_age_seconds: Some(86_400),
            }),
            repository: Some(RepositoryGlanceView {
                dirty: (0..rows).map(|index| format!(" M file{index}")).collect(),
                dirty_total: rows,
                commits: (0..rows)
                    .map(|index| format!("abc{index} subject"))
                    .collect(),
                unlanded: (0..rows)
                    .map(|index| format!("def{index} unlanded"))
                    .collect(),
                unlanded_total: rows,
                unlanded_baseline: Some("0123456789ab".to_owned()),
                operation: None,
                unreadable: None,
            }),
            unknowns: Vec::new(),
            live: LiveView {
                turn: None,
                latest_final: None,
                compacted: None,
                recent: (0..rows)
                    .map(|index| RecentTurnView {
                        start: format!("2026-01-01T00:00:{index:02}Z"),
                        completed: true,
                        commands: 1,
                        patches: 1,
                        errors: 0,
                        final_answer: Some(text(bytes)),
                    })
                    .collect(),
                touched: (0..rows).map(|index| format!("src/{index}.rs")).collect(),
                history: Vec::new(),
            },
            sources: Vec::new(),
            residues: 0,
            digest: String::new(),
        }
    }

    /// Lines naming withheld rows, each with the exact command that reads them.
    fn folds(page: &str) -> Vec<&str> {
        page.lines()
            .filter(|line| line.starts_with("- … "))
            .collect()
    }

    #[test]
    fn a_small_briefing_is_whole_but_names_rows_its_source_withheld() {
        let mut report = recall(2, 40);
        let page = fold(&report, &[]);
        assert!(folds(&page).is_empty(), "{page}");

        report
            .repository
            .as_mut()
            .expect("fixture repository")
            .dirty_total = 5;
        assert_eq!(
            folds(&fold(&report, &[])),
            ["- … 3 more: `git status --short`"]
        );
    }

    #[test]
    fn a_claimless_briefing_makes_loose_work_the_held_task() {
        let mut report = recall(2, 40);
        report.held_work.own_claim = None;
        let page = fold(&report, &[]);
        let held = section(&page, "Held claim");
        assert!(
            held.contains("2 uncommitted paths, 2 unlanded commits"),
            "{held}"
        );
        assert!(held.contains("spice task capture"), "{held}");
        assert!(!held.contains("spice task next"), "{held}");
        assert!(
            page.contains("## Unlanded commits\n- def0 unlanded"),
            "{page}"
        );

        let repository = report.repository.as_mut().expect("fixture repository");
        repository.dirty_total = 0;
        repository.unlanded_total = 0;
        repository.operation = Some("rebase".to_owned());
        let held = section(&fold(&report, &[]), "Held claim").to_owned();
        assert!(held.contains("an unfinished rebase"), "{held}");
        assert!(held.contains("git rebase --abort"), "{held}");

        report
            .repository
            .as_mut()
            .expect("fixture repository")
            .operation = None;
        let held = section(&fold(&report, &[]), "Held claim").to_owned();
        assert!(held.contains("take work with `spice task next`"), "{held}");
    }

    #[test]
    fn an_oversized_briefing_folds_under_its_ceiling_and_names_every_cut() {
        for (rows, bytes) in [(8, 400), (16, 1024), (64, 2000)] {
            let page = fold(&recall(rows, bytes), &["/t/x.jsonl".to_owned()]);
            // The held claim is exempt; everything else stays under it.
            let held = section(&page, "Held claim");
            assert!(
                page.len() - held.len() <= BRIEFING_BYTES,
                "{rows}x{bytes}: {}",
                page.len()
            );
            // Every cut names a command, and every section either appears
            // or is listed as folded away under the typed read.
            for line in folds(&page) {
                assert!(
                    line.contains("more: `spice ") || line.contains("more: `git "),
                    "{line}"
                );
            }
            for title in ["Recent turns", "Recently touched paths"] {
                assert!(
                    page.contains(&format!("## {title}")) || page.contains("folded away"),
                    "{rows}x{bytes}: {title} vanished\n{page}"
                );
            }
            assert!(page.ends_with('\n'));
            assert!(page.contains("`spice session sweep /t/x.jsonl --limit 4`"));
        }
    }

    #[test]
    fn an_unread_inbox_is_not_an_empty_inbox() {
        let mut report = recall(1, 40);
        report.asks.as_mut().expect("fixture asks").rows.clear();
        assert!(!fold(&report, &[]).contains("## Pending steering"));

        report.asks = None;
        assert!(fold(&report, &[]).contains("## Pending steering\n- unread; see Unknown\n"));
    }

    #[test]
    fn an_answered_key_leaves_the_briefing_while_its_peers_stay_owed() {
        let page = fold(&recall(2, 40), &[]);
        assert!(page.contains("- key0 (new") && page.contains("- key1 (new"));

        let one = without_answered(&page, |key| key == "key0");
        assert!(!one.contains("key0 ("), "{one}");
        assert!(one.contains("## Pending steering (answer every key)\n- key1 (new"));
        assert!(one.contains("\n\n## Held claim"), "{one}");

        let none = without_answered(&page, |key| key.starts_with("key"));
        assert!(!none.contains("## Pending steering"), "{none}");
        assert_eq!(
            none,
            fold(
                &{
                    let mut report = recall(2, 40);
                    report.asks.as_mut().expect("fixture asks").rows.clear();
                    report
                },
                &[]
            ),
            "dropping every row leaves the page an empty inbox folds to"
        );
    }

    #[test]
    fn binding_authority_outranks_working_set() {
        let page = fold(&recall(64, 2000), &[]);
        let steering = page.find("## Pending steering").expect("steering leads");
        let held = page.find("## Held claim").expect("held work survives");
        assert!(steering < held);
        assert!(page.contains("key0 (new"), "the first ask is shown: {page}");
        assert!(page.contains("TASK-held [/p] todo: held title"));
        // Pressure folds the lowest-ranked sections first.
        assert!(!folds(&page).is_empty(), "{page}");
        assert!(!page.contains("## Recently touched paths"));
    }

    #[test]
    fn the_held_claim_stays_whole_while_lower_sections_fold() {
        let page = fold(&recall(64, 2000), &[]);
        let held = section(&page, "Held claim");
        let words = "word ".repeat(400);
        assert!(held.contains("\n- pitch: held pitch\n"), "{held}");
        assert!(
            held.contains(&format!("\n- description: {words}\n")),
            "{held}"
        );
        for index in 0..64 {
            let row = format!("\n- term: criterion {index} {words}\n");
            assert!(held.contains(&row), "{row} is cut: {held}");
        }
        assert!(page.contains("folded away"), "{page}");
    }

    /// One section of `page`, from its heading to the next one.
    fn section<'page>(page: &'page str, title: &str) -> &'page str {
        let start = page
            .find(&format!("\n## {title}\n"))
            .expect("the section is shown");
        let end = page[start + 1..]
            .find("\n## ")
            .map_or(page.len(), |offset| start + 1 + offset);
        &page[start..end]
    }

    #[test]
    fn a_current_lane_hold_is_visible_in_the_briefing_until_resumed() {
        let mut report = recall(1, 40);
        let hold_evidence = "occupant ended on a vendor error: provider refused this turn";
        report.posture = Some(PostureView {
            cwd: "/worktree".to_owned(),
            worktree: "/worktree".to_owned(),
            head: Some("main".to_owned()),
            lane: Some(LanePostureView {
                worktree: "/worktree".to_owned(),
                disposition: "vacant".to_owned(),
                life: "drive".to_owned(),
                vendor: "omp".to_owned(),
                model: None,
                hold: Some(wire::intent::LaneHold {
                    evidence: hold_evidence.to_owned(),
                    until: "2026-10-08T22:30:00.000Z".parse().expect("fixture instant"),
                }),
            }),
        });

        let held = fold(&report, &[]);
        assert!(held.contains(hold_evidence), "{held}");
        assert!(held.contains("2026-10-08T22:30:00.000Z"), "{held}");

        report
            .posture
            .as_mut()
            .and_then(|posture| posture.lane.as_mut())
            .expect("fixture lane posture")
            .hold = None;
        assert!(!fold(&report, &[]).contains(hold_evidence));
    }

    #[test]
    fn one_answer_is_shown_once() {
        let mut report = recall(1, 40);
        let live = &mut report.live;
        live.recent[0].final_answer = Some(clip("the one distinctive answer", 80));
        let answer = live.recent[0].final_answer.clone();
        live.latest_final = answer.clone();
        live.recent.insert(
            0,
            RecentTurnView {
                start: "later".to_owned(),
                completed: false,
                commands: 0,
                patches: 0,
                errors: 0,
                final_answer: None,
            },
        );
        live.turn = Some(super::super::model::LiveTurnView {
            start: "later".to_owned(),
            end: None,
            completed: false,
            commands: 0,
            patches: 0,
            errors: 0,
            prompt: None,
        });
        let page = fold(&report, &[]);
        let answer = answer.expect("fixture answer").text;
        assert_eq!(page.matches(answer.as_str()).count(), 1, "{page}");
    }
}
