use app::speech_owner::PlaybackOwnership;

#[test]
fn a_second_begin_invalidates_the_first_playback_owner() {
    let mut ownership = PlaybackOwnership::default();

    let first = ownership.begin();
    assert!(ownership.is_current(first));

    let second = ownership.begin();
    assert_ne!(second, first);
    assert!(!ownership.is_current(first));
    assert!(ownership.is_current(second));
}

#[test]
fn repeated_stop_never_revives_an_invalidated_owner() {
    let mut ownership = PlaybackOwnership::default();
    let first = ownership.begin();
    let second = ownership.begin();

    ownership.stop();
    assert!(!ownership.is_current(first));
    assert!(!ownership.is_current(second));

    ownership.stop();
    assert!(!ownership.is_current(first));
    assert!(!ownership.is_current(second));
}
