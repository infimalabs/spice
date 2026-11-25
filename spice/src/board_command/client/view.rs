//! The `board` leaf: a compact posture by default, a bounded family page, and
//! the complete board document only as an exact file export.
//!
//! In-process callers keep reading the whole board through `current_board`;
//! this module only projects that one snapshot for a person or an agent. Every
//! row is ranked by one standing law so the posture and a family page agree,
//! and every partial page names the exact command that continues it.

use std::collections::BTreeMap;

use serde::Serialize;
use wire::output::{Admission, Bounded, ClippedText, Export, Limits, Request, Resume, Surface};
use wire::{CanonicalUuid, SpiceCommand};

use super::*;

/// Board rows: ten by default, short titles, and maxima well under the
/// repository ceiling.
pub(crate) const BOARD: Surface = Surface::new(
    Limits {
        rows: 10,
        bytes: 8_192,
        text: 120,
    },
    Limits {
        rows: 200,
        bytes: 131_072,
        text: 4_096,
    },
);

/// The file a continuation names when one row alone exceeds the byte maximum.
const EXPORT_FILE: &str = "board.json";

/// One `board` invocation's answer.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum BoardView {
    Posture(Box<Posture>),
    Family(Box<FamilyPage>),
    Export(ExportReceipt),
}

impl BoardView {
    /// The text form of this answer.
    pub(crate) fn text(&self) -> Option<&str> {
        Some(match self {
            Self::Posture(posture) => &posture.text,
            Self::Family(page) => &page.text,
            Self::Export(export) => &export.text,
        })
    }
}

/// Where a task stands for the reading actor; earlier standings rank first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Standing {
    /// The reading actor's own active claim.
    Claimed,
    /// Another actor's active claim.
    Active,
    /// Open work the allocator could hand the reading actor.
    Ready,
    /// Unclaimed review-phase work.
    Review,
    /// A Design whose completed pass waits on the operator's decision.
    AwaitingDecision,
    /// A claim whose lease lapsed.
    Stale,
    /// Execution work whose unresolved gate has no advancing path.
    Blocked,
    /// Normal Plan/Design control or sequencing behind advancing work.
    Workflow,
    /// Deferred until a scheduled time.
    Deferred,
    /// Open but not workable by the reading actor.
    Pending,
}

impl Standing {
    const fn label(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Active => "active",
            Self::Ready => "ready",
            Self::Review => "review",
            Self::AwaitingDecision => "awaiting-decision",
            Self::Stale => "stale",
            Self::Blocked => "blocked",
            Self::Workflow => "waiting",
            Self::Deferred => "deferred",
            Self::Pending => "pending",
        }
    }
}

/// One task as a board page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BoardRow {
    pub(crate) handle: TaskHandle,
    pub(crate) standing: Standing,
    pub(crate) phase: String,
    /// An open Design's pass-cycle state, as the resident judged it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cycle: Option<wire::rooms::DesignCycle>,
    /// Absent while the task is unplaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) topic: Option<wire::Topic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ownership: Option<wire::intent::TaskOwnership>,
    /// Lanes holding an active claim on the current round.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) lanes: Vec<CanonicalUuid>,
    /// Participants the current phase needs, when more than one.
    #[serde(skip_serializing_if = "is_one")]
    pub(crate) target: u32,
    /// Participants that already completed the current round.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) arrived: usize,
    pub(crate) title: ClippedText,
    /// One plain sentence saying who can do what once the task lands and why,
    /// whole: an admitted pitch is one bounded line.
    pub(crate) pitch: String,
}

impl std::fmt::Display for BoardRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}  {:<7}  {}",
            self.handle,
            self.standing.label(),
            self.phase
        )?;
        if let Some(cycle) = self.cycle {
            write!(formatter, " ({})", cycle.label())?;
        }
        if self.target > 1 {
            write!(
                formatter,
                " [{}/{}]",
                self.lanes.len() + self.arrived,
                self.target
            )?;
        }
        if let Some(ownership) = &self.ownership {
            write!(formatter, " [")?;
            write_ownership(formatter, ownership)?;
            write!(formatter, "]")?;
        }
        write!(
            formatter,
            "  {}  {}\n  {}",
            wire::topic_label(self.topic.as_ref()),
            self.title,
            self.pitch
        )
    }
}

