//! Sole resident writer, durable commit notifications, and armed timers.

#![forbid(unsafe_code)]

pub mod timer;

#[cfg(not(target_arch = "wasm32"))]
mod runtime {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
    use std::thread::{self, JoinHandle};

    use journal::{
        CommitFault, GuardError, GuardRefusal, Journal, JournalHead, JournalWrite, TaskHeads,
        Verification, VerifyError,
    };

    const REQUEST_CAPACITY: usize = 64;
    const BROADCAST_CAPACITY: usize = 64;
    /// Greatest number of steering keys one presentation request can resolve.
    pub const MAX_STEERING_PRESENTATION_KEYS: usize = 64;
    type FactPage =
        journal::crossing::Packed<journal::crossing::Envelope<journal::crossing::AnyFact>>;

    mod occupant_address;
    pub use occupant_address::{
        AddressProof, AddressRefusal, LaneCodeRefusal, OccupantBirth, journal_seat_addresses,
    };
    use occupant_address::{AddressRead, BirthRecord};
    mod task_snapshot;
    pub use task_snapshot::{
        AllocationFault, AllocationRead, AllocationWork, AuthorshipFault, ClaimAuthor,
        FindingAuthors, TaskFacts, TaskOrigin, TopicInputs, context_opening,
    };
    use task_snapshot::{
        TaskHeadStore, TaskSnapshotState, advance_task_snapshot, invalidate_task_snapshot,
    };
    mod transcript_sessions;
    use transcript_sessions::TranscriptSessionState;
    pub use transcript_sessions::{TranscriptSession, TranscriptSessions};
    mod outbox;
    use outbox::OutboxState;
    pub use outbox::{OutboxQuery, OutboxSubmission};
    mod error;
    pub use error::{BusError, BusReadError, BusVerifyError};
    mod rooms;
    mod steering_presentation;
    pub use steering_presentation::{SteeringPresentation, SteeringPublication};

