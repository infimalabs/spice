//! Identity-safe cleanup for owned descendants that create new process groups.

use std::collections::BTreeSet;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod snapshot;

use snapshot::process_snapshot;

use crate::ProcessExitObserver;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProcessGeneration {
    seconds: u64,
    subsecond: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Process {
    pid: u32,
    parent: u32,
    group: u32,
    terminal: bool,
    generation: Option<ProcessGeneration>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProcessIdentity {
    pid: u32,
    generation: ProcessGeneration,
}

fn process_identity(process: &Process) -> Option<ProcessIdentity> {
    process.generation.map(|generation| ProcessIdentity {
        pid: process.pid,
        generation,
    })
}

/// A captured process tree whose non-root process groups remain bound to native
/// process generations even after their original parent exits.
#[derive(Debug)]
pub struct DescendantGroups {
    root: u32,
    groups: BTreeSet<u32>,
    members: BTreeSet<ProcessIdentity>,
    /// The root's exit, armed while the root can still be named.
    root_exit: Option<ProcessExitObserver>,
}

/// The bounds of one reap, each measured from its start.
#[derive(Clone, Copy, Debug)]
struct ReapBounds {
    grace: Duration,
    reap: Duration,
    output_drain: Duration,
}

/// What one observation found still running in the owned groups.
#[derive(Debug, Default, Eq, PartialEq)]
struct Live {
    groups: BTreeSet<u32>,
    members: Vec<u32>,
}

impl DescendantGroups {
    /// Capture the process rooted at `root` and every descendant visible now.
    pub fn discover(root: u32) -> io::Result<Self> {
        let root_exit = ProcessExitObserver::or_ended(root)?;
        let processes = process_snapshot()?;
        let mut descendants = Self::from_snapshot(root, &processes)?;
        descendants.root_exit = Some(root_exit);
        Ok(descendants)
    }

    fn from_snapshot(root: u32, processes: &[Process]) -> io::Result<Self> {
        let members = match processes.iter().find(|process| process.pid == root) {
            Some(process) => BTreeSet::from([process_identity(process).ok_or_else(|| {
                io::Error::other(format!(
                    "owned root {root} has no readable native process generation"
                ))
            })?]),
            None => BTreeSet::new(),
        };
        let mut descendants = Self {
            root,
            members,
            groups: BTreeSet::new(),
            root_exit: None,
        };
        descendants.reconcile(processes)?;
        Ok(descendants)
    }

    /// Send `SIGTERM` to every currently owned non-root process group.
    pub fn terminate(&mut self) -> io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    /// Reconcile groups created during shutdown, escalate live groups to
    /// `SIGKILL` after `grace`, and require both the root and captured tree to
    /// remain terminal inside the declared bounds.
    pub fn reap_while_root_terminates(
        &mut self,
        grace: Duration,
        reap: Duration,
        output_drain: Duration,
        root_terminal: &AtomicBool,
    ) -> io::Result<()> {
        let mut snapshot = process_snapshot;
        let mut signal_group = kill_process_group;
        let mut root_exit = self.root_exit.take();
        let mut wait = |members: &[u32], deadline: Instant| {
            await_exits(members, &mut root_exit, root_terminal, deadline)
        };
        let reaped = self.reap_while_root_terminates_with(
            ReapBounds {
                grace,
                reap,
                output_drain,
            },
            root_terminal,
            &mut snapshot,
            &mut signal_group,
            &mut wait,
        );
        self.root_exit = root_exit;
        reaped
    }

    /// Observe the tree, then wait on what it found running, until the root
    /// and tree are terminal across two consecutive observations. `wait`
    /// blocks until every listed member and, while it runs, the root have
    /// exited, or until the deadline it is given.
    fn reap_while_root_terminates_with<Snapshot, Signal, Wait>(
        &mut self,
        bounds: ReapBounds,
        root_terminal: &AtomicBool,
        snapshot: &mut Snapshot,
        signal_group: &mut Signal,
        wait: &mut Wait,
    ) -> io::Result<()>
    where
        Snapshot: FnMut() -> io::Result<Vec<Process>>,
        Signal: FnMut(u32, libc::c_int) -> io::Result<()>,
        Wait: FnMut(&[u32], Instant) -> io::Result<()>,
    {
        let started = Instant::now();
        let grace = bounds.grace;
        let reap_deadline = grace.saturating_add(bounds.reap);
        let absolute_deadline = reap_deadline.saturating_add(bounds.output_drain);
        let mut signal = libc::SIGTERM;
        let mut observed_terminal = false;
        loop {
            let elapsed = started.elapsed();
            if signal == libc::SIGTERM && elapsed >= grace {
                signal = libc::SIGKILL;
                self.observe_and_signal(signal, true, snapshot, signal_group)?;
            }
            let root_was_terminal = root_terminal.load(Ordering::Acquire);
            let live = self.observe_and_signal(signal, false, snapshot, signal_group)?;
            if root_was_terminal && live.groups.is_empty() {
                if observed_terminal {
                    return Ok(());
                }
                observed_terminal = true;
            } else {
                observed_terminal = false;
            }
            let elapsed = started.elapsed();
            if elapsed >= absolute_deadline || (elapsed >= reap_deadline && !live.groups.is_empty())
            {
                let detail = if !live.groups.is_empty() {
                    format!(
                        "process groups {} remained live",
                        display_groups(&live.groups)
                    )
                } else if !root_terminal.load(Ordering::Acquire) {
                    "the action process group did not finish inside its cleanup bounds".to_owned()
                } else {
                    "the descendant tree did not remain terminal across two observations".to_owned()
                };
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{detail} after the {:?} reap deadline", bounds.reap),
                ));
            }
            if observed_terminal {
                continue;
            }
            let boundary = if signal == libc::SIGTERM {
                grace
            } else if live.groups.is_empty() {
                absolute_deadline
            } else {
                reap_deadline
            };
            wait(&live.members, deadline_after(started, boundary))?;
        }
    }

    fn signal(&mut self, signal: libc::c_int) -> io::Result<()> {
        let mut snapshot = process_snapshot;
        let mut signal_group = kill_process_group;
        self.observe_and_signal(signal, true, &mut snapshot, &mut signal_group)
            .map(|_| ())
    }

    fn observe_and_signal<Snapshot, Signal>(
        &mut self,
        signal: libc::c_int,
        all: bool,
        snapshot: &mut Snapshot,
        signal_group: &mut Signal,
    ) -> io::Result<Live>
    where
        Snapshot: FnMut() -> io::Result<Vec<Process>>,
        Signal: FnMut(u32, libc::c_int) -> io::Result<()>,
    {
        let processes = snapshot()?;
        let changed = self.reconcile(&processes)?;
        let targets = if all { self.groups.clone() } else { changed };
        if !targets.is_empty() {
            signal_owned_groups(
                self.root,
                &targets,
                &self.members,
                &mut *snapshot,
                &mut *signal_group,
                signal,
            )?;
        }
        let groups = owned_live_groups(&processes, self.root, &self.groups, &self.members)?;
        let members = processes
            .iter()
            .filter(|process| groups.contains(&process.group) && !process.terminal)
            .map(|process| process.pid)
            .collect();
        Ok(Live { groups, members })
    }

    fn reconcile(&mut self, processes: &[Process]) -> io::Result<BTreeSet<u32>> {
        // Validate every previously captured group before current parentage can
        // add authority. A recycled foreign group must never become owned merely
        // because a different current process happens to use its numeric PGID.
        owned_live_groups(processes, self.root, &self.groups, &self.members)?;

        let owned = owned_processes(processes, &self.members);
        let mut changed = BTreeSet::new();
        for process in processes
            .iter()
            .filter(|process| owned.contains(&process.pid))
        {
            let identity = process_identity(process).ok_or_else(|| {
                io::Error::other(format!(
                    "owned descendant {} has no readable native process generation",
                    process.pid
                ))
            })?;
            let new_member = self.members.insert(identity);
            let new_group = process.group != self.root && self.groups.insert(process.group);
            if process.group != self.root && (new_member || new_group) {
                changed.insert(process.group);
            }
        }
        Ok(changed)
    }
}

