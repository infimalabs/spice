//! Physical history progress shared by every consumer of one provider.
//!
//! A page anchor names what a reader last saw; a continuation names how far
//! the source was physically read to prove everything older lies behind it.
//! Continuations are facts about the frozen source rather than one consumer's
//! cursor, so reading one never consumes it and a second app paging the
//! same lane cannot evict the first.

use std::sync::Condvar;

use super::*;

/// Tape continuations one lane remembers, the app anchor memory's bound.
const CONTINUATIONS_PER_LANE: usize = 64;

type Chronology = (i64, CanonicalUuid, u64, u64, u64, u32);

#[derive(Debug, Default)]
pub(super) struct HistoryProgress {
    /// Per lane, most recently used last.
    tape: Mutex<HashMap<CanonicalUuid, Vec<TapeContinuation>>>,
    flights: Mutex<Vec<Arc<Flight>>>,
}

/// Every visible tape card older than `anchor` lies physically before `next`.
#[derive(Clone, Debug)]
struct TapeContinuation {
    anchor: wire::MessageLocus,
    chronology: Chronology,
    next: listen::Checkpoint,
}

/// One page read other identical requests may join while it runs.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FlightKey {
    pub(super) lane: CanonicalUuid,
    pub(super) anchor: Option<wire::MessageLocus>,
    pub(super) limit: usize,
    /// Physical tape progress, carrying the source identity it was read from.
    pub(super) tape: Option<listen::Checkpoint>,
}

type PageAnswer = Result<serve::stream::MessageHistoryPage, String>;

#[derive(Debug)]
struct Flight {
    key: FlightKey,
    answer: Mutex<Option<PageAnswer>>,
    ready: Condvar,
}

/// Publishes the leader's answer and retires the flight, even on unwind.
struct Landing<'a> {
    flights: &'a Mutex<Vec<Arc<Flight>>>,
    flight: &'a Arc<Flight>,
}

impl Landing<'_> {
    fn land(&self, answer: PageAnswer) {
        if let Ok(mut flights) = self.flights.lock() {
            flights.retain(|flight| !Arc::ptr_eq(flight, self.flight));
        }
        if let Ok(mut slot) = self.flight.answer.lock()
            && slot.is_none()
        {
            *slot = Some(answer);
        }
        self.flight.ready.notify_all();
    }
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        self.land(Err("durable history read was abandoned".to_owned()));
    }
}

impl HistoryProgress {
    /// Where the tape half of a page before `before` begins reading.
    ///
    /// A continuation issued for this anchor may already stand behind empty
    /// windows a sparse page crossed. Otherwise a tape anchor names its own
    /// position.
    pub(super) fn tape_start(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
    ) -> Result<Option<listen::Checkpoint>, String> {
        let Some(source_end) = source.source.as_ref() else {
            return Ok(None);
        };
        let Some(before) = before else {
            return Ok(Some(source_end.clone()));
        };
        // A tape anchor's own position is always a valid start.
        let own = Some(listen::Checkpoint {
            source: source_end.source.clone(),
            next: listen::Position {
                member: before.locus.member,
                offset: before.locus.offset,
            },
        });
        let mut tape = self.tape()?;
        let known = tape.entry(lane).or_default();
        let issued = known
            .iter()
            .position(|known| known.anchor == before.locus)
            .map(|index| {
                let continuation = known.remove(index);
                let next = continuation.next.clone();
                known.push(continuation);
                next
            });
        // Every continuation at or after this anchor also proves it; the
        // physically oldest start among valid ones reads the least.
        let chronology = before.chronology_key();
        let borrowed = || {
            known
                .iter()
                .filter(|known| known.chronology >= chronology)
                .min_by_key(|known| known.chronology)
                .map(|known| known.next.clone())
        };
        let start = [issued, own]
            .into_iter()
            .flatten()
            .filter(|start| start.source == source_end.source)
            .min_by_key(|start| start.next)
            .or_else(borrowed)
            .unwrap_or_else(|| source_end.clone());
        Ok(Some(start))
    }

    /// Remember tape progress proven for `anchor`, keeping the further one
    /// when two readers proved it for the same anchor.
    pub(super) fn remember_tape(
        &self,
        lane: CanonicalUuid,
        anchor: &wire::AgentMessage,
        next: listen::Checkpoint,
    ) -> Result<(), String> {
        let mut tape = self.tape()?;
        let known = tape.entry(lane).or_default();
        let next = match known.iter().position(|known| known.anchor == anchor.locus) {
            Some(index) => {
                let prior = known.remove(index).next;
                if prior.source == next.source && prior.next < next.next {
                    prior
                } else {
                    next
                }
            }
            None => next,
        };
        known.push(TapeContinuation {
            anchor: anchor.locus,
            chronology: anchor.chronology_key(),
            next,
        });
        if known.len() > CONTINUATIONS_PER_LANE {
            known.remove(0);
        }
        Ok(())
    }

    /// Drop a lane's tape progress after its source failed a read.
    pub(super) fn forget_tape(&self, lane: CanonicalUuid) {
        if let Ok(mut tape) = self.tape.lock() {
            tape.remove(&lane);
        }
    }

    fn tape(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<CanonicalUuid, Vec<TapeContinuation>>>, String>
    {
        self.tape
            .lock()
            .map_err(|_| "durable history continuation is unavailable".to_owned())
    }

    /// Run `read` once for every concurrent request with the same key.
    ///
    /// Two readers join only when anchor, limit, and physical tape progress
    /// with its source identity all match, which is exactly when their reads
    /// would decode the same records into the same page.
    pub(super) fn single_flight(
        &self,
        key: FlightKey,
        read: impl FnOnce() -> PageAnswer,
    ) -> PageAnswer {
        let (flight, leader) = {
            let mut flights = self
                .flights
                .lock()
                .map_err(|_| "durable history flights are unavailable".to_owned())?;
            match flights.iter().find(|flight| flight.key == key) {
                Some(flight) => (Arc::clone(flight), false),
                None => {
                    let flight = Arc::new(Flight {
                        key,
                        answer: Mutex::new(None),
                        ready: Condvar::new(),
                    });
                    flights.push(Arc::clone(&flight));
                    (flight, true)
                }
            }
        };
        if !leader {
            let unavailable = || "durable history flight is unavailable".to_owned();
            let answer = flight.answer.lock().map_err(|_| unavailable())?;
            let answer = flight
                .ready
                .wait_while(answer, |answer| answer.is_none())
                .map_err(|_| unavailable())?;
            return answer.clone().unwrap_or_else(|| Err(unavailable()));
        }
        let landing = Landing {
            flights: &self.flights,
            flight: &flight,
        };
        let answer = read();
        landing.land(answer.clone());
        answer
    }
}