    /// One successful journal transaction, constructable only by the bus
    /// writer after durability.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct JournalCommit {
        first: u64,
        last: u64,
        cursor: journal::crossing::JournalCursor,
    }

    impl JournalCommit {
        fn from_applied(applied: journal::Applied) -> Self {
            Self::from_range(applied.first(), applied.last())
        }

        fn from_range(first: journal::crossing::Seq, last: journal::crossing::Seq) -> Self {
            Self {
                first: first.get(),
                last: last.get(),
                cursor: journal::crossing::JournalCursor::at(last),
            }
        }

        /// First fact position committed by this transaction.
        #[must_use]
        pub const fn first(&self) -> u64 {
            self.first
        }

        /// Last fact position committed by this transaction.
        #[must_use]
        pub const fn last(&self) -> u64 {
            self.last
        }

        /// Monotone journal cursor made visible by this commit.
        #[must_use]
        pub const fn cursor(&self) -> journal::crossing::JournalCursor {
            self.cursor
        }
    }

    /// Typed result of one submitted guarded transaction.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum CommitOutcome {
        /// Every effect is durable and the cursor may be observed.
        Committed(JournalCommit),
        /// A precondition or proposal shape refused with zero writes.
        Refused(GuardRefusal),
        /// Storage or encoding faulted and rolled the transaction back.
        Fault(CommitFault),
    }

    /// Result and optional durable range from one sole-writer transaction.
    pub struct JournalTransaction<T> {
        result: T,
        committed: Option<(journal::crossing::Seq, journal::crossing::Seq)>,
    }

    impl<T> JournalTransaction<T> {
        /// A transaction that made no durable change.
        pub const fn settled(result: T) -> Self {
            Self {
                result,
                committed: None,
            }
        }

        /// A transaction that committed the exact supplied journal range.
        pub const fn committed(
            result: T,
            first: journal::crossing::Seq,
            last: journal::crossing::Seq,
        ) -> Self {
            Self {
                result,
                committed: Some((first, last)),
            }
        }
    }

    /// Standing of one durable task identity at a task-snapshot barrier.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum DependencyStanding {
        Open,
        Completed,
        Closed,
        Deleted,
    }

    impl DependencyStanding {
        #[must_use]
        pub const fn is_settled(self) -> bool {
            matches!(self, Self::Completed | Self::Closed | Self::Deleted)
        }
    }

    /// One queued, immutable view of the heads needed by board requests.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct TaskSnapshot {
        cursor: Option<journal::crossing::Seq>,
        task_projection_cursor: Option<journal::crossing::Seq>,
        facts: TaskFacts,
        tasks: TaskHeadStore,
        topic_inputs: TopicInputs,
        described_topics: Arc<[journal::TopicRoutingNode]>,
        allocation_frontier: imbl::OrdMap<journal::crossing::TaskStamp, TaskHeads>,
        timer_frontier: imbl::OrdMap<journal::crossing::TaskStamp, TaskHeads>,
        dependency_standing:
            Arc<std::collections::HashMap<journal::crossing::TaskStamp, DependencyStanding>>,
        dependency_references: Arc<std::collections::HashMap<journal::crossing::TaskStamp, usize>>,
        allocation_faults: Arc<[AllocationFault]>,
        trust: Arc<[JournalHead<journal::crossing::TrustHead>]>,
        residency: Option<JournalHead<journal::crossing::ResidencyHead>>,
        oops_generation: Option<JournalHead<journal::crossing::OopsGenerationHead>>,
        materialization_steps: u64,
        task_head_update_visits: usize,
    }

    /// Incremental resident authority and one bounded pending-steering page
    /// observed at one writer-queue barrier.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct SteeringSnapshot {
        authority: TaskSnapshot,
        pending: journal::SteeringPage,
    }

    /// Cursor, trust, and residency heads observed inside one immutable authority snapshot.
    ///
    /// Fact pages are read separately through bounded primary-key ranges at
    /// this cursor, so opening the app never materializes the complete stream.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct AppHeads {
        cursor: Option<journal::crossing::Seq>,
        trust: Arc<[JournalHead<journal::crossing::TrustHead>]>,
        residency: Option<JournalHead<journal::crossing::ResidencyHead>>,
    }

    impl AppHeads {
        /// Last durable position visible to this head capture.
        #[must_use]
        pub const fn cursor(&self) -> Option<journal::crossing::Seq> {
            self.cursor
        }

        /// Current trust heads at this barrier, ordered by digest.
        #[must_use]
        pub fn trust(&self) -> &[JournalHead<journal::crossing::TrustHead>] {
            &self.trust
        }

        /// Current resident and lane authority at the same durable barrier.
        #[must_use]
        pub const fn residency(&self) -> Option<&JournalHead<journal::crossing::ResidencyHead>> {
            self.residency.as_ref()
        }
    }

    impl TaskSnapshot {
        #[must_use]
        pub const fn cursor(&self) -> Option<journal::crossing::Seq> {
            self.cursor
        }

        /// Statement, workflow and topic metadata position at this same snapshot.
        #[must_use]
        pub const fn task_projection_cursor(&self) -> Option<journal::crossing::Seq> {
            self.task_projection_cursor
        }

        /// Resident authority facts captured through the same board barrier.
        ///
        /// High-volume transcript-ingestion accounting is queried separately
        /// by origin and kind instead of being replayed into this cache.
        #[must_use]
        pub const fn facts(&self) -> &TaskFacts {
            &self.facts
        }

        #[must_use]
        pub fn tasks(&self) -> &[TaskHeads] {
            self.tasks.tasks()
        }

        /// Ordered eligible topic inputs at the same barrier as the task heads.
        pub fn topic_inputs(&self) -> &TopicInputs {
            &self.topic_inputs
        }

        /// Shared while no description or topic path changes.
        pub fn described_topics(&self) -> &Arc<[journal::TopicRoutingNode]> {
            &self.described_topics
        }

        /// Heads in the live allocation graph.
        ///
        /// The resident updates this compact slice only from moved heads. A
        /// request therefore visits no unrelated terminal row.
        pub fn allocation_frontier(&self) -> impl ExactSizeIterator<Item = &TaskHeads> {
            self.allocation_frontier.values()
        }

        /// Heads that can own a process timer: active claims and finite
        /// pending deferrals.
        pub fn timer_frontier(&self) -> impl ExactSizeIterator<Item = &TaskHeads> {
            self.timer_frontier.values()
        }

        /// Instrument one bounded next-work traversal.
        #[must_use]
        pub fn allocation_read(&self) -> AllocationRead<'_> {
            AllocationRead::new(self)
        }

        /// One exact current head bundle by durable task identity.
        #[must_use]
        pub fn task_heads(&self, task: &journal::crossing::TaskStamp) -> Option<&TaskHeads> {
            self.tasks.get(task)
        }

        /// Known standing for one dependency identity. `None` means the stamp
        /// is missing from authoritative task heads, not settled.
        #[must_use]
        pub fn dependency_standing(
            &self,
            task: &journal::crossing::TaskStamp,
        ) -> Option<DependencyStanding> {
            self.dependency_standing.get(task).copied()
        }

        /// Inconsistency detected while maintaining the live allocation index.
        #[must_use]
        pub fn allocation_fault(&self) -> Option<&AllocationFault> {
            self.allocation_faults.first()
        }

        /// Current trust heads captured at the same board barrier.
        #[must_use]
        pub fn trust(&self) -> &[JournalHead<journal::crossing::TrustHead>] {
            &self.trust
        }

        /// Closed environment derived from the heads captured at this barrier.
        #[must_use]
        pub fn environment(&self) -> journal::crossing::Environment {
            journal::crossing::Environment::journal_schema(journal::SCHEMA_VERSION)
                .expect("the compiled journal schema is positive")
                .with_trust_heads(self.trust.iter().map(|head| head.body().clone()))
        }

        #[must_use]
        pub fn residency(&self) -> Option<&JournalHead<journal::crossing::ResidencyHead>> {
            self.residency.as_ref()
        }

        /// Current repository OOPS generation at this board barrier.
        #[must_use]
        pub fn oops_generation(
            &self,
        ) -> Option<&JournalHead<journal::crossing::OopsGenerationHead>> {
            self.oops_generation.as_ref()
        }

        /// Number of bounded retained-fact pages applied for this generation.
        #[must_use]
        pub const fn materialization_steps(&self) -> u64 {
            self.materialization_steps
        }

        /// Changed task bundles visited while constructing this generation.
        #[must_use]
        pub const fn task_head_update_visits(&self) -> usize {
            self.task_head_update_visits
        }
    }

    impl SteeringSnapshot {
        /// Incrementally retained facts, residency, and cursor at this barrier.
        #[must_use]
        pub const fn authority(&self) -> &TaskSnapshot {
            &self.authority
        }

        /// Bounded live steering page selected at the same barrier.
        #[must_use]
        pub const fn pending(&self) -> &journal::SteeringPage {
            &self.pending
        }
    }

    /// Receiver for post-durability commit broadcasts.
    #[derive(Debug)]
    pub struct CommitSubscriber {
        receiver: Receiver<JournalCommit>,
    }

    /// Async, bounded cursor broadcast for generation-resident consumers.
    ///
    /// Tokio's broadcast receiver names lag explicitly. A consumer that falls
    /// behind never receives a silently repaired sequence: it observes
    /// [`tokio::sync::broadcast::error::RecvError::Lagged`] and can resume from
    /// its own last admitted cursor.
    #[derive(Debug)]
    pub struct BroadcastSubscriber {
        receiver: tokio::sync::broadcast::Receiver<JournalCommit>,
    }

    impl BroadcastSubscriber {
        /// Wait for the next durable commit or a typed closed/lagged boundary.
        pub async fn recv(
            &mut self,
        ) -> Result<JournalCommit, tokio::sync::broadcast::error::RecvError> {
            self.receiver.recv().await
        }
    }

    /// Cloneable read/broadcast door into the one resident-owned bus.
    ///
    /// Cloning this handle does not create another journal connection or
    /// writer. Every snapshot request is serialized through the same queue as
    /// commits, and every subscription listens to that writer's one bounded
    /// post-durability broadcast.
    #[derive(Clone, Debug)]
    pub struct BusReader {
        requests: SyncSender<Request>,
        broadcasts: tokio::sync::broadcast::Sender<JournalCommit>,
    }

    impl BusReader {
        /// Resolve a transcript author through this writer's incremental session index.
        pub fn transcript_session(
            &self,
            lane: journal::crossing::CanonicalUuid,
            source: journal::crossing::CanonicalUuid,
            member: u64,
            offset: u64,
        ) -> Result<Option<TranscriptSession>, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::TranscriptSession {
                    lane,
                    source,
                    member,
                    offset,
                    answer,
                })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read only the current durable cursor at one queued barrier.
        pub fn cursor(&self) -> Result<Option<journal::crossing::Seq>, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::Cursor { answer }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read the writer-owned incremental task snapshot at one queued barrier.
        pub fn task_snapshot(&self) -> Result<TaskSnapshot, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Tasks { answer })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
        }

        /// Read one bounded retained-kind page through the writer queue.
        pub fn selected_fact_page(
            &self,
            kinds: Vec<journal::crossing::FactKind>,
            after: Option<journal::crossing::Seq>,
            budget: journal::FactReadBudget,
        ) -> Result<journal::SelectedFactPage, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            let transaction = Box::new(move |journal: &mut Journal| {
                let result =
                    journal.read(move |read| read.selected_fact_page(&kinds, after, None, budget));
                let respond = Box::new(move || {
                    let _ = answer.send(result);
                });
                TransactionCompletion {
                    commit: None,
                    respond,
                }
            });
            self.requests
                .send(Request::Transaction { transaction })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read one bounded newest-first retained-kind page through the writer queue.
        pub fn recent_fact_page(
            &self,
            kind: journal::crossing::FactKind,
            before: Option<journal::crossing::Seq>,
            budget: journal::FactReadBudget,
        ) -> Result<journal::SelectedFactPage, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            let transaction = Box::new(move |journal: &mut Journal| {
                let result = journal.read(move |read| read.recent_fact_page(kind, before, budget));
                let respond = Box::new(move || {
                    let _ = answer.send(result);
                });
                TransactionCompletion {
                    commit: None,
                    respond,
                }
            });
            self.requests
                .send(Request::Transaction { transaction })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// The current principal role predicate.
        pub fn principal_roles(
            &self,
        ) -> Result<journal::crossing::principal::PrincipalRoles, BusReadError> {
            self.read(|read| read.principal_roles())
        }

        /// Run one bounded indexed read through the sole journal queue, as
        /// [`Bus::read`] does for the writer's own handle.
        pub fn read<T: Send + 'static>(
            &self,
            read: impl FnOnce(&journal::JournalRead<'_>) -> Result<T, GuardError> + Send + 'static,
        ) -> Result<T, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            let transaction = Box::new(move |journal: &mut Journal| {
                let result = journal.read(read);
                let respond = Box::new(move || {
                    let _ = answer.send(result);
                });
                TransactionCompletion {
                    commit: None,
                    respond,
                }
            });
            self.requests
                .send(Request::Transaction { transaction })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Resolve only the steering keys named by one fixed-size message batch.
        ///
        /// # Errors
        ///
        /// Refuses a batch larger than [`MAX_STEERING_PRESENTATION_KEYS`].
        pub fn steering_presentations(
            &self,
            keys: Vec<String>,
        ) -> Result<Vec<SteeringPresentation>, BusReadError> {
            if keys.len() > MAX_STEERING_PRESENTATION_KEYS {
                return Err(BusReadError::Head(GuardError::Invalid(format!(
                    "steering presentation batch has {} keys, maximum is {MAX_STEERING_PRESENTATION_KEYS}",
                    keys.len()
                ))));
            }
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::SteeringPresentations {
                    keys,
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read one bounded newest-first page from a fact kind and origin.
        pub fn recent_origin_facts(
            &self,
            origin: journal::crossing::Origin,
            kind: journal::crossing::FactKind,
            before: Option<journal::crossing::Seq>,
            budget: journal::FactReadBudget,
        ) -> Result<journal::SelectedFactPage, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::RecentOriginFacts {
                    origin,
                    kind,
                    before,
                    budget,
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Capture the app cursor and trust heads at one queued barrier.
        pub fn app_heads(&self) -> Result<AppHeads, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::AppHeads { answer })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
        }

        /// Read one bounded primary-key fact page through a captured horizon.
        pub fn app_facts(
            &self,
            start: journal::FactPageStart,
            horizon: journal::crossing::Seq,
            budget: journal::FactReadBudget,
        ) -> Result<FactPage, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::AppFacts {
                    start,
                    horizon,
                    budget,
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read the newest lane occupancy facts through a captured horizon.
        pub fn app_occupancy(
            &self,
            horizon: journal::crossing::Seq,
        ) -> Result<FactPage, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::AppOccupancy { horizon, answer }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read one bounded causal page directly through the thread index.
        pub fn app_thread(
            &self,
            position: journal::crossing::Seq,
            budget: journal::FactReadBudget,
        ) -> Result<Option<FactPage>, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::AppThread {
                    position,
                    budget,
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Subscribe to the writer's bounded post-durability cursor broadcast.
        #[must_use]
        pub fn subscribe(&self) -> BroadcastSubscriber {
            BroadcastSubscriber {
                receiver: self.broadcasts.subscribe(),
            }
        }
    }

    impl CommitSubscriber {
        /// Wait for the next durable commit notification.
        pub fn recv(&self) -> Result<JournalCommit, mpsc::RecvError> {
            self.receiver.recv()
        }

        /// Observe a pending notification without waiting.
        pub fn try_recv(&self) -> Result<JournalCommit, TryRecvError> {
            self.receiver.try_recv()
        }
    }

    /// Handle to the resident's bounded writer queue.
    #[derive(Debug)]
    pub struct Bus {
        requests: SyncSender<Request>,
        broadcasts: tokio::sync::broadcast::Sender<JournalCommit>,
        writer: Option<JoinHandle<()>>,
    }

    impl Bus {
        /// Move the only admitted writer connection onto one dedicated thread.
        ///
        /// # Errors
        ///
        /// Returns [`BusError::Start`] when the writer thread cannot be
        /// created.
        pub fn start(journal: Journal) -> Result<Self, BusError> {
            let (requests, receiver) = mpsc::sync_channel(REQUEST_CAPACITY);
            let (broadcasts, _) = tokio::sync::broadcast::channel(BROADCAST_CAPACITY);
            let writer_broadcasts = broadcasts.clone();
            let writer = thread::Builder::new()
                .name("spice-journal-writer".to_owned())
                .spawn(move || run_writer(journal, receiver, writer_broadcasts))
                .map_err(BusError::Start)?;
            Ok(Self {
                requests,
                broadcasts,
                writer: Some(writer),
            })
        }

        /// Clone a read/broadcast door into this exact bus generation.
        #[must_use]
        pub fn reader(&self) -> BusReader {
            BusReader {
                requests: self.requests.clone(),
                broadcasts: self.broadcasts.clone(),
            }
        }

        /// Submit one guarded transaction to the dedicated writer.
        pub fn commit(&self, write: JournalWrite) -> Result<CommitOutcome, BusError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Commit {
                    write: Box::new(write),
                    answer,
                })
                .map_err(|_| BusError::Closed)?;
            receiver.recv().map_err(|_| BusError::Closed)
        }

        /// Run one domain transaction through the sole journal writer.
        pub fn transact<T, F>(&self, transaction: F) -> Result<T, BusError>
        where
            T: Send + 'static,
            F: FnOnce(&mut Journal) -> JournalTransaction<T> + Send + 'static,
        {
            let (answer, receiver) = mpsc::sync_channel(1);
            let transaction = Box::new(move |journal: &mut Journal| {
                let transaction = transaction(journal);
                let commit = transaction
                    .committed
                    .map(|(first, last)| JournalCommit::from_range(first, last));
                let respond = Box::new(move || {
                    let _ = answer.send(transaction.result);
                });
                TransactionCompletion { commit, respond }
            });
            self.requests
                .send(Request::Transaction { transaction })
                .map_err(|_| BusError::Closed)?;
            receiver.recv().map_err(|_| BusError::Closed)
        }

        /// Run one bounded indexed read through the sole journal queue.
        ///
        /// The closure receives the same immutable SQLite snapshot used by
        /// journal guards. Callers choose exact head or index queries and
        /// therefore do not decode unrelated facts.
        pub fn read<T, F>(&self, query: F) -> Result<T, BusReadError>
        where
            T: Send + 'static,
            F: for<'a> FnOnce(&journal::JournalRead<'a>) -> Result<T, GuardError> + Send + 'static,
        {
            self.transact(move |journal| {
                JournalTransaction::settled(journal.read(query).map_err(BusReadError::Head))
            })
            .map_err(BusReadError::Bus)?
        }

        /// Subscribe to commits that become durable after registration.
        pub fn subscribe(&self) -> Result<CommitSubscriber, BusError> {
            let (commits, receiver) = mpsc::channel();
            let (ready, registered) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Subscribe { commits, ready })
                .map_err(|_| BusError::Closed)?;
            registered.recv().map_err(|_| BusError::Closed)?;
            Ok(CommitSubscriber { receiver })
        }

        /// Read only the current durable cursor through the writer queue.
        pub fn cursor(&self) -> Result<Option<journal::crossing::Seq>, BusReadError> {
            self.reader().cursor()
        }

        /// Read board facts, task heads, and residency together at one barrier.
        pub fn task_snapshot(&self) -> Result<TaskSnapshot, BusReadError> {
            self.reader().task_snapshot()
        }

        /// Read incremental resident authority and one pending-steering page.
        ///
        /// The retained authority shares already-decoded fact chunks across
        /// requests. Interactive inbox polling therefore never rebuilds the
        /// complete journal merely to recover a lane's context anchor.
        pub fn steering_snapshot(
            &self,
            lane: Option<journal::crossing::CanonicalUuid>,
            start: journal::FactPageStart,
        ) -> Result<SteeringSnapshot, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Steering {
                    lane,
                    start,
                    answer,
                })
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
        }

        /// Capture the app fact and trust planes at one authority barrier.
        pub fn app_heads(&self) -> Result<AppHeads, BusReadError> {
            self.reader().app_heads()
        }

        /// Read one bounded primary-key fact page through a captured horizon.
        pub fn app_facts(
            &self,
            start: journal::FactPageStart,
            horizon: journal::crossing::Seq,
            budget: journal::FactReadBudget,
        ) -> Result<FactPage, BusReadError> {
            self.reader().app_facts(start, horizon, budget)
        }

        /// Read one bounded page of current room-authority facts through a
        /// captured horizon.
        pub fn app_rooms(
            &self,
            start: journal::FactPageStart,
            horizon: journal::crossing::Seq,
            budget: journal::FactReadBudget,
        ) -> Result<FactPage, BusReadError> {
            self.reader().app_rooms(start, horizon, budget)
        }

        /// Read one bounded causal page directly through the thread index.
        pub fn app_thread(
            &self,
            position: journal::crossing::Seq,
            budget: journal::FactReadBudget,
        ) -> Result<Option<FactPage>, BusReadError> {
            self.reader().app_thread(position, budget)
        }

        /// Read the current resident and lane head through the writer queue.
        pub fn residency_head(
            &self,
        ) -> Result<Option<JournalHead<journal::crossing::ResidencyHead>>, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::Residency { answer }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read one executable trust head through the writer queue.
        pub fn trust_head(
            &self,
            digest: &journal::crossing::Digest,
        ) -> Result<Option<JournalHead<journal::crossing::TrustHead>>, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::Trust {
                    digest: digest.clone(),
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read boundaries for the actively claimed lanes among `lanes`.
        ///
        /// One submission rather than a boundary read per lane, so every lane in
        /// a membership transition is measured at the same barrier. Reading them
        /// separately would let live capture advance between two lanes of the
        /// same move and record a split that never existed at any instant.
        pub fn lane_source_boundaries(
            &self,
            lanes: &[journal::crossing::CanonicalUuid],
        ) -> Result<Vec<journal::crossing::body::LaneSourceBoundary>, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::LaneSourceBoundaries {
                    lanes: lanes.to_vec(),
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Read one lane's accounted transcript boundary through the writer queue.
        ///
        /// Going through the queue is what makes the boundary journal-authoritative
        /// rather than merely recent: it settles behind every earlier submission,
        /// so a lifecycle fact records a position no already-committed ingestion
        /// has moved past.
        pub fn source_boundary(
            &self,
            lane: &journal::crossing::CanonicalUuid,
        ) -> Result<journal::crossing::body::SourceBoundary, BusReadError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::SourceBoundary {
                    lane: *lane,
                    answer,
                }))
                .map_err(|_| BusReadError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusReadError::Bus(BusError::Closed))?
                .map_err(BusReadError::Head)
        }

        /// Verify authority through the writer queue, after all earlier
        /// submissions have settled.
        pub fn verify(&self) -> Result<Verification, BusVerifyError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Read(JournalRead::Verify { answer }))
                .map_err(|_| BusVerifyError::Bus(BusError::Closed))?;
            receiver
                .recv()
                .map_err(|_| BusVerifyError::Bus(BusError::Closed))?
                .map_err(BusVerifyError::Journal)
        }

        /// Stop the writer after every request before the shutdown message.
        pub fn shutdown(mut self) -> Result<(), BusError> {
            self.stop()
        }

        fn stop(&mut self) -> Result<(), BusError> {
            let Some(writer) = self.writer.take() else {
                return Ok(());
            };
            let (answer, receiver) = mpsc::sync_channel(1);
            let sent = self.requests.send(Request::Shutdown { answer }).is_ok();
            let acknowledged = sent && receiver.recv().is_ok();
            writer.join().map_err(|_| BusError::WriterPanicked)?;
            if acknowledged {
                Ok(())
            } else {
                Err(BusError::Closed)
            }
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    enum Request {
        Outbox {
            query: OutboxQuery,
            answer: SyncSender<Result<Vec<Arc<OutboxSubmission>>, GuardError>>,
        },
        TranscriptSession {
            lane: journal::crossing::CanonicalUuid,
            source: journal::crossing::CanonicalUuid,
            member: u64,
            offset: u64,
            answer: SyncSender<Result<Option<TranscriptSession>, GuardError>>,
        },
        Commit {
            write: Box<JournalWrite>,
            answer: SyncSender<CommitOutcome>,
        },
        Transaction {
            transaction: Box<dyn FnOnce(&mut Journal) -> TransactionCompletion + Send>,
        },
        Subscribe {
            commits: mpsc::Sender<JournalCommit>,
            ready: SyncSender<()>,
        },
        Read(JournalRead),
        Tasks {
            answer: SyncSender<Result<TaskSnapshot, BusReadError>>,
        },
        Steering {
            lane: Option<journal::crossing::CanonicalUuid>,
            start: journal::FactPageStart,
            answer: SyncSender<Result<SteeringSnapshot, BusReadError>>,
        },
        AppHeads {
            answer: SyncSender<Result<AppHeads, BusReadError>>,
        },
        Occupant {
            read: AddressRead,
            answer: SyncSender<Result<AddressProof, BusReadError>>,
        },
        Shutdown {
            answer: SyncSender<()>,
        },
    }

    /// Reads answered from the journal alone, owing nothing to writer state.
    enum JournalRead {
        Cursor {
            answer: SyncSender<Result<Option<journal::crossing::Seq>, GuardError>>,
        },
        SteeringPresentations {
            keys: Vec<String>,
            answer: SyncSender<Result<Vec<SteeringPresentation>, GuardError>>,
        },
        RecentOriginFacts {
            origin: journal::crossing::Origin,
            kind: journal::crossing::FactKind,
            before: Option<journal::crossing::Seq>,
            budget: journal::FactReadBudget,
            answer: SyncSender<Result<journal::SelectedFactPage, GuardError>>,
        },
        AppFacts {
            start: journal::FactPageStart,
            horizon: journal::crossing::Seq,
            budget: journal::FactReadBudget,
            answer: SyncSender<Result<FactPage, GuardError>>,
        },
        AppOccupancy {
            horizon: journal::crossing::Seq,
            answer: SyncSender<Result<FactPage, GuardError>>,
        },
        AppRooms {
            start: journal::FactPageStart,
            horizon: journal::crossing::Seq,
            budget: journal::FactReadBudget,
            answer: SyncSender<Result<FactPage, GuardError>>,
        },
        AppThread {
            position: journal::crossing::Seq,
            budget: journal::FactReadBudget,
            answer: SyncSender<Result<Option<FactPage>, GuardError>>,
        },
        RoomPublication {
            room: journal::crossing::TaskStamp,
            publication: journal::crossing::rooms::RoomAuthorityStamp,
            answer: SyncSender<
                Result<Option<journal::crossing::Envelope<journal::crossing::AnyFact>>, GuardError>,
            >,
        },
        Residency {
            answer: SyncSender<
                Result<Option<JournalHead<journal::crossing::ResidencyHead>>, GuardError>,
            >,
        },
        Trust {
            digest: journal::crossing::Digest,
            answer:
                SyncSender<Result<Option<JournalHead<journal::crossing::TrustHead>>, GuardError>>,
        },
        SourceBoundary {
            lane: journal::crossing::CanonicalUuid,
            answer: SyncSender<Result<journal::crossing::body::SourceBoundary, GuardError>>,
        },
        LaneSourceBoundaries {
            lanes: Vec<journal::crossing::CanonicalUuid>,
            answer:
                SyncSender<Result<Vec<journal::crossing::body::LaneSourceBoundary>, GuardError>>,
        },
        Verify {
            answer: SyncSender<Result<Verification, VerifyError>>,
        },
    }

    fn run_writer(
        mut journal: Journal,
        requests: Receiver<Request>,
        broadcasts: tokio::sync::broadcast::Sender<JournalCommit>,
    ) {
        let mut subscribers = Vec::<mpsc::Sender<JournalCommit>>::new();
        let mut task_snapshot = TaskSnapshotState::default();
        let mut transcript_sessions = TranscriptSessionState::default();
        let mut outbox = OutboxState::default();
        let mut snapshot_waiters = SnapshotWaiters::default();
        while let Some(request) = next_request(
            &journal,
            &requests,
            &mut task_snapshot,
            &mut snapshot_waiters,
        ) {
            match request {
                Request::Outbox { query, answer } => {
                    let _ = answer.send(outbox.select(&journal, query));
                }
                Request::TranscriptSession {
                    lane,
                    source,
                    member,
                    offset,
                    answer,
                } => {
                    let _ = answer
                        .send(transcript_sessions.lookup(&journal, lane, source, member, offset));
                }
                Request::Commit { write, answer } => {
                    answer_commit(
                        &mut journal,
                        &mut task_snapshot,
                        &mut subscribers,
                        &broadcasts,
                        *write,
                        answer,
                    );
                }
                Request::Transaction { transaction } => {
                    answer_transaction(
                        &mut journal,
                        &mut task_snapshot,
                        &mut subscribers,
                        &broadcasts,
                        transaction,
                    );
                }
                Request::Subscribe { commits, ready } => {
                    subscribers.push(commits);
                    let _ = ready.send(());
                }
                Request::Read(read) => answer_journal_read(&journal, read),
                Request::Tasks { answer } => {
                    answer_tasks(&task_snapshot, &mut snapshot_waiters.tasks, answer);
                }
                Request::Steering {
                    lane,
                    start,
                    answer,
                } => {
                    let _ = answer.send(steering_snapshot(&journal, &task_snapshot, lane, start));
                }
                Request::AppHeads { answer } => {
                    let _ = answer.send(app_heads(&task_snapshot));
                }
                Request::Occupant { read, answer } => {
                    let _ = answer.send(occupant_address::answer(&journal, &task_snapshot, read));
                }
                Request::Shutdown { answer } => {
                    let _ = answer.send(());
                    break;
                }
            }
        }
    }

    fn answer_journal_read(journal: &Journal, read: JournalRead) {
        match read {
            JournalRead::Cursor { answer } => {
                let _ = answer.send(journal.read(|read| read.cursor()));
            }
            JournalRead::SteeringPresentations { keys, answer } => {
                let _ = answer.send(steering_presentations(journal, keys));
            }
            JournalRead::RecentOriginFacts {
                origin,
                kind,
                before,
                budget,
                answer,
            } => {
                let _ = answer.send(
                    journal
                        .read(move |read| read.recent_origin_facts(&origin, kind, before, budget)),
                );
            }
            JournalRead::AppFacts {
                start,
                horizon,
                budget,
                answer,
            } => {
                let _ = answer.send(journal.read(|read| read.fact_page(start, horizon, budget)));
            }
            JournalRead::AppOccupancy { horizon, answer } => {
                let _ = answer.send(journal.read(|read| {
                    read.recent_lane_occupancy(horizon, journal::FactReadBudget::RETAINED)
                }));
            }
            JournalRead::AppRooms {
                start,
                horizon,
                budget,
                answer,
            } => {
                let _ = answer
                    .send(journal.read(|read| read.room_authority_page(start, horizon, budget)));
            }
            JournalRead::AppThread {
                position,
                budget,
                answer,
            } => {
                let _ = answer.send(journal.read(|read| read.causal_page(position, budget)));
            }
            JournalRead::RoomPublication {
                room,
                publication,
                answer,
            } => {
                let _ = answer
                    .send(journal.read(move |read| read.room_publication(&room, &publication)));
            }
            JournalRead::Residency { answer } => {
                let _ = answer.send(journal.read(|read| read.residency_head()));
            }
            JournalRead::Trust { digest, answer } => {
                let _ = answer.send(journal.read(|read| read.trust_head(&digest)));
            }
            JournalRead::SourceBoundary { lane, answer } => {
                let _ = answer.send(journal.read(|read| read.source_boundary(&lane)));
            }
            JournalRead::LaneSourceBoundaries { lanes, answer } => {
                let _ = answer.send(journal.read(|read| read.lane_source_boundaries(&lanes)));
            }
            JournalRead::Verify { answer } => {
                let _ = answer.send(journal.verify());
            }
        }
    }

    #[derive(Default)]
    struct SnapshotWaiters {
        tasks: Vec<SyncSender<Result<TaskSnapshot, BusReadError>>>,
        requests: VecDeque<Request>,
    }

    fn next_request(
        journal: &Journal,
        requests: &Receiver<Request>,
        task_snapshot: &mut TaskSnapshotState,
        waiters: &mut SnapshotWaiters,
    ) -> Option<Request> {
        loop {
            if !task_snapshot.needs_step() {
                if let Some(request) = waiters.requests.pop_front() {
                    return Some(request);
                }
                return requests.recv().ok();
            }
            let _ = advance_task_snapshot(journal, task_snapshot);
            answer_waiting_tasks(task_snapshot, &mut waiters.tasks);
            match request_during_snapshot(requests, &mut waiters.requests) {
                Ok(Some(request)) => return Some(request),
                Ok(None) => {}
                Err(()) => return None,
            }
        }
    }

    fn request_during_snapshot(
        requests: &Receiver<Request>,
        retained: &mut VecDeque<Request>,
    ) -> Result<Option<Request>, ()> {
        match requests.try_recv() {
            Ok(request)
                if request_needs_task_snapshot(&request) && retained.len() < REQUEST_CAPACITY =>
            {
                retained.push_back(request);
                Ok(None)
            }
            Ok(request) => Ok(Some(request)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        }
    }

    fn request_needs_task_snapshot(request: &Request) -> bool {
        matches!(
            request,
            Request::Steering { .. } | Request::AppHeads { .. } | Request::Occupant { .. }
        )
    }

    fn steering_presentations(
        journal: &Journal,
        keys: Vec<String>,
    ) -> Result<Vec<SteeringPresentation>, GuardError> {
        if keys.len() > MAX_STEERING_PRESENTATION_KEYS {
            return Err(GuardError::Invalid(format!(
                "steering presentation batch has {} keys, maximum is {MAX_STEERING_PRESENTATION_KEYS}",
                keys.len()
            )));
        }
        journal.read(move |read| SteeringPresentation::read_batch(read, &keys))
    }

    fn answer_commit(
        journal: &mut Journal,
        task_snapshot: &mut TaskSnapshotState,
        subscribers: &mut Vec<mpsc::Sender<JournalCommit>>,
        broadcasts: &tokio::sync::broadcast::Sender<JournalCommit>,
        write: JournalWrite,
        answer: SyncSender<CommitOutcome>,
    ) {
        let outcome = match journal.apply(write) {
            Ok(applied) => {
                let commit = JournalCommit::from_applied(applied);
                invalidate_task_snapshot(task_snapshot);
                publish_commit(subscribers, broadcasts, commit);
                CommitOutcome::Committed(commit)
            }
            Err(journal::CommitError::Refused(refusal)) => CommitOutcome::Refused(refusal),
            Err(journal::CommitError::Fault(fault)) => CommitOutcome::Fault(fault),
        };
        let _ = answer.send(outcome);
    }

    fn answer_transaction(
        journal: &mut Journal,
        task_snapshot: &mut TaskSnapshotState,
        subscribers: &mut Vec<mpsc::Sender<JournalCommit>>,
        broadcasts: &tokio::sync::broadcast::Sender<JournalCommit>,
        transaction: Box<dyn FnOnce(&mut Journal) -> TransactionCompletion + Send>,
    ) {
        let TransactionCompletion { commit, respond } = transaction(journal);
        if let Some(commit) = commit {
            invalidate_task_snapshot(task_snapshot);
            publish_commit(subscribers, broadcasts, commit);
        }
        respond();
    }

    fn publish_commit(
        subscribers: &mut Vec<mpsc::Sender<JournalCommit>>,
        broadcasts: &tokio::sync::broadcast::Sender<JournalCommit>,
        commit: JournalCommit,
    ) {
        subscribers.retain(|subscriber| subscriber.send(commit).is_ok());
        let _ = broadcasts.send(commit);
    }

    fn answer_tasks(
        state: &TaskSnapshotState,
        waiters: &mut Vec<SyncSender<Result<TaskSnapshot, BusReadError>>>,
        answer: SyncSender<Result<TaskSnapshot, BusReadError>>,
    ) {
        if state.needs_step() {
            if waiters.len() < REQUEST_CAPACITY {
                waiters.push(answer);
            } else {
                let _ = answer.send(state.current());
            }
        } else {
            let _ = answer.send(state.current());
        }
    }

    fn answer_waiting_tasks(
        state: &TaskSnapshotState,
        waiters: &mut Vec<SyncSender<Result<TaskSnapshot, BusReadError>>>,
    ) {
        if state.needs_step() {
            return;
        }
        for answer in waiters.drain(..) {
            let _ = answer.send(state.current());
        }
    }

    fn current_task_snapshot(state: &TaskSnapshotState) -> Result<TaskSnapshot, BusReadError> {
        state.current()
    }

    fn steering_snapshot(
        journal: &Journal,
        state: &TaskSnapshotState,
        lane: Option<journal::crossing::CanonicalUuid>,
        start: journal::FactPageStart,
    ) -> Result<SteeringSnapshot, BusReadError> {
        current_task_snapshot(state).and_then(|authority| {
            journal
                .read(|read| {
                    read.pending_steering(lane.map(journal::crossing::Actor::Lane).as_ref(), start)
                })
                .map_err(BusReadError::Head)
                .map(|pending| SteeringSnapshot { authority, pending })
        })
    }

    fn app_heads(state: &TaskSnapshotState) -> Result<AppHeads, BusReadError> {
        current_task_snapshot(state).map(|snapshot| AppHeads {
            cursor: snapshot.cursor,
            trust: snapshot.trust.clone(),
            residency: snapshot.residency.clone(),
        })
    }

    struct TransactionCompletion {
        commit: Option<JournalCommit>,
        respond: Box<dyn FnOnce() + Send>,
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{
    AddressProof, AddressRefusal, AllocationFault, AllocationRead, AllocationWork, AppHeads,
    AuthorshipFault, BroadcastSubscriber, Bus, BusError, BusReadError, BusReader, BusVerifyError,
    ClaimAuthor, CommitOutcome, CommitSubscriber, DependencyStanding, FindingAuthors,
    JournalCommit, JournalTransaction, LaneCodeRefusal, MAX_STEERING_PRESENTATION_KEYS,
    OccupantBirth, OutboxQuery, OutboxSubmission, SteeringPresentation, SteeringPublication,
    SteeringSnapshot, TaskFacts, TaskOrigin, TaskSnapshot, TopicInputs, TranscriptSession,
    TranscriptSessions, context_opening, journal_seat_addresses,
};