fn is_one(target: &u32) -> bool {
    *target == 1
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// Rows of each standing among the actor's routed open work.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PostureCounts {
    /// Open tasks on the reader's route.
    pub(crate) open: usize,
    pub(crate) active: usize,
    pub(crate) ready: usize,
    pub(crate) review: usize,
    pub(crate) stale: usize,
    pub(crate) blocked: usize,
    pub(crate) workflow_waiting: usize,
    pub(crate) deferred: usize,
    pub(crate) awaiting_decision: usize,
    pub(crate) pending: usize,
    /// Unfiled Drafts on the reader's route.
    pub(crate) drafts: usize,
}

/// Bare `board`: counts, the reader's own claim, and its most relevant work.
#[derive(Debug, Serialize)]
pub(crate) struct Posture {
    pub(crate) principal: Actor,
    pub(crate) counts: PostureCounts,
    /// The reader's own claim, outside any row budget so it is never withheld.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) claim: Option<BoardRow>,
    /// Ready, review, then blocked work on the reader's route, allocator order
    /// first.
    pub(crate) work: Bounded<BoardRow>,
    #[serde(skip)]
    text: String,
}

/// `board FAMILY`: one family's open tasks, most relevant first.
#[derive(Debug, Serialize)]
pub(crate) struct FamilyPage {
    pub(crate) family: String,
    pub(crate) tasks: Bounded<BoardRow>,
    #[serde(skip)]
    text: String,
}

/// An explicit `--output FILE` receipt.
#[derive(Debug, Serialize)]
pub(crate) struct ExportReceipt {
    #[serde(flatten)]
    pub(crate) export: Export,
    #[serde(skip)]
    text: String,
}

impl ExportReceipt {
    /// The receipt line.
    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

/// The parsed `board` flags.
pub(crate) struct ViewRequest {
    family: Option<String>,
    budget: wire::output::Budget,
    request: Request,
    after: Option<TaskHandle>,
    output: Option<String>,
}

/// Execute one `board` invocation.
pub(crate) fn board_view(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<BoardView> {
    let request = match ViewRequest::from_matches(matches) {
        Ok(request) => request,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let repository = match discover_repository() {
        Ok(repository) => repository,
        Err(outcome) => return crate::typed_outcome(outcome),
    };
    let repository = Host {
        repository: &repository,
        ambient,
    };
    // An export carries the complete board exactly as the resident answers
    // it; a view reads only the open board and the evidence it is judged by.
    let read = match &request.output {
        Some(_) => BoardRead::All {
            family: request.family.clone(),
        },
        None => OPEN_BOARD,
    };
    let (position, board) = match crate::command_client::execute_typed(
        repository,
        contract,
        CommandKind::BoardRead,
        Vec::new(),
        &read,
    ) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Board(board),
        }) => (position, board),
        Outcome::Reply(_) => {
            return protocol_fault(
                "board::answer-invalid",
                "board read returned another answer shape",
            );
        }
        Outcome::Refusal(refusal) => return Outcome::Refusal(refusal),
        Outcome::Fault(fault) => return Outcome::Fault(fault),
    };
    match project_board(&board, &request) {
        Ok(body) => Outcome::Reply(Reply { position, body }),
        Err(outcome) => crate::typed_outcome(outcome),
    }
}

/// Project one board snapshot for one request.
pub(in crate::board_command) fn project_board(
    board: &BoardAnswer,
    request: &ViewRequest,
) -> Result<BoardView, Outcome<CommandAnswer>> {
    if let Some(output) = &request.output {
        return export(board, output).map(BoardView::Export);
    }
    let entries = actor_entries(board)?;
    let standings = Standings::measure(board, &entries);
    match &request.family {
        Some(family) => family_page(board, &entries, &standings, family, request)
            .map(|page| BoardView::Family(Box::new(page))),
        None => posture(board, &entries, &standings, request)
            .map(|posture| BoardView::Posture(Box::new(posture))),
    }
}

