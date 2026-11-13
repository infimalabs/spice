use super::*;

const SCRIPT: &str = "\
# A complete scenario.
0 steering Received.
0:01.5 call read {\"path\": \"context.txt\"}
1:02.25 say Compact this turn.
1:02.250 compact The summary.
0:01:03 say Resumed.
";

fn due(plan: &Plan) -> Vec<u64> {
    plan.cues().iter().map(|cue| cue.due_ms).collect()
}

#[test]
fn every_event_parses_on_the_base_clock_and_the_rate_scales_each_interval() {
    let plan = Plan::parse(SCRIPT, Rate::Scaled(1.0)).expect("script parses");
    assert_eq!(due(&plan), [0, 1_500, 62_250, 62_250, 63_000]);
    assert_eq!(
        plan.cues()
            .iter()
            .map(|cue| (cue.line, cue.event.clone()))
            .collect::<Vec<_>>(),
        [
            (
                2,
                Event::Steering {
                    text: "Received.".to_owned()
                }
            ),
            (
                3,
                Event::Call {
                    name: "read".to_owned(),
                    arguments: serde_json::from_str("{\"path\": \"context.txt\"}")
                        .expect("fixture arguments"),
                }
            ),
            (
                4,
                Event::Say {
                    text: "Compact this turn.".to_owned()
                }
            ),
            (
                5,
                Event::Compact {
                    summary: "The summary.".to_owned()
                }
            ),
            (
                6,
                Event::Say {
                    text: "Resumed.".to_owned()
                }
            ),
        ]
    );

    let faster = Plan::parse(SCRIPT, Rate::Scaled(2.0)).expect("script parses");
    assert_eq!(due(&faster), [0, 750, 31_125, 31_125, 31_500]);
    let slower = Plan::parse(SCRIPT, Rate::Scaled(0.5)).expect("script parses");
    assert_eq!(due(&slower), [0, 3_000, 124_500, 124_500, 126_000]);
    let instant = Plan::parse(SCRIPT, Rate::Instant).expect("script parses");
    assert_eq!(due(&instant), [0, 0, 0, 0, 0]);
    let huge = Plan::parse("1 say late", Rate::Scaled(f64::MIN_POSITIVE)).expect("script parses");
    assert_eq!(due(&huge), [u64::MAX]);
}

#[test]
fn a_malformed_cue_refuses_naming_its_line() {
    for (script, line) in [
        ("0 say a\n\n1 shout b", 3),
        ("0 say a\n0:60 say b", 2),
        ("1:0 say a", 1),
        ("0.1234 say a", 1),
        ("1. say a", 1),
        ("-1 say a", 1),
        ("say a", 1),
        ("2 say a\n1 say b", 2),
        ("0 say", 1),
        ("0 steering", 1),
        ("0 call read [1]", 1),
        ("0 call read {", 1),
        ("0 call read", 1),
        ("0 compact", 1),
        ("0 compact summary\n1 say resumed", 1),
        ("0 call read {}\n1 compact summary", 2),
    ] {
        match Plan::parse(script, Rate::Scaled(1.0)) {
            Err(ScriptError::Line { line: refused, .. }) => {
                assert_eq!(refused, line, "{script:?}");
            }
            other => panic!("{script:?} was not refused by line: {other:?}"),
        }
    }
    assert_eq!(
        Plan::parse("# nothing\n\n", Rate::Instant),
        Err(ScriptError::Empty)
    );
}

#[test]
fn the_model_slot_splits_its_rate_on_the_last_separator() {
    assert_eq!(
        Selection::parse("cues/demo.cue"),
        Ok(Selection {
            path: "cues/demo.cue",
            rate: Rate::Scaled(1.0)
        })
    );
    assert_eq!(
        Selection::parse("at@home/demo.cue@instant"),
        Ok(Selection {
            path: "at@home/demo.cue",
            rate: Rate::Instant
        })
    );
    assert_eq!(
        Selection::parse("demo.cue@2.5"),
        Ok(Selection {
            path: "demo.cue",
            rate: Rate::Scaled(2.5)
        })
    );
    for refused in [
        "at@home/demo.cue",
        "demo.cue@0",
        "demo.cue@-1",
        "demo.cue@inf",
        "demo.cue@NaN",
        "demo.cue@",
        "@instant",
        "",
    ] {
        assert!(
            matches!(Selection::parse(refused), Err(ScriptError::Selection(_))),
            "{refused:?}"
        );
    }
}
