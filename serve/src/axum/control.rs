//! Bounded priority scheduling for authenticated resident commands.

use tokio::sync::mpsc;

use wire::CommandKind;

const DEMAND_BURST_LIMIT: usize = 3;
pub(super) const FAIR_CYCLE_LIMIT: usize = DEMAND_BURST_LIMIT + 1;

/// Scheduling class selected at the authenticated app boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Class {
    Demand,
    Ordinary,
}

/// Classify every command explicitly so registry growth cannot inherit policy.
pub(super) const fn class(kind: CommandKind) -> Class {
    match kind {
        CommandKind::OutboxSubmit | CommandKind::OutboxRead => Class::Ordinary,
        CommandKind::LaneSeat
        | CommandKind::LaneRenewal
        | CommandKind::LaneRetune
        | CommandKind::LaneEffortRetune
        | CommandKind::LaneFastRetune
        | CommandKind::TeamConfigure
        | CommandKind::TeamArrange => Class::Demand,
        CommandKind::BoardRead
        | CommandKind::TopicRead
        | CommandKind::TopicEdit
        | CommandKind::TaskDetail
        | CommandKind::TaskRelations
        | CommandKind::TaskTimeline
        | CommandKind::RackRead
        | CommandKind::GlobalSettingsRead
        | CommandKind::GlobalSettingsSet
        | CommandKind::VendorCatalog
        | CommandKind::BoardGraph
        | CommandKind::BoardHandout
        | CommandKind::DocLedger
        | CommandKind::RoomPost
        | CommandKind::RoomScore
        | CommandKind::RoomConversationText
        | CommandKind::RoomLeave
        | CommandKind::RoomContribute
        | CommandKind::RoomContributionRead
        | CommandKind::PreparationDecide
        | CommandKind::OutputAdopt
        | CommandKind::TaskAdd
        | CommandKind::TaskOopsAdd
        | CommandKind::TaskSweep
        | CommandKind::TaskEdit
        | CommandKind::TaskDelete
        | CommandKind::TaskClose
        | CommandKind::TaskConclude
        | CommandKind::TaskNote
        | CommandKind::TaskDefer
        | CommandKind::TaskWake
        | CommandKind::TaskFile
        | CommandKind::TaskPlace
        | CommandKind::TaskMedia
        | CommandKind::TaskDepends
        | CommandKind::TaskTarget
        | CommandKind::TaskJoin
        | CommandKind::TaskInvite
        | CommandKind::TaskDone
        | CommandKind::TaskReview
        | CommandKind::ClaimWin
        | CommandKind::ClaimRefresh
        | CommandKind::ClaimFree
        | CommandKind::ClaimLapse
        | CommandKind::SteerSend
        | CommandKind::SteerInbox
        | CommandKind::SteerResend
        | CommandKind::SteerRecover
        | CommandKind::SteerCancel
        | CommandKind::SteerLineControl
        | CommandKind::QuoteRead
        | CommandKind::ConsentRead
        | CommandKind::ConsentPolicy
        | CommandKind::ConsentDecide
        | CommandKind::TrustPend
        | CommandKind::TrustGrant
        | CommandKind::TrustRevoke
        | CommandKind::TrustShow
        | CommandKind::PrincipalDirectoryRead
        | CommandKind::PrincipalRead
        | CommandKind::PrincipalAdmit
        | CommandKind::PrincipalEdit
        | CommandKind::PrincipalGrant
        | CommandKind::PrincipalRevoke
        | CommandKind::PrincipalDeauthorize
        | CommandKind::LaneRead
        | CommandKind::LaneState
        | CommandKind::LaneJobsRead
        | CommandKind::LaneJobRead
        | CommandKind::LaneJobStop
        | CommandKind::LaneRecall
        | CommandKind::ProofRecognize
        | CommandKind::UpChange => Class::Ordinary,
    }
}

#[derive(Debug)]
struct Scheduled<T> {
    item: T,
    barrier: bool,
}

/// Producer side of the one global scheduler, with finite capacity per class.
#[derive(Clone, Debug)]
pub(super) struct Sender<T> {
    demands: mpsc::Sender<Scheduled<T>>,
    ordinary: mpsc::Sender<Scheduled<T>>,
}