impl ViewRequest {
    fn from_matches(matches: &ArgMatches) -> Result<Self, Outcome<CommandAnswer>> {
        let number = |flag: &str| -> Result<Option<usize>, Outcome<CommandAnswer>> {
            matches
                .get_one::<String>(flag)
                .map(|written| {
                    written.parse::<usize>().map_err(|_| {
                        refusal(
                            "board::limit-invalid",
                            Some(written.clone()),
                            format!("--{flag} takes a non-negative integer"),
                        )
                    })
                })
                .transpose()
        };
        let request = Request {
            rows: number("limit")?,
            bytes: number("max-bytes")?,
            text: number("max-text")?,
        };
        let after = matches
            .get_one::<String>("after")
            .map(|written| {
                written.parse::<TaskHandle>().map_err(|error| {
                    refusal(
                        "board::cursor-invalid",
                        Some(written.clone()),
                        error.to_string(),
                    )
                })
            })
            .transpose()?;
        Self::new(
            matches.get_one::<String>("FAMILY").cloned(),
            request,
            after,
            matches.get_one::<String>("output").cloned(),
        )
    }

    pub(in crate::board_command) fn new(
        family: Option<String>,
        request: Request,
        after: Option<TaskHandle>,
        output: Option<String>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        if let Some(family) = &family {
            validate_family(family)?;
        }
        let budget = BOARD
            .budget(request)
            .map_err(|error| refusal("board::limit-invalid", None, error.to_string()))?;
        if output.is_some() && (after.is_some() || request != Request::default()) {
            return Err(refusal(
                "board::export-bounded",
                None,
                "--output writes the complete board; drop --limit, --max-bytes, --max-text, and \
                 --after",
            ));
        }
        Ok(Self {
            family,
            budget,
            request,
            after,
            output,
        })
    }

    /// The command that reads this request again from `cursor`, widened to
    /// `bytes` when the next row alone needs it.
    fn continuation(&self, resume: Resume<'_, BoardRow>) -> SpiceCommand {
        let mut arguments = self.family.iter().cloned().collect::<Vec<_>>();
        let (row, bytes) = match resume {
            Resume::At(row) => (Some(row), self.request.bytes),
            Resume::Widened { record, bytes } => (Some(record), Some(bytes)),
            Resume::Export(_) => (None, None),
        };
        let Some(row) = row else {
            arguments.extend(["--output".to_owned(), EXPORT_FILE.to_owned()]);
            return SpiceCommand::new("board", arguments).expect("board is a command token");
        };
        // A count-only read continues at the default row limit.
        let flags = [
            ("--limit", self.request.rows.filter(|rows| *rows > 0)),
            ("--max-bytes", bytes),
            ("--max-text", self.request.text),
        ];
        for (flag, value) in flags {
            if let Some(value) = value {
                arguments.extend([flag.to_owned(), value.to_string()]);
            }
        }
        arguments.extend(["--after".to_owned(), row.handle.to_string()]);
        SpiceCommand::new("board", arguments).expect("board is a command token")
    }
}

/// Every open task's standing for the reader, and the allocator's rank of
/// the work it could hand the reader.
pub(in crate::board_command) struct Standings {
    standing: BTreeMap<TaskStamp, Standing>,
    rank: BTreeMap<TaskStamp, usize>,
}

impl Standings {
    fn measure(board: &BoardAnswer, entries: &[Entry]) -> Self {
        let satisfied = board::resolved_tasks(entries);
        let environment = board.environment();
        let ownerless = ownerless_tasks(board);
        let standing = entries
            .iter()
            .filter(|entry| entry.workflow().status.is_pending())
            .map(|entry| {
                let standing = standing(entry, board, &environment, &satisfied, &ownerless);
                (entry.born().clone(), standing)
            })
            .collect();
        let rank = board
            .offer
            .iter()
            .cloned()
            .enumerate()
            .map(|(rank, task)| (task, rank))
            .collect();
        Self { standing, rank }
    }