/// The pids of every running process in `group`.
pub(crate) fn live_group_members(group: u32) -> io::Result<Vec<u32>> {
    Ok(process_snapshot()?
        .into_iter()
        .filter(|process| process.group == group && !process.terminal)
        .map(|process| process.pid)
        .collect())
}

/// Block until every one of `members` has exited and, unless it is already
/// terminal, the root has too, or until `deadline`. The root's observed exit
/// marks it terminal.
fn await_exits(
    members: &[u32],
    root_exit: &mut Option<ProcessExitObserver>,
    root_terminal: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    let observers = members
        .iter()
        .map(|pid| ProcessExitObserver::or_ended(*pid))
        .collect::<io::Result<Vec<_>>>()?;
    for observer in &observers {
        if !observer.wait_until(deadline)? {
            return Ok(());
        }
    }
    if !root_terminal.load(Ordering::Acquire)
        && let Some(root) = root_exit
        && root.wait_until(deadline)?
    {
        *root_exit = None;
        root_terminal.store(true, Ordering::Release);
    }
    Ok(())
}

/// The instant `offset` after `started`, held at the farthest representable
/// instant when the bound is effectively unbounded.
fn deadline_after(started: Instant, offset: Duration) -> Instant {
    started
        .checked_add(offset)
        .unwrap_or_else(|| started + Duration::from_secs(u64::from(u32::MAX)))
}

