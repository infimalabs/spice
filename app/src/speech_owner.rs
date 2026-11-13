//! Playback ownership independent of browser media APIs.

/// The one generation ledger that decides which speech request owns playback.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct PlaybackOwnership {
    generation: u64,
}

impl PlaybackOwnership {
    /// Invalidate the prior owner and return the replacement generation.
    pub fn begin(&mut self) -> u64 {
        self.stop();
        self.generation
    }

    /// Whether asynchronous work still belongs to the current owner.
    #[must_use]
    pub const fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// Invalidate the current owner without selecting a replacement.
    pub fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Callback admission within the current playback generation.
#[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
#[derive(Default)]
pub(crate) struct PlaybackProgress {
    generation: std::cell::Cell<u64>,
    next_chunk: std::cell::Cell<usize>,
    voice_publication: std::cell::Cell<bool>,
}

#[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
impl PlaybackProgress {
    pub(crate) fn begin(&self, generation: u64) {
        self.generation.set(generation);
        self.next_chunk.set(0);
        self.voice_publication.set(false);
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Open one event admission for this current playback generation.
    pub(crate) fn arm_voice_publication(&self, generation: u64) {
        if self.generation() == generation {
            self.voice_publication.set(true);
        }
    }

    /// Consume one voice-list event before its deferred continuation starts.
    pub(crate) fn claim_voice_publication(&self, generation: u64) -> bool {
        if self.generation() != generation || !self.voice_publication.get() {
            return false;
        }
        self.voice_publication.set(false);
        true
    }

    pub(crate) fn awaits_voice_publication(&self, generation: u64) -> bool {
        self.generation() == generation && self.voice_publication.get()
    }

    /// Consume this chunk before its callback may enqueue a successor.
    pub(crate) fn advance(&self, generation: u64, chunk: usize) -> bool {
        if self.generation() != generation || self.next_chunk.get() != chunk {
            return false;
        }
        self.next_chunk.set(chunk + 1);
        true
    }

    /// Admit one terminal result without letting stale callbacks mutate playback.
    pub(crate) fn finish(&self, generation: u64) -> bool {
        if self.generation() != generation {
            return false;
        }
        self.stop();
        true
    }

    pub(crate) fn stop(&self) {
        self.generation.set(self.generation().wrapping_add(1));
        self.next_chunk.set(0);
        self.voice_publication.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaybackOwnership, PlaybackProgress};

    #[test]
    fn only_the_current_chunk_can_advance_playback_once() {
        let progress = PlaybackProgress::default();
        progress.begin(1);
        assert!(!progress.advance(1, 1));
        assert!(progress.advance(1, 0));
        assert!(!progress.advance(1, 0));
        assert!(!progress.advance(1, 2));
        assert!(progress.advance(1, 1));
    }

    #[test]
    fn replacement_stop_and_terminal_events_retire_only_their_own_chain() {
        let mut ownership = PlaybackOwnership::default();
        let progress = PlaybackProgress::default();
        let first = ownership.begin();
        progress.begin(first);
        assert!(progress.advance(first, 0));

        let replacement = ownership.begin();
        progress.begin(replacement);
        assert!(!progress.finish(first));
        assert!(!progress.advance(first, 1));
        assert!(progress.advance(replacement, 0));
        assert!(progress.finish(replacement));
        assert!(!progress.finish(replacement));
        assert!(!progress.advance(replacement, 1));

        let stopped = ownership.begin();
        progress.begin(stopped);
        ownership.stop();
        progress.stop();
        assert!(!progress.advance(stopped, 0));
        assert!(!progress.finish(stopped));
    }

    #[test]
    fn one_voice_publication_event_claims_each_armed_generation() {
        let progress = PlaybackProgress::default();
        progress.begin(1);
        progress.arm_voice_publication(1);
        assert!(progress.awaits_voice_publication(1));
        assert!(progress.claim_voice_publication(1));
        assert!(!progress.awaits_voice_publication(1));
        assert!(!progress.claim_voice_publication(1));

        progress.arm_voice_publication(1);
        assert!(progress.claim_voice_publication(1));
        progress.begin(2);
        assert!(!progress.claim_voice_publication(1));
        assert!(!progress.claim_voice_publication(2));

        progress.arm_voice_publication(2);
        progress.stop();
        assert!(!progress.claim_voice_publication(2));
    }
}
