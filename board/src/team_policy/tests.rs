use super::*;
use crate::routing::RoutingRoots;
use wire::{CanonicalUuid, Phase, Topic};

mod oops;
mod private;
mod unplaced;

fn config(json: &str) -> Config {
    wire::decode(json.as_bytes()).unwrap()
}
fn team(n: u8, name: Option<&str>) -> Team {
    Team {
        id: format!("018f82d4-6f59-7d06-92e3-bce9131163{n:02x}")
            .parse()
            .unwrap(),
        name: name.map(str::to_owned),
        drive: true,
        drain: true,
        lanes: vec![
            format!("018f82d4-6f59-7d06-92e3-bce9131164{n:02x}")
                .parse()
                .unwrap(),
        ],
    }
}
fn owners(
    config: &Config,
    teams: &[Team],
    topic: &str,
    phase: &str,
    creator: Option<CanonicalUuid>,
) -> Ownership {
    Resolver::new(config, teams).owners(Task {
        topic: Some(&topic.parse::<Topic>().unwrap()),
        phase: &phase.parse::<Phase>().unwrap(),
        creator,
    })
}
fn drive(
    config: &Config,
    teams: &[Team],
    topic: &str,
    phase: &str,
    creator: Option<CanonicalUuid>,
) -> bool {
    Resolver::new(config, teams).drive(
        teams[0].id,
        Task {
            topic: Some(&topic.parse::<Topic>().unwrap()),
            phase: &phase.parse::<Phase>().unwrap(),
            creator,
        },
    )
}

