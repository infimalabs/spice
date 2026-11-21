use super::*;

#[test]
fn composed_tail_consumes_the_longest_exact_boundary_once() {
    let prefix: Topic = "/app/css".parse().unwrap();
    let tail = |s: &str| match s.parse::<wire::TopicInput>().unwrap() {
        wire::TopicInput::Tail(tail) => tail,
        _ => panic!("relative tail"),
    };
    assert_eq!(
        compose(&prefix, &tail("app/css/theme")).as_str(),
        "/app/css/theme"
    );
    assert_eq!(compose(&prefix, &tail("css")), prefix);
    assert_eq!(
        compose(&prefix, &tail("app/theme")).as_str(),
        "/app/css/app/theme"
    );
}

#[test]
fn drive_roots_follow_literal_prefixes_without_using_phase_or_runtime_admission() {
    let rules: crate::team_policy::Rules = wire::decode(
        br#"[
        {"topic":["/engine/*/parser","/storage/**"],"phase":["review"]},
        {"topic":["/engine/front-end"]}
    ]"#,
    )
    .unwrap();
    assert_eq!(
        rules.routing_roots().unwrap(),
        RoutingRoots::Selected(vec![
            "/engine".parse().unwrap(),
            "/engine/front-end".parse().unwrap(),
            "/storage".parse().unwrap()
        ])
    );
    let unrestricted: crate::team_policy::Rules = wire::decode(br#"[{"phase":[]}]"#).unwrap();
    assert_eq!(unrestricted.routing_roots().unwrap(), RoutingRoots::All);
    let empty: crate::team_policy::Rules = wire::decode(br#"[{"topic":[]}]"#).unwrap();
    assert_eq!(
        empty.routing_roots().unwrap(),
        RoutingRoots::Selected(vec![])
    );
}

#[test]
fn drive_globs_without_a_literal_prefix_keep_public_roots_reachable() {
    for pattern in ["**", "**/parser", "{/engine,/storage}/**"] {
        let rules: crate::team_policy::Rules =
            wire::decode(format!(r#"[{{"topic":["{pattern}"]}}]"#).as_bytes()).unwrap();
        assert_eq!(
            rules.routing_roots().unwrap(),
            RoutingRoots::All,
            "{pattern}"
        );
    }
}

#[test]
fn policy_unmatched_must_be_public_and_outside_oops() {
    let policy = |text: &str| -> Result<Scope, RouteError> {
        let policy: crate::team_policy::Policy = wire::decode(text.as_bytes()).unwrap();
        policy.routing_scope()
    };
    assert_eq!(
        policy(r#"{"drive":[{"topic":["/engine/**"]}],"unmatched":"/engine/triage-intake"}"#)
            .unwrap(),
        Scope {
            roots: RoutingRoots::Selected(vec!["/engine".parse().unwrap()]),
            unmatched: Some("/engine/triage-intake".parse().unwrap()),
        }
    );
    for invalid in [
        r#"{"unmatched":"/oops/triage"}"#,
        r#"{"unmatched":"018f82d4-6f59-7d06-92e3-bce913116411"}"#,
    ] {
        assert!(
            matches!(policy(invalid), Err(RouteError::InvalidUnmatched(_))),
            "{invalid}"
        );
    }
    assert!(
        wire::decode::<crate::team_policy::Config>(br#"{"default":{"unmatched":"/oops"}}"#)
            .is_err(),
        "an invalid unmatched topic refuses the whole configuration"
    );
}

#[test]
fn automatic_placement_lead_is_a_whole_percentage() {
    let parse = |value: &str| {
        wire::decode::<crate::team_policy::Config>(
            format!(r#"{{"default":{{"placement_min_lead":{value}}}}}"#).as_bytes(),
        )
    };
    for value in [0, 1, 100] {
        assert_eq!(
            parse(&value.to_string())
                .unwrap()
                .default_policy()
                .placement_min_lead,
            value
        );
    }
    for value in ["-1", "101", "0.5"] {
        assert!(parse(value).is_err(), "{value}");
    }
}
