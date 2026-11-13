use super::*;

#[test]
fn reporting_capability_requires_membership_not_drive_and_never_grants_ownership() {
    let cfg = config(
        r#"{"default":{"drive":[{"topic":["/ordinary"],"phase":["todo"],"team":["self"]}],"drain":[]},"closed":{"drive":[],"drain":[]}}"#,
    );
    let mut teams = [team(1, None), team(2, Some("closed"))];
    for enabled in [false, true] {
        for current in &mut teams {
            current.drive = enabled;
            current.drain = enabled;
        }
        let resolver = Resolver::new(&cfg, &teams);
        for current in &teams {
            for path in ["/oops", "/oops/design", "/oops/report/details"] {
                let topic = path.parse().unwrap();
                let phase = "review".parse().unwrap();
                let task = Task {
                    topic: Some(&topic),
                    phase: &phase,
                    creator: None,
                };
                assert!(resolver.drive(current.id, task));
                assert!(!resolver.drain(current.id, task));
                assert_eq!(resolver.owners(task), Ownership::default());
                assert!(!resolver.drive(team(3, None).id, task));
            }
            for path in [
                "/oops_extra",
                "/oopsx",
                "/elsewhere",
                "018f82d4-6f59-7d06-92e3-bce913116399",
            ] {
                assert!(!drive(
                    &cfg,
                    std::slice::from_ref(current),
                    path,
                    "review",
                    None
                ));
            }
        }
    }
    teams[0].lanes.clear();
    assert!(!drive(
        &cfg,
        &teams,
        "/oops/report",
        "todo",
        Some(teams[0].id)
    ));
}