/// Reserved slot in one scheduler class.
pub(super) struct Permit<'a, T>(PermitKind<'a, T>);

enum PermitKind<'a, T> {
    Demand(mpsc::Permit<'a, Scheduled<T>>),
    Ordinary(mpsc::Permit<'a, Scheduled<T>>),
}

impl<T> Sender<T> {
    pub(super) async fn reserve(&self, class: Class) -> Result<Permit<'_, T>, ()> {
        match class {
            Class::Demand => self
                .demands
                .reserve()
                .await
                .map(|permit| Permit(PermitKind::Demand(permit)))
                .map_err(|_| ()),
            Class::Ordinary => self
                .ordinary
                .reserve()
                .await
                .map(|permit| Permit(PermitKind::Ordinary(permit)))
                .map_err(|_| ()),
        }
    }
}

impl<T> Permit<'_, T> {
    pub(super) fn send(self, item: T, barrier: bool) {
        let scheduled = Scheduled { item, barrier };
        match self.0 {
            PermitKind::Demand(permit) => permit.send(scheduled),
            PermitKind::Ordinary(permit) => permit.send(scheduled),
        }
    }
}

/// Consumer side of the scheduler, owned only by resident `Control`.
#[derive(Debug)]
pub(super) struct Receiver<T> {
    demands: mpsc::Receiver<Scheduled<T>>,
    ordinary: mpsc::Receiver<Scheduled<T>>,
    demand_head: Option<Scheduled<T>>,
    ordinary_head: Option<Scheduled<T>>,
    consecutive_demands: usize,
}

/// Build one scheduler with `capacity` queued slots plus one consumer head per class.
pub(super) fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (demands, demand_receiver) = mpsc::channel(capacity);
    let (ordinary, ordinary_receiver) = mpsc::channel(capacity);
    (
        Sender { demands, ordinary },
        Receiver {
            demands: demand_receiver,
            ordinary: ordinary_receiver,
            demand_head: None,
            ordinary_head: None,
            consecutive_demands: 0,
        },
    )
}

impl<T> Receiver<T> {
    pub(super) async fn recv(&mut self) -> Option<T> {
        loop {
            self.fill_ready_heads();
            if let Some(item) = self.take_ready() {
                return Some(item);
            }

            match (self.demands.is_closed(), self.ordinary.is_closed()) {
                (true, true) => return None,
                (true, false) => self.ordinary_head = self.ordinary.recv().await,
                (false, true) => self.demand_head = self.demands.recv().await,
                (false, false) if self.consecutive_demands < DEMAND_BURST_LIMIT => {
                    tokio::select! {
                        biased;
                        item = self.demands.recv() => self.demand_head = item,
                        item = self.ordinary.recv() => self.ordinary_head = item,
                    }
                }
                (false, false) => {
                    tokio::select! {
                        biased;
                        item = self.ordinary.recv() => self.ordinary_head = item,
                        item = self.demands.recv() => self.demand_head = item,
                    }
                }
            }
        }
    }

