use super::*;

#[test]
fn private_identity_follows_lane_membership_before_team_identity() {
    let cfg = Config::default();
    for phase in ["todo", "review"] {
        let mut teams = [team(1, None), team(2, None)];
        let identity = teams[1].id;
        let topic = format!("{identity}/task/a");
        teams[0].lanes.push(identity);
        let selected = owners(&cfg, &teams, &topic, phase, None);
        assert_eq!(selected.teams, vec![teams[0].id]);
        assert!(selected.reserved);
        assert_eq!(
            owners(&cfg, &teams, &teams[0].id.to_string(), phase, None).teams,
            vec![teams[0].id]
        );

        teams[0].lanes.retain(|lane| *lane != identity);
        teams[1].lanes.push(identity);
        assert_eq!(
            owners(&cfg, &teams, &topic, phase, None).teams,
            vec![teams[1].id]
        );
        teams[1].lanes.retain(|lane| *lane != identity);
        assert_eq!(
            owners(&cfg, &teams, &topic, phase, None).teams,
            vec![teams[1].id]
        );
        teams[1].lanes.clear();
        let orphan = owners(&cfg, &teams, &topic, phase, None);
        assert!(!orphan.reserved);
        assert_eq!(orphan.teams, vec![teams[0].id]);
    }
}

#[test]
fn private_owner_switches_gate_work_without_public_rule_exemptions_for_other_teams() {
    let cfg = config(
        r#"{"default":{"drive":[],"drain":[]},"outsider":{"drive":[{"topic":["**"],"exclusive":true}]}}"#,
    );
    let mut teams = [team(1, None), team(2, Some("outsider"))];
    let topic: Topic = format!("{}/task/a", teams[0].lanes[0]).parse().unwrap();
    let phase: Phase = "review".parse().unwrap();
    let task = Task {
        topic: Some(&topic),
        phase: &phase,
        creator: Some(teams[1].id),
    };
    let resolver = Resolver::new(&cfg, &teams);
    assert_eq!(resolver.owners(task).teams, vec![teams[0].id]);
    assert!(resolver.drive(teams[0].id, task));
    assert!(resolver.drain(teams[0].id, task));
    assert!(!resolver.drive(teams[1].id, task));
    assert!(!resolver.drain(teams[1].id, task));

    teams[0].drain = false;
    let resolver = Resolver::new(&cfg, &teams);
    assert_eq!(resolver.owners(task).teams, vec![teams[0].id]);
    assert!(resolver.drive(teams[0].id, task));
    assert!(!resolver.drain(teams[0].id, task));
    assert!(!resolver.drain(teams[1].id, task));
    teams[0].drive = false;
    let resolver = Resolver::new(&cfg, &teams);
    assert!(!resolver.drive(teams[0].id, task));
    assert!(!resolver.drive(teams[1].id, task));
}

#[test]
fn orphan_private_topics_match_only_catch_all_topic_rules() {
    let topic = format!("{}/task/a", team(9, None).id);
    let teams = [team(1, Some("chosen"))];
    for pattern in [topic.as_str(), "018f*", "/**", "/public/**"] {
        let cfg = config(&format!(
            r#"{{"chosen":{{"drive":[{{"topic":["{pattern}"]}}]}}}}"#
        ));
        assert!(owners(&cfg, &teams, &topic, "todo", None).teams.is_empty());
        assert!(!drive(&cfg, &teams, &topic, "todo", None));
    }
    for rules in [
        "true",
        r#"[{"topic":["*"]}]"#,
        r#"[{"topic":["**"],"exclusive":true}]"#,
    ] {
        let cfg = config(&format!(r#"{{"chosen":{{"drive":{rules}}}}}"#));
        let selected = owners(&cfg, &teams, &topic, "todo", None);
        assert!(!selected.reserved);
        assert_eq!(selected.teams, vec![teams[0].id]);
        assert!(drive(&cfg, &teams, &topic, "todo", None));
    }
    let cfg = config(r#"{"chosen":{"drive":[{"topic":["**"],"phase":["todo"],"team":["self"]}]}}"#);
    assert!(drive(&cfg, &teams, &topic, "todo", Some(teams[0].id)));
    assert!(!drive(&cfg, &teams, &topic, "review", Some(teams[0].id)));
    assert!(!drive(&cfg, &teams, &topic, "todo", None));
}
