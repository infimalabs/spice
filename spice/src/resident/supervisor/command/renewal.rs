//! Occupant-scoped renewal state on the coalescing private aside channel.

use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct Notice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) position: u64,
    pub(in crate::resident) requested: bool,
}

pub(in crate::resident) fn notify(paths: &RuntimePaths, lane: CanonicalUuid, notice: Notice) {
    send(paths, lane, &Kick::Renewal(notice));
}

impl<W: Write + Send + 'static> Driver<W> {
    pub(super) fn deliver_renewal(&self, notice: Notice) {
        let mut delivered = self.renewal.lock().unwrap_or_else(PoisonError::into_inner);
        if delivered.is_some_and(|previous| previous.position >= notice.position) {
            return;
        }
        if delivered.map_or(!notice.requested, |previous| {
            previous.requested == notice.requested
        }) {
            *delivered = Some(notice);
            return;
        }
        let message = if notice.requested {
            "<system-notice>\nAgent renewal requested. Complete or hand off the current phase cleanly, post a concise handoff, do not claim additional work, and finish this agent turn. This is a private lifecycle notification; no acknowledgment key or visible ACK/NAK is required.\n</system-notice>"
        } else {
            "<system-notice>\nAgent renewal cancelled. The renewal preparation instruction is withdrawn; continue under the current task and lane authority rather than ending this turn for renewal. This is a private lifecycle notification; no acknowledgment key or visible ACK/NAK is required.\n</system-notice>"
        };
        if self.background("renewal", message).is_ok() {
            *delivered = Some(notice);
        }
    }
}

#[cfg(test)]
mod tests;
