//! Meter reducer tests keep announcement suppression and lease posture reviewable.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str) -> PathBuf {
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "spice-session-meter-{name}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("fixture directory");
    path
}

fn state(remaining: Option<u64>) -> WorkingState {
    WorkingState {
        pending_inbox_count: 1,
        drain_queue: false,
        claim: Some(ClaimState {
            handle: "METER-00000001".to_owned(),
            phase: "todo".to_owned(),
            elapsed: Some(Duration::from_secs(90)),
            remaining: remaining.map(Duration::from_secs),
            rolling: false,
        }),
        last_taste: "fallbacks".to_owned(),
        narration: None,
    }
}

#[test]
fn announcement_identity_survives_exit_and_boundary_changes_speak() {
    let root = fixture("durable");
    let path = root.join("working-state.json");
    let first = Meter::new(&path)
        .evaluate(&state(Some(599)), Duration::from_secs(1))
        .expect("first process speaks");
    let next_process = Meter::new(&path)
        .evaluate(&state(Some(598)), Duration::from_secs(2))
        .expect("next process reads durable state");
    let next_boundary = Meter::new(&path)
        .evaluate(&state(Some(299)), Duration::from_secs(3))
        .expect("boundary transition evaluates");

    assert!(first.announcement.is_some());
    assert_eq!(next_process.announcement, None);
    assert!(next_boundary.announcement.is_some());

    fs::write(&path, b"not-json").expect("corrupt prior announcement");
    let recovered = Meter::new(&path)
        .evaluate(&state(Some(299)), Duration::from_secs(4))
        .expect("malformed state is absence");
    assert!(recovered.announcement.is_some());

    let live_path = root.join("time-sensitive.json");
    let live = Meter::new(&live_path);
    let initial_boundary = live
        .evaluate_time_sensitive(&state(Some(599)), Duration::from_secs(10))
        .expect("initial live boundary evaluates");
    let quiet_renewal = live
        .evaluate_time_sensitive(&state(Some(700)), Duration::from_secs(11))
        .expect("quiet live renewal evaluates");
    let next_cycle = live
        .evaluate_time_sensitive(&state(Some(599)), Duration::from_secs(12))
        .expect("next live countdown cycle evaluates");
    assert!(initial_boundary.announcement.is_some());
    assert_eq!(quiet_renewal.announcement, None);
    assert!(next_cycle.announcement.is_some());
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn expiry_refresh_targets_only_the_next_boundary() {
    let root = fixture("boundaries");
    let cases = [
        (601, Some(Duration::from_secs(2))),
        (599, Some(Duration::from_secs(300))),
        (299, Some(Duration::from_secs(240))),
        (59, None),
        (0, None),
    ];
    for (index, (remaining, expected)) in cases.into_iter().enumerate() {
        let meter = Meter::new(root.join(format!("state-{index}.json")));
        let decision = meter
            .evaluate(&state(Some(remaining)), Duration::from_secs(1))
            .expect("boundary evaluates");
        assert_eq!(decision.refresh_after, expected);
        if let Some(delay) = decision.refresh_after {
            assert!(!delay.is_zero());
        }
    }
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn lease_boundaries_are_strict_and_renewal_transitions_speak_once() {
    let root = fixture("strict-boundaries");

    let path = root.join("renewals.json");
    let meter = Meter::new(&path);
    let final_boundary = meter
        .evaluate(&state(Some(59)), Duration::from_secs(10))
        .expect("final boundary evaluates");
    let same_band = meter
        .evaluate(&state(Some(58)), Duration::from_secs(11))
        .expect("same band evaluates");
    let middle_boundary = meter
        .evaluate(&state(Some(299)), Duration::from_secs(12))
        .expect("final to middle boundary evaluates");
    let first_boundary = meter
        .evaluate(&state(Some(599)), Duration::from_secs(13))
        .expect("middle to first boundary evaluates");
    let quiet_state = state(Some(600));
    let quiet = meter
        .evaluate(&quiet_state, Duration::from_secs(16))
        .expect("countdown exit evaluates");
    let renewed_state = state(Some(700));
    let renewed = meter
        .evaluate(&renewed_state, Duration::from_secs(17))
        .expect("renewed lease evaluates");

    assert!(final_boundary.announcement.is_some());
    assert_eq!(same_band.announcement, None);
    assert!(middle_boundary.announcement.is_some());
    assert!(first_boundary.announcement.is_some());
    assert!(quiet.announcement.is_some());
    assert_eq!(renewed.announcement, None);
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn rolled_lease_names_its_roll_and_only_warns_once_the_roll_stops() {
    let root = fixture("rolling");
    let meter = Meter::new(root.join("working-state.json"));
    let mut rolling = state(Some(88));
    let claim = rolling.claim.as_mut().expect("fixture claim");
    claim.rolling = true;

    let decision = meter
        .evaluate(&rolling, Duration::from_secs(1))
        .expect("rolling lease evaluates");
    let line = decision.announcement.expect("new state speaks once");
    assert!(!line.contains("spice task free"));
    assert_eq!(
        decision.refresh_after,
        Some(Duration::from_secs(29)),
        "a rolled lease is only watched for the band that proves its roll stopped",
    );

    let mut rolled_again = rolling.clone();
    rolled_again
        .claim
        .as_mut()
        .expect("fixture claim")
        .remaining = Some(Duration::from_secs(300));
    assert_eq!(
        meter
            .evaluate(&rolled_again, Duration::from_secs(2))
            .expect("re-rolled lease evaluates")
            .announcement,
        None,
        "a rolling lease has no countdown bands to announce",
    );

    let mut stalled = rolling.clone();
    stalled.claim.as_mut().expect("fixture claim").remaining = Some(Duration::from_secs(12));
    let warned = meter
        .evaluate(&stalled, Duration::from_secs(3))
        .expect("stalled roll evaluates");
    assert!(warned.announcement.is_some());
    assert_eq!(warned.refresh_after, None);
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn no_claim_keeps_durable_suppression_for_same_key_reacquisition() {
    let root = fixture("reacquisition");
    let path = root.join("working-state.json");
    let meter = Meter::new(&path);
    let first = meter
        .evaluate(&state(Some(599)), Duration::from_secs(10))
        .expect("boundary speaks");
    let stored = fs::read(&path).expect("suppression state reads");
    let absent = meter
        .evaluate(&WorkingState::default(), Duration::from_secs(11))
        .expect("no claim is silent");
    let reacquired = Meter::new(&path)
        .evaluate(&state(Some(598)), Duration::from_secs(12))
        .expect("fresh process honors durable suppression");

    assert!(first.announcement.is_some());
    assert_eq!(absent, Decision::default());
    assert_eq!(fs::read(&path).expect("state remains"), stored);
    assert_eq!(reacquired.announcement, None);
    assert_eq!(reacquired.refresh_after, Some(Duration::from_secs(299)));
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn invalid_usage_time_is_omitted_and_unknown_context_stays_unknown() {
    assert_eq!(
        ContextSnapshot::admit("not-a-timestamp", Some(200_000)),
        None
    );
    let snapshot =
        ContextSnapshot::admit("2026-08-08T16:00:00Z", None).expect("valid usage timestamp");

    assert_eq!(snapshot.at.to_string(), "2026-08-08T16:00:00.000Z");
    assert_eq!(snapshot.model_context_window, None);
}

#[test]
fn an_open_prod_speaks_once_per_prod_and_falls_silent_once_cleared() {
    let root = fixture("narration");
    let path = root.join("working-state.json");
    let meter = Meter::new(&path);
    let prodded = |count: u32| WorkingState {
        narration: Some(NarrationState {
            moments: 24 * count,
            count,
        }),
        ..WorkingState::default()
    };

    // A prod alone is signal: nothing else in the snapshot is set.
    assert!(prodded(1).has_signal());
    let first = meter
        .evaluate(&prodded(1), Duration::from_secs(1))
        .expect("first prod evaluates");
    assert!(first.announcement.is_some());
    assert_eq!(first.refresh_after, None);
    // The same prod at the next boundary is the same key: no repeat.
    let again = meter
        .evaluate(&prodded(1), Duration::from_secs(2))
        .expect("same prod evaluates");
    assert_eq!(again.announcement, None);
    // The next prod in the streak changes the key and names its ordinal.
    let second = meter
        .evaluate(&prodded(2), Duration::from_secs(3))
        .expect("second prod evaluates");
    assert!(second.announcement.is_some());
    // Once the resident clears the streak the snapshot carries no signal.
    let cleared = meter
        .evaluate(&WorkingState::default(), Duration::from_secs(4))
        .expect("cleared state evaluates");
    assert_eq!(cleared, Decision::default());
    assert!(!WorkingState::default().has_signal());

    fs::remove_dir_all(root).expect("fixture cleanup");
}