    pub(super) fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.fill_ready_heads();
        if let Some(item) = self.take_ready() {
            return Ok(item);
        }
        if self.demands.is_closed() && self.ordinary.is_closed() {
            Err(mpsc::error::TryRecvError::Disconnected)
        } else {
            Err(mpsc::error::TryRecvError::Empty)
        }
    }

    pub(super) fn close(&mut self) {
        self.demands.close();
        self.ordinary.close();
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.demand_head.is_none()
            && self.ordinary_head.is_none()
            && self.demands.is_empty()
            && self.ordinary.is_empty()
    }

    fn fill_ready_heads(&mut self) {
        if self.demand_head.is_none()
            && let Ok(item) = self.demands.try_recv()
        {
            self.demand_head = Some(item);
        }
        if self.ordinary_head.is_none()
            && let Ok(item) = self.ordinary.try_recv()
        {
            self.ordinary_head = Some(item);
        }
    }

    fn take_ready(&mut self) -> Option<T> {
        match (self.demand_head.is_some(), self.ordinary_head.is_some()) {
            (false, false) => None,
            (true, false) => {
                self.consecutive_demands = 0;
                self.demand_head.take().map(|scheduled| scheduled.item)
            }
            (false, true) => {
                self.consecutive_demands = 0;
                self.ordinary_head.take().map(|scheduled| scheduled.item)
            }
            (true, true) => {
                let ordinary_is_barrier = self
                    .ordinary_head
                    .as_ref()
                    .is_some_and(|scheduled| scheduled.barrier);
                if ordinary_is_barrier || self.consecutive_demands < DEMAND_BURST_LIMIT {
                    self.consecutive_demands = self.consecutive_demands.saturating_add(1);
                    self.demand_head.take().map(|scheduled| scheduled.item)
                } else {
                    self.consecutive_demands = 0;
                    self.ordinary_head.take().map(|scheduled| scheduled.item)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send(sender: &Sender<u8>, class: Class, item: u8, barrier: bool) {
        sender
            .reserve(class)
            .await
            .expect("scheduler remains open")
            .send(item, barrier);
    }

    #[test]
    fn classification_is_closed_over_the_command_registry() {
        let demands = CommandKind::ALL
            .iter()
            .copied()
            .filter(|kind| class(*kind) == Class::Demand)
            .collect::<Vec<_>>();
        assert_eq!(
            demands,
            vec![
                CommandKind::LaneSeat,
                CommandKind::LaneRenewal,
                CommandKind::LaneRetune,
                CommandKind::LaneEffortRetune,
                CommandKind::LaneFastRetune,
                CommandKind::TeamConfigure,
                CommandKind::TeamArrange,
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_ready_work_exposes_one_fair_fifo_burst_then_its_tail() {
        let (sender, mut receiver) = channel(8);
        for item in [1, 2] {
            send(&sender, Class::Ordinary, item, false).await;
        }
        for item in [10, 11, 12, 13, 14] {
            send(&sender, Class::Demand, item, false).await;
        }

        let mut received = Vec::new();
        for _ in 0..FAIR_CYCLE_LIMIT {
            received.push(receiver.try_recv().expect("server-scheduled burst"));
        }
        assert_eq!(received, [10, 11, 12, 1]);
        while let Ok(item) = receiver.try_recv() {
            received.push(item);
        }
        assert_eq!(received, [10, 11, 12, 1, 13, 14, 2]);
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_terminal_barrier_waits_for_every_admitted_demand() {
        let (sender, mut receiver) = channel(8);
        send(&sender, Class::Ordinary, 1, false).await;
        send(&sender, Class::Ordinary, 2, true).await;
        for item in [10, 11, 12, 13] {
            send(&sender, Class::Demand, item, false).await;
        }

        let mut received = Vec::new();
        for _ in 0..6 {
            received.push(receiver.recv().await.expect("queued work"));
        }
        assert_eq!(received, [10, 11, 12, 1, 13, 2]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn each_class_has_independent_finite_capacity_and_close_wakes_reservations() {
        let (sender, mut receiver) = channel(1);
        send(&sender, Class::Ordinary, 1, false).await;
        send(&sender, Class::Demand, 2, false).await;

        let ordinary = sender.clone();
        let demand = sender.clone();
        let (ordinary_started, ordinary_entered) = tokio::sync::oneshot::channel();
        let (demand_started, demand_entered) = tokio::sync::oneshot::channel();
        let ordinary_wait = tokio::spawn(async move {
            let _ = ordinary_started.send(());
            ordinary.reserve(Class::Ordinary).await.is_err()
        });
        let demand_wait = tokio::spawn(async move {
            let _ = demand_started.send(());
            demand.reserve(Class::Demand).await.is_err()
        });
        ordinary_entered.await.expect("ordinary waiter starts");
        demand_entered.await.expect("demand waiter starts");
        receiver.close();

        assert!(ordinary_wait.await.expect("ordinary waiter joins"));
        assert!(demand_wait.await.expect("demand waiter joins"));
        assert_eq!(receiver.try_recv(), Ok(2));
        assert_eq!(receiver.try_recv(), Ok(1));
        assert_eq!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        );
    }
}