    fn of(&self, entry: &Entry) -> Standing {
        self.standing[entry.born()]
    }

    /// Standing first; within it, the allocator's order, then the oldest task.
    fn order(&self, left: &Entry, right: &Entry) -> std::cmp::Ordering {
        let rank = |entry: &Entry| self.rank.get(entry.born()).copied().unwrap_or(usize::MAX);
        self.of(left)
            .cmp(&self.of(right))
            .then_with(|| rank(left).cmp(&rank(right)))
            .then_with(|| left.born().cmp(right.born()))
    }
}

fn standing(
    entry: &Entry,
    answer: &BoardAnswer,
    environment: &wire::Environment,
    satisfied: &board::ResolvedTasks,
    ownerless: &std::collections::BTreeSet<&wire::TaskStamp>,
) -> Standing {
    let workflow = entry.workflow();
    if entry.claim_of(&answer.principal).is_some() {
        return Standing::Claimed;
    }
    if entry.is_held() {
        return Standing::Active;
    }
    if ownerless.contains(entry.born()) {
        return Standing::Blocked;
    }
    if entry.is_lapsed() {
        return Standing::Stale;
    }
    if workflow.deferral.is_some() {
        Standing::Deferred
    } else if answer.awaiting_selection(entry.born()) {
        Standing::AwaitingDecision
    } else if answer.workflow_waiting.contains(entry.born()) {
        Standing::Workflow
    } else if answer.blocked.contains(entry.born()) {
        Standing::Blocked
    } else if workflow.phase.as_str() == "review" {
        Standing::Review
    } else if board::eligible_entry(satisfied, entry, &answer.principal, environment) {
        Standing::Ready
    } else {
        Standing::Pending
    }
}

fn row(
    board: &BoardAnswer,
    entry: &Entry,
    standing: Standing,
    budget: &wire::output::Budget,
) -> BoardRow {
    let task = board
        .tasks
        .iter()
        .find(|task| &task.statement.body.born == entry.born());
    BoardRow {
        handle: entry.handle().clone(),
        standing,
        phase: entry.workflow().phase.as_str().to_owned(),
        cycle: board.design_cycles.get(entry.born()).copied(),
        topic: entry.topic().cloned(),
        ownership: task.and_then(|task| task.ownership.clone()),
        lanes: entry
            .active_claims()
            .map(|held| held.claim().lane)
            .collect(),
        target: entry.completion().target(&entry.workflow().phase).get(),
        arrived: entry.completion().arrivals.len(),
        title: budget.clip(task.map_or("", |task| task.presentation_title.as_str())),
        pitch: entry.statement().pitch.clone(),
    }
}

/// Offer `ordered` from the cursor on, refusing a cursor the order lacks.
fn admit(
    board: &BoardAnswer,
    standings: &Standings,
    ordered: &[&Entry],
    request: &ViewRequest,
) -> Result<Bounded<BoardRow>, Outcome<CommandAnswer>> {
    let start = match &request.after {
        None => 0,
        Some(after) => ordered
            .iter()
            .position(|entry| entry.handle() == after)
            .ok_or_else(|| {
                refusal(
                    "board::cursor-unknown",
                    Some(after.to_string()),
                    "the cursor task left this page's order; rerun without --after",
                )
            })?,
    };
    let mut admission = Admission::new(request.budget);
    for (index, entry) in ordered[start..].iter().enumerate() {
        if admission.is_closed() {
            admission.withhold(ordered.len() - start - index);
            break;
        }
        let row = row(board, entry, standings.of(entry), &request.budget);
        admission
            .offer(row)
            .map_err(|error| protocol_fault("board::row-unencodable", error.to_string()))?;
    }
    Ok(admission.finish(|resume| request.continuation(resume)))
}

