//! Persistent task-head storage and incremental snapshot construction.

use super::*;
/// Maximum changed-head generations retained before one complete view becomes
/// the next immutable base.
const TASK_HEAD_DELTA_CHUNKS_MAX: usize = 32;
/// Small stores may retain this many changed heads without premature churn.
const TASK_HEAD_DELTA_HEADS_MIN: usize = 64;

/// Persistent task heads with one shared complete-board materialization.
///
/// Snapshot clones share both the base/deltas and the generation's lazy cache.
/// A bounded delta count and head count ensure a later complete read never
/// replays process-lifetime history.
#[derive(Debug)]
pub(in crate::runtime) struct TaskHeadStore {
    base: Arc<[journal::TaskHeads]>,
    deltas: Arc<[Arc<[journal::TaskHeads]>]>,
    delta_heads: usize,
    len: usize,
    materialized: Arc<std::sync::OnceLock<Arc<[journal::TaskHeads]>>>,
}

impl TaskHeadStore {
    fn from_base(tasks: Vec<journal::TaskHeads>) -> Self {
        let base: Arc<[journal::TaskHeads]> = Arc::from(tasks);
        let materialized = Arc::new(std::sync::OnceLock::new());
        materialized
            .set(Arc::clone(&base))
            .expect("a new task-head materialization is empty");
        Self {
            len: base.len(),
            base,
            deltas: Arc::from([]),
            delta_heads: 0,
            materialized,
        }
    }

    fn advanced(&self, moved: Vec<journal::TaskHeads>, len: usize) -> Self {
        if moved.is_empty() {
            let mut carried = self.clone();
            carried.len = len;
            return carried;
        }
        let delta_heads = self.delta_heads.saturating_add(moved.len());
        if self.deltas.len() >= TASK_HEAD_DELTA_CHUNKS_MAX
            || delta_heads > self.len.max(TASK_HEAD_DELTA_HEADS_MIN)
        {
            let tasks = merge_task_head_sources([self.tasks(), moved.as_slice()], len);
            return Self::from_base(tasks);
        }
        let mut deltas = self.deltas.to_vec();
        deltas.push(Arc::from(moved));
        Self {
            base: Arc::clone(&self.base),
            deltas: Arc::from(deltas),
            delta_heads,
            len,
            materialized: Arc::new(std::sync::OnceLock::new()),
        }
    }

    const fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn get(&self, task: &journal::crossing::TaskStamp) -> Option<&journal::TaskHeads> {
        fn search<'a>(
            rows: &'a [journal::TaskHeads],
            task: &journal::crossing::TaskStamp,
        ) -> Option<&'a journal::TaskHeads> {
            rows.binary_search_by(|heads| heads.statement().body().born.cmp(task))
                .ok()
                .map(|index| &rows[index])
        }
        if let Some(rows) = self.materialized.get() {
            return search(rows, task);
        }
        self.deltas
            .iter()
            .rev()
            .find_map(|rows| search(rows, task))
            .or_else(|| search(&self.base, task))
    }

    pub(in crate::runtime) fn tasks(&self) -> &[journal::TaskHeads] {
        self.materialized
            .get_or_init(|| {
                Arc::from(merge_task_head_sources(
                    std::iter::once(self.base.as_ref())
                        .chain(self.deltas.iter().map(|delta| delta.as_ref())),
                    self.len,
                ))
            })
            .as_ref()
    }
}

#[derive(Debug)]
struct SortedMergeCursor<'a, T, K> {
    rows: &'a [T],
    position: usize,
    generation: usize,
    key: for<'row> fn(&'row T) -> &'row K,
}

impl<'a, T, K> SortedMergeCursor<'a, T, K> {
    fn current(&self) -> &'a T {
        &self.rows[self.position]
    }

    fn current_key(&self) -> &'a K {
        (self.key)(self.current())
    }
}

