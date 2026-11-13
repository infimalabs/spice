use super::*;

fn unplaced(phase: &Phase) -> Task<'_> {
    Task {
        topic: None,
        phase,
        creator: None,
    }
}

#[test]
fn topic_free_drain_rules_own_unplaced_work_over_topic_scoped_teams() {
    let cfg = config(
        r#"{"scoped":{"drain":[{"topic":["/spice/**"]}]},"phased":{"drain":[{"phase":["todo"]}]},"open":{"drain":true}}"#,
    );
    let phase: Phase = "todo".parse().unwrap();
    let teams = [team(1, Some("scoped")), team(2, Some("phased"))];
    let resolver = Resolver::new(&cfg, &teams);
    assert_eq!(resolver.owners(unplaced(&phase)).teams, vec![teams[1].id]);
    assert!(!resolver.drain(teams[0].id, unplaced(&phase)));
    assert!(resolver.drain(teams[1].id, unplaced(&phase)));

    let teams = [team(1, Some("scoped")), team(3, Some("open"))];
    let resolver = Resolver::new(&cfg, &teams);
    assert_eq!(
        resolver.owners(unplaced(&phase)).teams,
        vec![teams[1].id],
        "drain = true names no topic"
    );
}

#[test]
fn without_a_topic_free_drain_rule_every_live_draining_team_owns_unplaced_work() {
    let cfg = config(
        r#"{"scoped":{"drain":[{"topic":["/spice/**"]}]},"other":{"drain":[{"topic":["/engine/**"],"exclusive":true}]}}"#,
    );
    let phase: Phase = "todo".parse().unwrap();
    let mut teams = [
        team(1, Some("scoped")),
        team(2, Some("other")),
        team(3, Some("scoped")),
        team(4, Some("other")),
    ];
    teams[2].drain = false;
    teams[3].lanes.clear();
    let resolver = Resolver::new(&cfg, &teams);
    assert_eq!(
        resolver.owners(unplaced(&phase)),
        Ownership {
            teams: vec![teams[0].id, teams[1].id],
            reserved: false,
        }
    );
}

#[test]
fn any_live_team_with_drive_on_may_create_unplaced_work() {
    let cfg = config(r#"{"scoped":{"drive":[{"topic":["/spice/**"]}],"drain":[]}}"#);
    let phase: Phase = "todo".parse().unwrap();
    let mut teams = [team(1, Some("scoped")), team(2, Some("scoped"))];
    teams[1].drive = false;
    let resolver = Resolver::new(&cfg, &teams);
    assert!(resolver.drive(teams[0].id, unplaced(&phase)));
    assert!(resolver.drive_policy(teams[1].id, unplaced(&phase)));
    assert!(!resolver.drive(teams[1].id, unplaced(&phase)));
    assert!(!resolver.drive(team(3, Some("scoped")).id, unplaced(&phase)));
}
