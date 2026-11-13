//! A blocked job wait as the lane's current activity: each in-flight wait's
//! latest progress is written as a partial result of its call and restated
//! whenever another call settles, so the wait, not that settled call, stays
//! the newest activity until its own answer.

use std::io::Write;
use std::sync::PoisonError;

use listen::{Decoded, Event, ToolMoment};

use super::super::terminal::Report;
use super::Driver;

impl<W: Write + Send + 'static> Driver<W> {
    /// Write one in-flight wait's progress and keep it for restatement.
    pub(super) fn report_wait(&self, id: &str, report: &Report) {
        let mut waits = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        let _updated = self.write(&vendor::omp::host_tool_update(id, &report.output));
        waits.insert(id.to_owned(), report.output.clone());
    }

    /// Stop restating one call before its answer is written.
    pub(super) fn end_wait(&self, id: &str) {
        self.waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }

    /// Restate each in-flight wait's latest progress after another tool call
    /// settles.
    pub(super) fn restate_waits(&self, decoded: &Decoded) {
        let waits = self.waits.lock().unwrap_or_else(PoisonError::into_inner);
        let settled = decoded.events().iter().any(|event| {
            matches!(event, Event::Tool(tool)
                if matches!(tool.moment, ToolMoment::Result(_))
                    && !waits.contains_key(&tool.call_id))
        });
        if !settled {
            return;
        }
        for (id, output) in waits.iter() {
            let _restated = self.write(&vendor::omp::host_tool_update(id, output));
        }
    }
}