impl<T, K: Ord> PartialEq for SortedMergeCursor<'_, T, K> {
    fn eq(&self, other: &Self) -> bool {
        self.current_key() == other.current_key()
            && self.generation == other.generation
            && self.position == other.position
    }
}

impl<T, K: Ord> Eq for SortedMergeCursor<'_, T, K> {}

impl<T, K: Ord> Ord for SortedMergeCursor<'_, T, K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .current_key()
            .cmp(self.current_key())
            .then_with(|| self.generation.cmp(&other.generation))
            .then_with(|| self.position.cmp(&other.position))
    }
}

impl<T, K: Ord> PartialOrd for SortedMergeCursor<'_, T, K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn merge_task_head_sources<'a>(
    sources: impl IntoIterator<Item = &'a [journal::TaskHeads]>,
    expected_len: usize,
) -> Vec<journal::TaskHeads> {
    merge_sorted_sources(sources, expected_len, |heads| {
        &heads.statement().body().born
    })
}

fn merge_sorted_sources<'a, T: Clone + 'a, K: Ord + 'a>(
    sources: impl IntoIterator<Item = &'a [T]>,
    expected_len: usize,
    key: for<'row> fn(&'row T) -> &'row K,
) -> Vec<T> {
    let mut pending = std::collections::BinaryHeap::new();
    for (generation, rows) in sources.into_iter().enumerate() {
        debug_assert!(
            rows.windows(2)
                .all(|pair| { key(&pair[0]) < key(&pair[1]) })
        );
        if !rows.is_empty() {
            pending.push(SortedMergeCursor {
                rows,
                position: 0,
                generation,
                key,
            });
        }
    }

    let mut merged = Vec::with_capacity(expected_len);
    while let Some(cursor) = pending.pop() {
        let current_key = cursor.current_key();
        let mut winner = cursor.current();
        let mut winner_generation = cursor.generation;
        push_next_sorted(&mut pending, cursor);
        while pending
            .peek()
            .is_some_and(|next| next.current_key() == current_key)
        {
            let duplicate = pending.pop().expect("the matching cursor is present");
            if duplicate.generation > winner_generation {
                winner = duplicate.current();
                winner_generation = duplicate.generation;
            }
            push_next_sorted(&mut pending, duplicate);
        }
        merged.push(winner.clone());
    }
    debug_assert_eq!(merged.len(), expected_len);
    merged
}