fn owned_processes(processes: &[Process], members: &BTreeSet<ProcessIdentity>) -> BTreeSet<u32> {
    let mut descendants = processes
        .iter()
        .filter(|process| {
            process_identity(process).is_some_and(|identity| members.contains(&identity))
        })
        .map(|process| process.pid)
        .collect::<BTreeSet<_>>();
    loop {
        let before = descendants.len();
        for process in processes {
            if descendants.contains(&process.parent) {
                descendants.insert(process.pid);
            }
        }
        if descendants.len() == before {
            break;
        }
    }
    descendants
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupOwnership {
    Absent,
    Owned,
    Drifted,
}

fn group_ownership(
    processes: &[Process],
    group: u32,
    members: &BTreeSet<ProcessIdentity>,
) -> GroupOwnership {
    let owned = owned_processes(processes, members);
    let live = processes
        .iter()
        .filter(|process| process.group == group && !process.terminal)
        .collect::<Vec<_>>();
    if live.is_empty() {
        return GroupOwnership::Absent;
    }
    if live.iter().any(|process| process.generation.is_none()) {
        return GroupOwnership::Drifted;
    }
    if live.iter().any(|process| owned.contains(&process.pid)) {
        GroupOwnership::Owned
    } else {
        GroupOwnership::Drifted
    }
}

fn owned_live_groups(
    processes: &[Process],
    root: u32,
    groups: &BTreeSet<u32>,
    members: &BTreeSet<ProcessIdentity>,
) -> io::Result<BTreeSet<u32>> {
    let mut live = BTreeSet::new();
    for group in groups {
        match group_ownership(processes, *group, members) {
            GroupOwnership::Absent => {}
            GroupOwnership::Owned => {
                live.insert(*group);
            }
            GroupOwnership::Drifted => return Err(ownership_drift(root, *group)),
        }
    }
    Ok(live)
}

fn signal_owned_groups<Snapshot, Signal>(
    root: u32,
    groups: &BTreeSet<u32>,
    members: &BTreeSet<ProcessIdentity>,
    mut snapshot: Snapshot,
    mut signal_group: Signal,
    signal: libc::c_int,
) -> io::Result<()>
where
    Snapshot: FnMut() -> io::Result<Vec<Process>>,
    Signal: FnMut(u32, libc::c_int) -> io::Result<()>,
{
    let mut failures = Vec::new();
    for group in groups {
        match group_ownership(&snapshot()?, *group, members) {
            GroupOwnership::Absent => continue,
            GroupOwnership::Drifted => return Err(ownership_drift(root, *group)),
            GroupOwnership::Owned => {}
        }
        if let Err(error) = signal_group(*group, signal) {
            match error.raw_os_error() {
                Some(libc::ESRCH) => {}
                Some(libc::EPERM) => match group_ownership(&snapshot()?, *group, members) {
                    GroupOwnership::Absent => {}
                    GroupOwnership::Drifted => return Err(ownership_drift(root, *group)),
                    GroupOwnership::Owned => failures.push(format!("{group}: {error}")),
                },
                _ => failures.push(format!("{group}: {error}")),
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "signal {signal} failed for {}",
            failures.join(", ")
        )))
    }
}

