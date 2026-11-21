use super::*;
use wire::topic::TreeLag;

impl Generation {
    pub(in crate::board_command) fn lag(&self, observed: &TaskSnapshot) -> Option<TreeLag> {
        let source = observed.task_projection_cursor()?;
        (self.projection < Some(source)).then_some(TreeLag {
            lineage: self.lineage,
            projection: self.projection,
            source,
        })
    }

    /// This admits grouping evidence, never a journal write. The decision's
    /// scope, statement and destination come from `observed`.
    pub(super) fn admit_read(&self, observed: &TaskSnapshot) -> Result<(), Outcome<CommandAnswer>> {
        if !Arc::ptr_eq(&self.fixed, observed.described_topics()) {
            return Err(read_guard::refusal(
                "topic::preview-catching-up",
                None,
                "described topics changed since this tree was built",
            ));
        }
        let wanted = observed.task_projection_cursor();
        if self.projection == wanted {
            return Ok(());
        }
        let refuse = |why| read_guard::refusal("topic::preview-catching-up", None, why);
        // `facts().after` walks backwards from the tail. Bound that walk too,
        // not just the number of task rows visited by the iterator it returns.
        let distance = observed
            .cursor()
            .map_or(0, Seq::get)
            .saturating_sub(self.through.map_or(0, Seq::get));
        if self.projection > wanted
            || self.through > observed.cursor()
            || distance > CAPACITY as u64
        {
            return Err(refuse(
                "the decision is outside the tree's bounded journal lookback",
            ));
        }
        let mut touched = std::collections::BTreeSet::new();
        let mut visited = 0;
        let mut drift = 0;
        for row in observed.facts().after(self.through) {
            if Some(row.seq) > wanted {
                break;
            }
            if row.fact.read::<TopicMoved>().is_some() {
                return Err(refuse("a topic subtree moved since this tree was built"));
            }
            for id in row.fact.mutated_tasks() {
                visited += 1;
                if visited > CAPACITY {
                    return Err(refuse("too many task changes to check without rebuilding"));
                }
                if !touched.insert(id) {
                    continue;
                }
                let arrival = observed.topic_inputs().member(id);
                if let Some(prior) = self.inputs.member(id) {
                    if arrival != Some(prior) {
                        return Err(refuse("a prior member left or arrived again"));
                    }
                    if let Some(seat) = self.window.iter().position(|h| &h.id == id) {
                        let head = observed
                            .task_heads(id)
                            .expect("indexed topic input")
                            .statement()
                            .body();
                        drift += usize::from(!self.holds(seat, head));
                    }
                } else {
                    drift += usize::from(arrival.is_some());
                }
                // Permit one changed or missing input only when unchanged
                // retained evidence still outnumbers it. This is a reuse
                // budget, not a claim that clustering is invariant to edits.
                if drift > 1 || (drift != 0 && self.window.len() <= drift * 2) {
                    return Err(refuse("too little unchanged evidence in this tree"));
                }
            }
        }
        Ok(())
    }
}