fn push_next_sorted<'a, T, K: Ord>(
    pending: &mut std::collections::BinaryHeap<SortedMergeCursor<'a, T, K>>,
    mut cursor: SortedMergeCursor<'a, T, K>,
) {
    cursor.position += 1;
    if cursor.position < cursor.rows.len() {
        pending.push(cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::merge_sorted_sources;

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Row {
        key: u8,
        value: &'static str,
    }

    #[test]
    fn sorted_merge_replaces_primed_rows_with_the_newest_generation() {
        let base = [
            Row {
                key: 2,
                value: "base two",
            },
            Row {
                key: 4,
                value: "base four",
            },
        ];
        let older_delta = [
            Row {
                key: 1,
                value: "older one",
            },
            Row {
                key: 3,
                value: "older three",
            },
        ];
        let newest_delta = [
            Row {
                key: 1,
                value: "newest one",
            },
            Row {
                key: 4,
                value: "newest four",
            },
        ];

        let primed = merge_sorted_sources([base.as_slice(), &older_delta], 4, |row| &row.key);
        assert_eq!(
            primed
                .iter()
                .map(|row| (row.key, row.value))
                .collect::<Vec<_>>(),
            [
                (1, "older one"),
                (2, "base two"),
                (3, "older three"),
                (4, "base four"),
            ]
        );

        let merged = merge_sorted_sources([primed.as_slice(), &newest_delta], 4, |row| &row.key);
        assert_eq!(
            merged,
            [
                Row {
                    key: 1,
                    value: "newest one",
                },
                Row {
                    key: 2,
                    value: "base two",
                },
                Row {
                    key: 3,
                    value: "older three",
                },
                Row {
                    key: 4,
                    value: "newest four",
                },
            ]
        );
    }
}

impl Clone for TaskHeadStore {
    fn clone(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            deltas: Arc::clone(&self.deltas),
            delta_heads: self.delta_heads,
            len: self.len,
            materialized: Arc::clone(&self.materialized),
        }
    }
}

impl PartialEq for TaskHeadStore {
    fn eq(&self, other: &Self) -> bool {
        self.tasks() == other.tasks()
    }
}

impl Eq for TaskHeadStore {}

#[derive(Default)]
pub(in crate::runtime) enum TaskSnapshotState {
    #[default]
    Empty,
    Building(TaskSnapshotBuild),
    Ready(TaskSnapshot),
    Fault(String),
}

impl TaskSnapshotState {
    pub(in crate::runtime) const fn needs_step(&self) -> bool {
        matches!(self, Self::Empty | Self::Building { .. })
    }

    pub(in crate::runtime) fn current(&self) -> Result<TaskSnapshot, BusReadError> {
        match self {
            Self::Ready(snapshot) => Ok(snapshot.clone()),
            Self::Fault(detail) => Err(BusReadError::Head(GuardError::Invalid(detail.clone()))),
            Self::Empty => Err(pending_error(None)),
            Self::Building(build) => Err(pending_error(build.facts.horizon())),
        }
    }
}

pub(in crate::runtime) struct TaskSnapshotBuild {
    facts: TaskFacts,
    cursor: Option<journal::crossing::Seq>,
    tasks: Vec<journal::TaskHeads>,
    task_after: Option<journal::crossing::TaskStamp>,
    /// Last ready generation. Changed task bundles are applied to its
    /// persistent head store and compact allocation indexes.
    prior: Option<TaskSnapshot>,
    trust: Vec<JournalHead<journal::crossing::TrustHead>>,
    trust_after: Option<journal::crossing::Digest>,
    stage: SnapshotStage,
    steps: u64,
}

#[derive(Clone, Copy)]
enum SnapshotStage {
    Facts,
    Tasks,
    Trust,
}

impl TaskSnapshotBuild {
    fn new(facts: TaskFacts) -> Self {
        Self {
            facts,
            cursor: None,
            tasks: Vec::new(),
            task_after: None,
            prior: None,
            trust: Vec::new(),
            trust_after: None,
            stage: SnapshotStage::Facts,
            steps: 0,
        }
    }
}

pub(in crate::runtime) fn invalidate_task_snapshot(state: &mut TaskSnapshotState) {
    let carried = match state {
        TaskSnapshotState::Ready(snapshot) => Some((
            snapshot.facts.clone(),
            snapshot.cursor.map(|_| snapshot.clone()),
        )),
        TaskSnapshotState::Building(build) => Some((build.facts.clone(), build.prior.clone())),
        TaskSnapshotState::Empty | TaskSnapshotState::Fault(_) => None,
    };
    if let Some((facts, prior)) = carried {
        let mut build = TaskSnapshotBuild::new(facts);
        build.prior = prior;
        *state = TaskSnapshotState::Building(build);
    }
}

pub(in crate::runtime) fn advance_task_snapshot(
    journal: &Journal,
    state: &mut TaskSnapshotState,
) -> Result<(), BusReadError> {
    let mut build = match std::mem::take(state) {
        TaskSnapshotState::Empty => TaskSnapshotBuild::new(TaskFacts::default()),
        TaskSnapshotState::Building(build) => build,
        ready @ TaskSnapshotState::Ready(_) => {
            *state = ready;
            return Ok(());
        }
        TaskSnapshotState::Fault(detail) => {
            *state = TaskSnapshotState::Fault(detail.clone());
            return Err(BusReadError::Head(GuardError::Invalid(detail.clone())));
        }
    };
    build.steps = build.steps.saturating_add(1);
    let step = match build.stage {
        SnapshotStage::Facts => advance_facts(journal, build),
        SnapshotStage::Tasks => advance_tasks(journal, build),
        SnapshotStage::Trust => advance_trust(journal, build),
    }
    .map_err(map_read_error);
    match step {
        Ok(BuildStep::Pending(build)) => {
            *state = TaskSnapshotState::Building(build);
            Ok(())
        }
        Ok(BuildStep::Ready(snapshot)) => {
            *state = TaskSnapshotState::Ready(snapshot);
            Ok(())
        }
        Err(error) => {
            *state = TaskSnapshotState::Fault(error.to_string());
            Err(error)
        }
    }
}

fn advance_facts(
    journal: &Journal,
    mut build: TaskSnapshotBuild,
) -> Result<BuildStep, journal::ReadError<BusReadError>> {
    journal.read_with(|read| {
        let page = read
            .selected_fact_page(
                &resident_authority_kinds(),
                build.facts.horizon(),
                None,
                journal::FactReadBudget::RETAINED,
            )
            .map_err(BusReadError::Head)?;
        if let Some(horizon) = page.continuation.or(page.horizon) {
            build.facts = build
                .facts
                .append_selected(page.rows, horizon)
                .map_err(BusReadError::Head)?;
        }
        if page.continuation.is_none() {
            build.cursor = page.horizon;
            build.stage = SnapshotStage::Tasks;
        }
        Ok(BuildStep::Pending(build))
    })
}

fn advance_tasks(
    journal: &Journal,
    mut build: TaskSnapshotBuild,
) -> Result<BuildStep, journal::ReadError<BusReadError>> {
    journal.read_with(|read| {
        if let Some(prior) = &build.prior {
            if read.cursor().map_err(BusReadError::Head)? != build.cursor {
                return Err(BusReadError::Head(GuardError::Invalid(
                    "task snapshot authority moved between bounded pages".to_owned(),
                )));
            }
            let since = prior
                .cursor()
                .expect("a carried task snapshot has a durable cursor");
            if let Some(cursor) = build.cursor.filter(|cursor| *cursor > since) {
                let first = journal::crossing::Seq::new(since.get() + 1)
                    .expect("a position past a position is not zero");
                build.tasks = read
                    .changed_task_heads(first, cursor)
                    .map_err(BusReadError::Head)?;
            }
            build.stage = SnapshotStage::Trust;
            return Ok(BuildStep::Pending(build));
        }
        let page = read
            .task_head_page(build.task_after.as_ref(), journal::FactReadBudget::RETAINED)
            .map_err(BusReadError::Head)?;
        require_cursor(page.witness, build.cursor)?;
        build.tasks.extend(page.rows);
        build.task_after = page.continuation;
        if build.task_after.is_none() {
            build.stage = SnapshotStage::Trust;
        }
        Ok(BuildStep::Pending(build))
    })
}

fn advance_trust(
    journal: &Journal,
    mut build: TaskSnapshotBuild,
) -> Result<BuildStep, journal::ReadError<BusReadError>> {
    journal.read_with(|read| {
        let page = read
            .trust_head_page(
                build.trust_after.as_ref(),
                journal::FactReadBudget::RETAINED,
            )
            .map_err(BusReadError::Head)?;
        require_cursor(page.witness, build.cursor)?;
        build.trust.extend(page.rows);
        build.trust_after = page.continuation;
        if build.trust_after.is_some() {
            return Ok(BuildStep::Pending(build));
        }
        let residency = read.residency_head().map_err(BusReadError::Head)?;
        let oops_generation = read.oops_generation().map_err(BusReadError::Head)?;
        let task_projection_cursor = read.task_projection_cursor().map_err(BusReadError::Head)?;
        if read.cursor().map_err(BusReadError::Head)? != build.cursor {
            return Err(BusReadError::Head(GuardError::Invalid(
                "task snapshot authority moved during completion".to_owned(),
            )));
        }
        let task_head_update_visits = build.tasks.len();
        let task_count = build.prior.as_ref().map_or(build.tasks.len(), |prior| {
            let since = prior
                .cursor()
                .expect("a carried task snapshot has a durable cursor");
            prior.tasks.len()
                + build
                    .tasks
                    .iter()
                    .filter(|heads| heads.statement().body().topic_origin > since)
                    .count()
        });
        let allocation = match &build.prior {
            Some(prior) => advance_allocation_index(read, prior, &build.tasks),
            None => rebuild_allocation_index(&build.tasks),
        }
        .map_err(BusReadError::Head)?;
        build.facts.retain_allocation_origins(&allocation.frontier);
        let mut topic_inputs = build
            .prior
            .as_ref()
            .map(|prior| prior.topic_inputs.clone())
            .unwrap_or_default();
        for heads in &build.tasks {
            topic_inputs.update(heads);
        }
        let tasks = match &build.prior {
            Some(prior) => prior
                .tasks
                .advanced(std::mem::take(&mut build.tasks), task_count),
            None => TaskHeadStore::from_base(std::mem::take(&mut build.tasks)),
        };
        let described_topics = descriptions(read, &build)?;
        Ok(BuildStep::Ready(TaskSnapshot {
            cursor: build.cursor,
            task_projection_cursor,
            facts: build.facts,
            tasks,
            topic_inputs,
            described_topics,
            allocation_frontier: allocation.frontier,
            dependency_standing: allocation.standing,
            dependency_references: allocation.references,
            allocation_faults: allocation.faults,
            trust: Arc::from(build.trust),
            residency,
            oops_generation,
            materialization_steps: build.steps,
            task_head_update_visits,
        }))
    })
}

fn descriptions(
    read: &journal::JournalRead<'_>,
    build: &TaskSnapshotBuild,
) -> Result<Arc<[journal::TopicRoutingNode]>, BusReadError> {
    use journal::crossing::FactKind::{TopicDescribed, TopicMoved};
    if let Some(prior) = &build.prior
        && !build
            .facts
            .after(prior.cursor())
            .any(|row| matches!(row.fact.kind(), TopicDescribed | TopicMoved))
    {
        return Ok(Arc::clone(prior.described_topics()));
    }
    let mut nodes = read.described_topics().map_err(BusReadError::Head)?;
    nodes.retain(|node| !node.description.trim().is_empty());
    Ok(nodes.into())
}

enum BuildStep {
    Pending(TaskSnapshotBuild),
    Ready(TaskSnapshot),
}

fn require_cursor(
    witness: journal::CurrentHeadWitness,
    expected: Option<journal::crossing::Seq>,
) -> Result<(), BusReadError> {
    (witness.cursor == expected).then_some(()).ok_or_else(|| {
        BusReadError::Head(GuardError::Invalid(
            "task snapshot authority moved between bounded pages".to_owned(),
        ))
    })
}

fn pending_error(through: Option<journal::crossing::Seq>) -> BusReadError {
    BusReadError::Head(GuardError::Invalid(format!(
        "task snapshot materialization is pending after {through:?}"
    )))
}

/// Facts consumed by resident decisions and bounded presentation recovery.
///
/// Transcript ingestion is high-volume accounting, not resident authority.
/// Its consumers use the origin-and-kind index directly, so the shared
/// authority cache never replays it. Keeping every other registered kind here
/// preserves the existing TaskFacts contract as the vocabulary grows.
fn resident_authority_kinds() -> Vec<journal::crossing::FactKind> {
    journal::crossing::FactKind::ALL
        .iter()
        .copied()
        .filter(|kind| *kind != journal::crossing::FactKind::JournalIngested)
        .collect()
}

fn map_read_error(error: journal::ReadError<BusReadError>) -> BusReadError {
    match error {
        journal::ReadError::Journal(error) => BusReadError::Head(error),
        journal::ReadError::Answer(error) => error,
    }
}
