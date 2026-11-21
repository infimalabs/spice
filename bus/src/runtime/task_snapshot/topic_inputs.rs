//! Snapshot-local secondary index of the statements a topic tree may read, in
//! the order they arrived. Where a statement was filed decides only whether it
//! is public; it is never an index key. Persistent maps share untouched branches.

use imbl::{HashMap, OrdSet};
use journal::TaskHeads;
use journal::crossing::{Seq, TaskStamp, is_oops_topic};

/// Eligible public statements at one immutable task projection. Old generations
/// can retain this handle for membership checks without retaining statement text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TopicInputs {
    by_task: HashMap<TaskStamp, Seq>,
    by_arrival: OrdSet<(Seq, TaskStamp)>,
}

impl TopicInputs {
    pub(super) fn update(&mut self, heads: &TaskHeads) {
        let task = heads.statement().body();
        let workflow = heads.workflow().body();
        let eligible = task.topic.path.as_ref().is_some_and(|path| {
            path.private_owner().is_none()
                && !is_oops_topic(path)
                && workflow.is_activated()
                && (workflow.status.is_pending() || workflow.concluded_positively())
        });
        self.replace(&task.born, eligible.then_some(task.topic_origin));
    }

    fn replace(&mut self, id: &TaskStamp, arrival: Option<Seq>) {
        if self.by_task.get(id).copied() == arrival {
            return;
        }
        if let Some(old) = self.by_task.remove(id) {
            self.by_arrival.remove(&(old, id.clone()));
        }
        if let Some(arrival) = arrival {
            self.by_task.insert(id.clone(), arrival);
            self.by_arrival.insert((arrival, id.clone()));
        }
    }

    /// Arrival position of an eligible statement.
    pub fn member(&self, id: &TaskStamp) -> Option<Seq> {
        self.by_task.get(id).copied()
    }

    /// Every eligible statement, including those older than any window.
    pub fn len(&self) -> usize {
        self.by_task.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_task.is_empty()
    }

    /// At most `limit` of the newest arrivals, oldest first; no task head is
    /// decoded or visited here.
    pub fn newest(&self, limit: usize) -> Vec<&TaskStamp> {
        let mut selected: Vec<_> = (self.by_arrival.iter().rev())
            .take(limit)
            .map(|(_, id)| id)
            .collect();
        selected.reverse();
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_arrivals_come_oldest_first_and_a_prior_handle_keeps_its_members() {
        let ids = [8, 5, 3, 1].map(|i| TaskStamp::for_micros(i).unwrap());
        let seq = |n| Seq::new(n).unwrap();
        let mut index = TopicInputs::default();
        for (i, id) in ids.iter().enumerate() {
            index.replace(id, Some(seq(i as u64 + 1)));
        }
        let prior = index.clone();
        assert_eq!(prior.newest(2), [&ids[2], &ids[3]]);
        // One statement leaves and an older one arrives again.
        index.replace(&ids[3], None);
        index.replace(&ids[0], Some(seq(9)));
        assert_eq!(index.newest(2), [&ids[2], &ids[0]]);
        assert_eq!(index.len(), 3);
        assert_eq!(prior.member(&ids[3]), Some(seq(4)));
        assert_eq!(index.member(&ids[3]), None);
    }
}