#[test]
fn named_policies_do_not_inherit_and_omitted_drain_follows_local_drive() {
    let cfg =
        config(r#"{"default":{"drive":[]},"open":{},"limited":{"drive":[{"topic":["/one"]}]}}"#);
    let unnamed = [team(1, None)];
    let open = [team(2, Some("open"))];
    let limited = [team(3, Some("limited"))];
    assert!(!drive(&cfg, &unnamed, "/one", "todo", None));
    assert!(drive(&cfg, &open, "/any", "todo", None));
    assert_eq!(
        owners(&cfg, &open, "/any", "todo", None).teams,
        vec![open[0].id]
    );
    assert_eq!(
        owners(&cfg, &limited, "/one", "todo", None).teams,
        vec![limited[0].id]
    );
    assert!(
        owners(&cfg, &limited, "/other", "todo", None)
            .teams
            .is_empty()
    );
    assert!(
        owners(&cfg, &[team(4, Some("missing"))], "/one", "todo", None)
            .teams
            .is_empty()
    );
    assert_eq!(
        owners(&Config::default(), &unnamed, "/one", "todo", None).teams,
        vec![unnamed[0].id]
    );
}

#[test]
fn raw_globs_keep_component_boundaries_without_implicit_parent_matches() {
    let teams = [team(1, None)];
    let shallow = config(r#"{"default":{"drive":[{"topic":["/spice/*"]}]}}"#);
    let deep = config(r#"{"default":{"drive":[{"topic":["/spice/**"]}]}}"#);
    assert!(drive(&shallow, &teams, "/spice/app", "todo", None));
    assert!(!drive(&shallow, &teams, "/spice/app/cli", "todo", None));
    assert!(!drive(&shallow, &teams, "/spice", "todo", None));
    assert!(drive(&deep, &teams, "/spice/app/cli", "todo", None));
    assert!(!drive(&deep, &teams, "/spice", "todo", None));
}

#[test]
fn rule_fields_intersect_and_creator_names_resolve_to_current_bindings() {
    let cfg = config(
        r#"{"default":{"drive":[{"topic":["/one","/two"],"phase":["todo","plan"],"team":["self","source"]},{"topic":["/public"]}]}}"#,
    );
    let mut teams = [team(1, None), team(2, Some("source"))];
    assert!(drive(&cfg, &teams, "/two", "plan", Some(teams[0].id)));
    assert!(drive(&cfg, &teams, "/one", "todo", Some(teams[1].id)));
    assert!(!drive(&cfg, &teams, "/one", "review", Some(teams[1].id)));
    assert!(!drive(&cfg, &teams, "/one", "todo", None));
    assert!(drive(&cfg, &teams, "/public", "review", None));
    teams[1].name = None;
    assert!(!drive(&cfg, &teams, "/one", "todo", Some(teams[1].id)));
    teams[0].drive = false;
    assert!(!drive(&cfg, &teams, "/public", "todo", None));
    let empty = config(r#"{"default":{"drive":[{"topic":[]}]}}"#);
    teams[0].drive = true;
    assert!(!drive(&empty, &teams, "/public", "todo", None));
}

#[test]
fn reservation_outranks_listeners_but_review_uses_ordinary_drain_rules() {
    let cfg = config(
        r#"{"reserved":{"drive":[{"topic":["/spice/**"],"exclusive":true}]},"specific":{"drain":[{"topic":["/spice/app/*"]}]}}"#,
    );
    let mut teams = [team(1, Some("reserved")), team(2, Some("specific"))];
    teams[0].drain = false;
    let selected = owners(&cfg, &teams, "/spice/app/cli", "todo", None);
    assert!(selected.reserved);
    assert_eq!(selected.teams, vec![teams[0].id]);
    let reviewed = owners(&cfg, &teams, "/spice/app/cli", "review", None);
    assert!(!reviewed.reserved);
    assert_eq!(reviewed.teams, vec![teams[1].id]);
    teams[0].lanes.clear();
    assert_eq!(
        owners(&cfg, &teams, "/spice/app/cli", "todo", None).teams,
        vec![teams[1].id]
    );
    teams[1].drain = false;
    assert!(
        owners(&cfg, &teams, "/spice/app/cli", "todo", None)
            .teams
            .is_empty()
    );
}

#[test]
fn literal_prefix_precedes_field_count_and_equal_scores_share() {
    let cfg = config(
        r#"{"wide":{"drain":[{"topic":["/spice/**"],"phase":["todo"],"team":["self"]}]},"narrow":{"drain":[{"topic":["/spice/app/*"]}]}}"#,
    );
    let teams = [
        team(1, Some("wide")),
        team(2, Some("narrow")),
        team(3, None),
    ];
    assert_eq!(
        owners(&cfg, &teams, "/spice/app/cli", "todo", Some(teams[0].id)).teams,
        vec![teams[1].id]
    );
    let tied = [team(1, None), team(2, None)];
    assert_eq!(
        owners(&Config::default(), &tied, "/any", "todo", None).teams,
        vec![tied[0].id, tied[1].id]
    );
}

#[test]
fn policy_queries_ignore_runtime_switches_but_require_live_admission() {
    let cfg = config(
        r#"{"default":{"drive":[{"topic":["/drive/**"]}],"drain":[{"topic":["/drain/**"]}]}}"#,
    );
    let mut teams = [team(1, None)];
    teams[0].drive = false;
    teams[0].drain = false;
    let drive_topic: Topic = "/drive/work".parse().unwrap();
    let drain_topic: Topic = "/drain/work".parse().unwrap();
    let phase: Phase = "todo".parse().unwrap();
    let drive_task = Task {
        topic: Some(&drive_topic),
        phase: &phase,
        creator: None,
    };
    let drain_task = Task {
        topic: Some(&drain_topic),
        phase: &phase,
        creator: None,
    };
    let resolver = Resolver::new(&cfg, &teams);
    assert!(resolver.drive_policy(teams[0].id, drive_task));
    assert!(!resolver.drive(teams[0].id, drive_task));
    assert!(resolver.drain_policy(teams[0].id, drain_task));
    assert!(!resolver.drain(teams[0].id, drain_task));
    assert!(!resolver.drive_policy(teams[0].id, drain_task));
    assert!(!resolver.drain_policy(teams[0].id, drive_task));

    teams[0].lanes.clear();
    let resolver = Resolver::new(&cfg, &teams);
    assert!(!resolver.drive_policy(teams[0].id, drive_task));
    assert!(!resolver.drain_policy(teams[0].id, drain_task));
}

#[test]
fn invalid_rule_shapes_and_default_exclusivity_refuse_at_admission() {
    for json in [
        r#"{"default":{"drive":false}}"#,
        r#"{"default":{"drive":[{"exclusive":true}]}}"#,
        r#"{"default":{"drain":[{"exclusive":false}]}}"#,
        r#"{"named":{"drive":[{"topic":["["]}]}}"#,
        r#"{"named":{"drive":[{"topic":"/one"}]}}"#,
        r#"{"named":{"drive":[{"unexpected":true}]}}"#,
        r#"{"named":{"drive":[{"team":["default"]}]}}"#,
    ] {
        assert!(wire::decode::<Config>(json.as_bytes()).is_err(), "{json}");
    }
    assert!(validate_name("default").is_err());
    assert!(validate_name("").is_err());
}

#[test]
fn the_last_matching_topic_pattern_decides_and_a_trailing_slash_covers_the_subtree() {
    let teams = [team(1, Some("chosen"))];
    let chosen = |topics: &str| {
        config(&format!(
            r#"{{"chosen":{{"drive":[{{"topic":{topics}}}]}}}}"#
        ))
    };
    let admits = |topics: &str, topic: &str| drive(&chosen(topics), &teams, topic, "todo", None);
    let roots = |topics: &str| {
        let policy = chosen(topics);
        policy
            .named("chosen")
            .unwrap()
            .routing_scope()
            .unwrap()
            .roots
    };

    let ordered = r#"["/spice/","!/spice/app/","/spice/app/cli"]"#;
    for (topic, admitted) in [
        ("/spice", true),
        ("/spice/web/x", true),
        ("/spicey", false),
        ("/spice/app", false),
        ("/spice/app/x", false),
        ("/spice/app/cli", true),
    ] {
        assert_eq!(admits(ordered, topic), admitted, "{topic}");
    }
    let excluded_last = r#"["/spice/app/cli","/spice/","!/spice/app/"]"#;
    assert!(!admits(excluded_last, "/spice/app/cli"));
    assert_eq!(
        roots(r#"["/spice/","!/other/"]"#),
        RoutingRoots::Selected(vec!["/spice".parse().unwrap()])
    );

    // Exclusions alone subtract from everything, as a leading `**` would.
    let orphan = format!("{}/task/a", team(9, None).id);
    for (topic, admitted) in [
        ("/other", true),
        (orphan.as_str(), true),
        ("/spice/app", false),
        ("/spice/app/x", false),
    ] {
        for topics in [r#"["!/spice/app/"]"#, r#"["**","!/spice/app/"]"#] {
            assert_eq!(admits(topics, topic), admitted, "{topics} {topic}");
        }
    }
    assert_eq!(roots(r#"["!/spice/app/"]"#), RoutingRoots::All);
}
