//! Resident-owned derived state: one tree over the newest eligible public
//! statements on the board, wherever they were filed. The journal remains the
//! only authority for current statements and router policy.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use bus::TaskSnapshot;
use topic::{CAPACITY, kept::Rolling};
use wire::topic::{GroupingFreshness, GroupingRebuild, GroupingRecovery, TopicMoved};
use wire::{CanonicalUuid, FactKind, Seq, TaskStamp};

use super::{CommandAnswer, Outcome, prepare, read_guard};

mod admission;
mod worker;

// Replay budget, distinct from the number of seats held in memory.
const LOOKBACK_LIMIT: usize = 1024;

#[derive(Default)]
pub(crate) struct TopicTree {
    published: Mutex<Option<Arc<Generation>>>,
    wake: std::sync::OnceLock<std::sync::mpsc::SyncSender<worker::Wake>>,
}

/// One complete tree and the journal position it was grown to.
#[derive(Clone)]
pub(in crate::board_command) struct Generation {
    lineage: CanonicalUuid,
    projection: Option<Seq>,
    through: Option<Seq>,
    tree: Arc<Rolling>,
    pub(super) fixed: Arc<[journal::TopicRoutingNode]>,
    pub(super) window: Arc<VecDeque<WindowHead>>,
    pub(super) inputs: bus::TopicInputs,
    recovery: GroupingRecovery,
}

#[derive(Clone)]
pub(super) struct WindowHead {
    pub(super) id: TaskStamp,
    origin: Seq,
    pub(super) revision: Seq,
}

static OPEN: LazyLock<Mutex<BTreeMap<PathBuf, Weak<TopicTree>>>> = LazyLock::new(Default::default);

impl TopicTree {
    /// Commands in the same journal namespace borrow the resident's live tree.
    /// Weak entries never keep a stopped resident or a test fixture alive.
    pub(crate) fn shared(directory: PathBuf) -> Arc<Self> {
        let mut open = OPEN.lock().expect("topic tree registry lock");
        if let Some(tree) = open.get(&directory).and_then(Weak::upgrade) {
            return tree;
        }
        open.retain(|_, reference| reference.strong_count() != 0);
        let tree = Arc::new(Self::default());
        open.insert(directory, Arc::downgrade(&tree));
        tree
    }

    pub(in crate::board_command) fn for_route(directory: &Path) -> Option<Arc<Self>> {
        OPEN.lock()
            .expect("topic tree registry lock")
            .get(directory)?
            .upgrade()
    }

    pub(in crate::board_command) fn with_current<T>(
        &self,
        observed: &TaskSnapshot,
        read: impl FnOnce(&Generation) -> T,
    ) -> Result<T, Outcome<CommandAnswer>> {
        let state = self.published.lock().expect("topic tree lock").clone();
        let state = state.ok_or_else(|| {
            self.signal(worker::Wake::Cold);
            read_guard::refusal(
                "topic::preview-catching-up",
                None,
                "no complete tree is available yet; choose a topic or ask again shortly",
            )
        })?;
        if state.admit_read(observed).is_ok() {
            return Ok(read(&state));
        }

        // The command already holds the exact journal view it must decide
        // against. Advance from that view instead of making the caller poll
        // while the background writer batches unrelated commits.
        self.update(observed);
        let state = self
            .published
            .lock()
            .expect("topic tree lock")
            .clone()
            .expect("an existing topic generation remains published");
        state.admit_read(observed)?;
        Ok(read(&state))
    }
}

impl Generation {
    fn build(observed: &TaskSnapshot, recovery: GroupingRecovery) -> Self {
        let mut state = Self {
            lineage: CanonicalUuid::new(uuid::Uuid::new_v4()),
            projection: observed.task_projection_cursor(),
            through: observed.cursor(),
            tree: Arc::new(Rolling::with_fixed(observed.described_topics().iter().map(
                |node| {
                    topic::evidence::prepare_fields(
                        node.path.as_str(),
                        [&[], &[], std::slice::from_ref(&node.description), &[]],
                    )
                },
            ))),
            fixed: Arc::clone(observed.described_topics()),
            window: Arc::new(VecDeque::with_capacity(CAPACITY)),
            inputs: observed.topic_inputs().clone(),
            recovery,
        };
        for id in observed.topic_inputs().newest(LOOKBACK_LIMIT) {
            state.push(
                observed
                    .task_heads(id)
                    .expect("indexed topic input")
                    .statement()
                    .body(),
            );
        }
        state
    }

