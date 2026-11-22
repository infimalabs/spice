use super::*;

#[test]
fn a_landing_lands_past_main_and_never_before_its_task_commit() {
    assert_eq!(
        landing_seconds(1_792_729_149, 1_792_728_997),
        Some(1_792_729_150),
        "an older task commit still advances main's clock"
    );
    assert_eq!(
        landing_seconds(1_792_729_149, 1_792_729_149),
        Some(1_792_729_150)
    );
    assert_eq!(
        landing_seconds(1_792_728_614, 1_792_729_149),
        Some(1_792_729_149),
        "a newer task commit keeps its own second"
    );
    assert_eq!(landing_seconds(i64::MAX, 0), None);
}

#[test]
fn a_landing_date_carries_the_zone_offset_in_force_at_that_second() {
    let newfoundland =
        jiff::tz::TimeZone::posix("NST3:30NDT,M3.2.0,M11.1.0").expect("valid POSIX zone");
    assert_eq!(
        git_date(1_768_000_000, &newfoundland).as_deref(),
        Some("@1768000000 -0330"),
        "winter keeps standard time and a negative half-hour offset"
    );
    assert_eq!(
        git_date(1_784_000_000, &newfoundland).as_deref(),
        Some("@1784000000 -0230"),
        "summer selects daylight time from the landing second itself"
    );
    let nepal = jiff::tz::TimeZone::fixed(
        jiff::tz::Offset::from_seconds(5 * 3600 + 45 * 60).expect("valid offset"),
    );
    assert_eq!(
        git_date(1_768_000_000, &nepal).as_deref(),
        Some("@1768000000 +0545")
    );
    assert_eq!(git_date(i64::MAX, &nepal), None);
}

#[test]
fn planning_publication_requires_a_clean_captured_opening() {
    let clean = GitBoundary {
        head: "opening-head".to_owned(),
        integrated: "opening-head".to_owned(),
        upstream: Some("origin/main".to_owned()),
        upstream_head: Some("opening-head".to_owned()),
        local_commits: 0,
    };

    for phase in ["plan", "design"] {
        assert!(require_planning_opening(phase, false, Some(&clean)).is_ok());

        let missing = require_planning_opening(phase, false, None)
            .expect_err("a planning claim without a captured opening must fail closed");
        assert_eq!(missing.reason(), "git::planning-implementation");

        let resolution = require_planning_opening(phase, true, Some(&clean))
            .expect_err("a planning claim in merge-resolution state must fail closed");
        assert_eq!(resolution.reason(), "git::planning-implementation");

        let mut committed = clean.clone();
        committed.local_commits = 1;
        let local = require_planning_opening(phase, false, Some(&committed))
            .expect_err("a planning claim opened with implementation history must fail closed");
        assert_eq!(local.reason(), "git::planning-implementation");
    }
    for phase in ["todo", "review"] {
        assert!(require_planning_opening(phase, true, None).is_ok());
    }
}