fn kill_process_group(group: u32, signal: libc::c_int) -> io::Result<()> {
    let group = libc::pid_t::try_from(group)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "group identity exceeds pid_t"))?;
    // SAFETY: the caller immediately revalidated this owned process group.
    if unsafe { libc::kill(-group, signal) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn ownership_drift(root: u32, group: u32) -> io::Error {
    io::Error::other(format!(
        "process group {group} is live but no longer descends from owned root {root}"
    ))
}

fn display_groups(groups: &BTreeSet<u32>) -> String {
    groups
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_tree_follows_parentage_across_process_group_boundaries() {
        let processes = [
            process(10, 1, 10),
            process(11, 10, 10),
            process(12, 11, 12),
            process(13, 12, 13),
            process(14, 99, 14),
        ];
        let descendants =
            DescendantGroups::from_snapshot(10, &processes).expect("fixture tree captures");

        assert_eq!(descendants.groups, BTreeSet::from([12, 13]));
        assert_eq!(
            descendants
                .members
                .iter()
                .map(|member| member.pid)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([10, 11, 12, 13]),
        );
    }

    #[test]
    fn same_second_recycled_generation_drifts_before_any_signal() {
        let groups = BTreeSet::from([12]);
        let owned = [process_with_generation(12, 10, 12, 900, 100)];
        let members = owned.iter().filter_map(process_identity).collect();
        let foreign = vec![process_with_generation(12, 99, 12, 900, 900)];
        let mut signals = Vec::new();

        signal_owned_groups(
            10,
            &groups,
            &members,
            || Ok(foreign.clone()),
            |group, signal| {
                signals.push((group, signal));
                Ok(())
            },
            libc::SIGTERM,
        )
        .expect_err("foreign ownership refuses");

        assert!(signals.is_empty(), "foreign group received a signal");
    }

    #[test]
    fn unreadable_native_generation_cannot_authorize_a_group_signal() {
        let groups = BTreeSet::from([12]);
        let owned = [process_with_generation(12, 10, 12, 900, 100)];
        let members = owned.iter().filter_map(process_identity).collect();
        let mut unreadable = process_with_generation(12, 10, 12, 900, 100);
        unreadable.generation = None;
        let mut signals = Vec::new();

        signal_owned_groups(
            10,
            &groups,
            &members,
            || Ok(vec![unreadable]),
            |group, signal| {
                signals.push((group, signal));
                Ok(())
            },
            libc::SIGTERM,
        )
        .expect_err("unreadable native generation refuses");

        assert!(signals.is_empty(), "unreadable group received a signal");
    }

    #[test]
    fn captured_generation_remains_owned_after_the_root_exits() {
        let groups = BTreeSet::from([12]);
        let detached = vec![process_with_generation(12, 1, 12, 900, 100)];
        let members = detached.iter().filter_map(process_identity).collect();
        let mut signals = Vec::new();

        signal_owned_groups(
            10,
            &groups,
            &members,
            || Ok(detached.clone()),
            |group, signal| {
                signals.push((group, signal));
                Ok(())
            },
            libc::SIGTERM,
        )
        .expect("captured detached generation remains owned");

        assert_eq!(signals, [(12, libc::SIGTERM)]);
    }

    #[test]
    fn group_created_after_initial_discovery_enters_signal_and_reap_accounting() {
        let initial = vec![process(10, 1, 10)];
        let late = vec![process(10, 1, 10), process(12, 10, 12)];
        let mut descendants =
            DescendantGroups::from_snapshot(10, &initial).expect("initial tree captures");
        let mut snapshot = || Ok(late.clone());
        let mut signals = Vec::new();

        let live = descendants
            .observe_and_signal(libc::SIGTERM, false, &mut snapshot, &mut |group, signal| {
                signals.push((group, signal));
                Ok(())
            })
            .expect("late owned group reconciles");

        assert_eq!(descendants.groups, BTreeSet::from([12]));
        assert!(
            descendants
                .members
                .contains(&process_identity(&late[1]).expect("fixture generation is readable")),
            "late member generation enters persistent ownership"
        );
        assert_eq!(live.groups, BTreeSet::from([12]));
        assert_eq!(live.members, [12]);
        assert_eq!(signals, [(12, libc::SIGTERM)]);
    }

    #[test]
    fn group_created_while_root_terminates_follows_term_into_the_kill_phase() {
        let initial = vec![process(10, 1, 10)];
        let late = process(12, 10, 12);
        let killed = std::cell::Cell::new(false);
        let root_terminal = AtomicBool::new(false);
        let mut descendants =
            DescendantGroups::from_snapshot(10, &initial).expect("initial tree captures");
        let mut snapshot = || {
            Ok(if killed.get() {
                initial.clone()
            } else {
                vec![initial[0], late]
            })
        };
        let mut signals = Vec::new();
        let mut signal_group = |group, signal| {
            signals.push((group, signal));
            if signal == libc::SIGKILL {
                killed.set(true);
                root_terminal.store(true, Ordering::Release);
            }
            Ok(())
        };
        // The late member ignores the request to leave, so no exit arrives
        // before the deadline each wait is given.
        let (_exits, exited) = std::sync::mpsc::channel::<()>();
        let mut waited = Vec::new();
        let mut wait = |members: &[u32], deadline: Instant| {
            waited.push(members.to_vec());
            let _ = exited.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            Ok(())
        };

        descendants
            .reap_while_root_terminates_with(
                ReapBounds {
                    grace: Duration::from_millis(20),
                    reap: Duration::from_millis(50),
                    output_drain: Duration::from_millis(50),
                },
                &root_terminal,
                &mut snapshot,
                &mut signal_group,
                &mut wait,
            )
            .expect("late group follows the bounded root cleanup phases");

        assert_eq!(signals, [(12, libc::SIGTERM), (12, libc::SIGKILL)],);
        assert_eq!(waited.first(), Some(&vec![12]));
    }

    #[test]
    fn permission_refusal_rechecks_ownership_without_retrying_a_recycled_group() {
        let groups = BTreeSet::from([12]);
        let owned = vec![
            process(10, 1, 10),
            process_with_generation(12, 10, 12, 900, 100),
        ];
        let members = owned.iter().filter_map(process_identity).collect();
        let foreign = vec![process_with_generation(12, 99, 12, 900, 900)];
        let mut snapshots = vec![foreign, owned].into_iter().rev();
        let mut signals = 0;

        signal_owned_groups(
            10,
            &groups,
            &members,
            || Ok(snapshots.next().expect("two ownership snapshots")),
            |_, _| {
                signals += 1;
                Err(io::Error::from_raw_os_error(libc::EPERM))
            },
            libc::SIGTERM,
        )
        .expect_err("recycled group is a typed ownership drift");

        assert_eq!(signals, 1, "permission refusal must not be retried");
    }

    #[test]
    fn permission_refusal_is_benign_when_the_owned_group_is_then_absent() {
        let groups = BTreeSet::from([12]);
        let owned = vec![
            process(10, 1, 10),
            process_with_generation(12, 10, 12, 900, 100),
        ];
        let members = owned.iter().filter_map(process_identity).collect();
        let mut snapshots = vec![Vec::new(), owned].into_iter().rev();
        let mut signals = 0;

        signal_owned_groups(
            10,
            &groups,
            &members,
            || Ok(snapshots.next().expect("two ownership snapshots")),
            |_, _| {
                signals += 1;
                Err(io::Error::from_raw_os_error(libc::EPERM))
            },
            libc::SIGTERM,
        )
        .expect("an exited owned group makes the stale permission refusal benign");

        assert_eq!(signals, 1, "permission refusal must not be retried");
    }

    fn process(pid: u32, parent: u32, group: u32) -> Process {
        process_with_generation(pid, parent, group, u64::from(pid), 0)
    }

    const fn process_with_generation(
        pid: u32,
        parent: u32,
        group: u32,
        seconds: u64,
        subsecond: u64,
    ) -> Process {
        Process {
            pid,
            parent,
            group,
            terminal: false,
            generation: Some(ProcessGeneration { seconds, subsecond }),
        }
    }
}