    /// Continue to the observed journal, starting a new lineage in place
    /// when this one cannot continue.
    fn advance(&mut self, observed: &TaskSnapshot) {
        if let Err(recovery) = self.carry(observed) {
            *self = Self::build(observed, recovery);
        }
    }

    /// Catch-up can only append rows still recoverable in their original
    /// order. A held task that stopped being eligible, or whose text the
    /// engine reads changed, requires a new lineage: the error says why, and
    /// the state it leaves is spent.
    fn carry(&mut self, observed: &TaskSnapshot) -> Result<(), GroupingRecovery> {
        if !Arc::ptr_eq(&self.fixed, observed.described_topics()) {
            return Err(changed());
        }
        let projection = observed.task_projection_cursor();
        let after = self.through;
        self.through = observed.cursor();
        if self.projection == projection {
            return Ok(());
        }
        let mut touched = BTreeMap::new();
        for row in observed.facts().after(after) {
            if Some(row.seq) > projection {
                break;
            }
            // A moved subtree can carry tasks into or out of public view.
            if row.fact.read::<TopicMoved>().is_some() {
                return Err(changed());
            }
            for id in row.fact.mutated_tasks() {
                let revised = touched.entry(id).or_insert(false);
                *revised |= row.fact.kind() == FactKind::TaskEdited;
            }
        }

        let mut additions = BTreeMap::new();
        for (id, revised) in touched {
            let heads = observed.task_heads(id);
            let head = heads
                .filter(|_| observed.topic_inputs().member(id).is_some())
                .map(|h| h.statement().body());
            if let Some(seat) = self.window.iter().position(|held| &held.id == id) {
                let Some(head) = head.filter(|head| self.holds(seat, head)) else {
                    return Err(changed());
                };
                // The statement revision moves on every rewrite of the head,
                // including ones the engine does not read.
                if self.window[seat].revision != head.revision {
                    Arc::make_mut(&mut self.window)[seat].revision = head.revision;
                }
            }
            let before = self.inputs.member(id).is_some();
            // A missed arrival that was activated and withdrawn cannot be
            // reproduced from current heads. Do not roll past it as though
            // it had never occupied a seat.
            if !before
                && head.is_none()
                && heads.is_some_and(|heads| {
                    let task = heads.statement().body();
                    Some(task.topic_origin) > after
                        && heads.workflow().body().is_activated()
                        && task.topic.path.as_ref().is_some_and(|path| {
                            path.private_owner().is_none() && !wire::is_oops_topic(path)
                        })
                })
            {
                return Err(changed());
            }
            if let Some(head) = head.filter(|_| !before) {
                if revised
                    || self
                        .window
                        .back()
                        .is_some_and(|last| head.topic_origin <= last.origin)
                {
                    return Err(changed());
                }
                additions.insert(head.topic_origin, head);
                if additions.len() > LOOKBACK_LIMIT {
                    return Err(GroupingRecovery::Rebuilt {
                        reason: GroupingRebuild::CatchUpLimit,
                    });
                }
            }
        }
        for head in additions.into_values() {
            self.push(head);
        }
        self.inputs = observed.topic_inputs().clone();
        self.projection = projection;
        Ok(())
    }

    /// Whether the tree's row for this seat is still what the engine reads
    /// from the task's current statement.
    fn holds(&self, seat: usize, head: &wire::TaskHead) -> bool {
        let held = &self.window[seat];
        head.revision == held.revision
            || (head.topic_origin == held.origin
                && self.tree.rows()[self.fixed.len() + seat] == prepare(head))
    }

    fn push(&mut self, head: &wire::TaskHead) {
        // Release metadata for the oldest seat before admitting its successor.
        let window = Arc::make_mut(&mut self.window);
        if window.len() == CAPACITY {
            window.pop_front();
        }
        window.push_back(WindowHead {
            id: head.born.clone(),
            origin: head.topic_origin,
            revision: head.revision,
        });
        Arc::make_mut(&mut self.tree).push(prepare(head));
    }

    pub(super) fn tree(&self) -> &Rolling {
        &self.tree
    }

    pub(super) fn freshness(&self, observed: &TaskSnapshot) -> GroupingFreshness {
        GroupingFreshness {
            lineage: self.lineage,
            projection: self.projection,
            checked_against: self.lag(observed).map(|lag| lag.source),
            oldest_origin: self.window.front().map(|head| head.origin),
            capacity: CAPACITY,
            catch_up_limit: LOOKBACK_LIMIT,
            recovery: self.recovery,
        }
    }
}

fn changed() -> GroupingRecovery {
    GroupingRecovery::Rebuilt {
        reason: GroupingRebuild::InputChanged,
    }
}
