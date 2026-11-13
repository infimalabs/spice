//! Private packet refusal asides for this occupant, which never wake an idle
//! occupant.

use std::io::Write;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use super::Driver;

impl<W: Write + Send + 'static> Driver<W> {
    /// Queue one refusal aside only into a running session. Activity and the
    /// write are one boundary, so an idle occupant, admitted or parked, is
    /// never woken: the notice waits for its next agent start instead.
    pub(super) fn refuse_quietly(&self, purpose: &str, message: &str) {
        let mut queued = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if queued
            .get(purpose)
            .is_some_and(|previous| previous == message)
        {
            return;
        }
        if !self.registration.holds_first_turn() {
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            let aside = vendor::omp::aside_request(sequence, purpose, message);
            if self.write(&aside).is_err() {
                drop(queued);
                self.quiet
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(purpose.to_owned(), message.to_owned());
                return;
            }
        }
        queued.insert(purpose.to_owned(), message.to_owned());
    }

    /// Queue every refusal an idle occupant could not take, once its next
    /// agent start shows it running.
    pub(super) fn release_quiet(&self) {
        let quiet = std::mem::take(&mut *self.quiet.lock().unwrap_or_else(PoisonError::into_inner));
        for (purpose, message) in quiet {
            self.refuse_quietly(&purpose, &message);
        }
    }
}