fn posture(
    board: &BoardAnswer,
    entries: &[Entry],
    standings: &Standings,
    request: &ViewRequest,
) -> Result<Posture, Outcome<CommandAnswer>> {
    let rows = actor_rows(board, entries);
    let mut counts = PostureCounts {
        open: rows.len(),
        drafts: actor_drafts(board, entries),
        ..PostureCounts::default()
    };
    let mut claim = None;
    let mut work = Vec::new();
    for entry in &rows {
        let standing = standings.of(entry);
        match standing {
            Standing::Claimed => claim = Some(row(board, entry, standing, &request.budget)),
            Standing::Active => counts.active += 1,
            Standing::Ready => counts.ready += 1,
            Standing::Review => counts.review += 1,
            Standing::Stale => counts.stale += 1,
            Standing::Blocked => counts.blocked += 1,
            Standing::Workflow => counts.workflow_waiting += 1,
            Standing::Deferred => counts.deferred += 1,
            Standing::AwaitingDecision => counts.awaiting_decision += 1,
            Standing::Pending => counts.pending += 1,
        }
        if matches!(
            standing,
            Standing::Ready
                | Standing::Review
                | Standing::AwaitingDecision
                | Standing::Workflow
                | Standing::Blocked
        ) {
            work.push(*entry);
        }
    }
    work.sort_by(|left, right| standings.order(left, right));
    let work = admit(board, standings, &work, request)?;
    let mut text = format!(
        "board for {} at cursor {}: {} open on this route\n\
         active {} ready {} review {} awaiting-decision {} blocked {} waiting {} deferred {} stale {} \
         pending {} drafts {}\n\
         claim ",
        board.principal,
        board.cursor,
        counts.open,
        counts.active,
        counts.ready,
        counts.review,
        counts.awaiting_decision,
        counts.blocked,
        counts.workflow_waiting,
        counts.deferred,
        counts.stale,
        counts.pending,
        counts.drafts,
    );
    match &claim {
        Some(claim) => text.push_str(&claim.to_string()),
        None => text.push('-'),
    }
    push_rows(&mut text, &work);
    Ok(Posture {
        principal: board.principal.clone(),
        counts,
        claim,
        work,
        text,
    })
}

fn family_page(
    board: &BoardAnswer,
    entries: &[Entry],
    standings: &Standings,
    family: &str,
    request: &ViewRequest,
) -> Result<FamilyPage, Outcome<CommandAnswer>> {
    let mut members = entries
        .iter()
        .filter(|entry| entry.workflow().status.is_pending() && in_family(entry.handle(), family))
        .collect::<Vec<_>>();
    members.sort_by(|left, right| standings.order(left, right));
    let tasks = admit(board, standings, &members, request)?;
    let mut text = format!("board {family}: {} tasks", members.len());
    push_rows(&mut text, &tasks);
    Ok(FamilyPage {
        family: family.to_owned(),
        tasks,
        text,
    })
}

fn push_rows(text: &mut String, page: &Bounded<BoardRow>) {
    for row in &page.rows {
        text.push('\n');
        text.push_str(&row.to_string());
    }
    let topics = page
        .rows
        .iter()
        .filter(|row| row.standing == Standing::Blocked)
        .filter(|row| {
            row.ownership
                .as_ref()
                .is_some_and(|owners| owners.teams.is_empty())
        })
        .map(|row| wire::topic_label(row.topic.as_ref()))
        .collect::<std::collections::BTreeSet<_>>();
    for topic in topics {
        text.push_str(&format!(
            "\ntopic {topic}: no team owns automatic allocation"
        ));
    }
    if let Some(omission) = &page.omission {
        text.push('\n');
        text.push_str(&omission.to_string());
    }
}

fn export(board: &BoardAnswer, output: &str) -> Result<ExportReceipt, Outcome<CommandAnswer>> {
    let bytes = wire::encode(board)
        .map_err(|error| protocol_fault("board::answer-unencodable", error.to_string()))?;
    export_file(output, &bytes)
}

/// Write one complete document to `output` and describe the receipt.
pub(super) fn export_file(
    output: &str,
    bytes: &[u8],
) -> Result<ExportReceipt, Outcome<CommandAnswer>> {
    let export =
        spice::export::export(std::path::Path::new(output), bytes).map_err(Outcome::Refusal)?;
    Ok(ExportReceipt {
        text: export.to_string(),
        export,
    })
}
