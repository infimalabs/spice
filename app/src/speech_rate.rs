//! Conversion between configured words per minute and the browser's relative
//! system-speech rate.

/// The installed words-per-minute target represented by the browser's native
/// rate. Web Speech exposes a dimensionless multiplier rather than measured
/// words per minute, so the installed baseline must map to `1.0` instead of
/// being accelerated a second time.
const REFERENCE_WORDS_PER_MINUTE: f32 = 200.0;
const MIN_SYSTEM_RATE: f32 = 0.5;
const MAX_SYSTEM_RATE: f32 = 2.0;

/// Project a configured words-per-minute target into Web Speech's relative
/// rate, preserving the browser API bounds used by the playback controller.
pub(crate) fn system_rate(words_per_minute: u32) -> f32 {
    (words_per_minute as f32 / REFERENCE_WORDS_PER_MINUTE).clamp(MIN_SYSTEM_RATE, MAX_SYSTEM_RATE)
}

#[cfg(test)]
mod tests {
    use super::system_rate;

    #[test]
    fn installed_baseline_and_browser_bounds_have_one_conversion() {
        assert_eq!(system_rate(200), 1.0);
        assert_eq!(system_rate(100), 0.5);
        assert_eq!(system_rate(400), 2.0);
        assert_eq!(system_rate(1), 0.5);
        assert_eq!(system_rate(1_000), 2.0);
    }
}
