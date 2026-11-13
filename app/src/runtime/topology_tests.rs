use super::*;

#[test]
fn a_stale_topology_refusal_requires_an_authoritative_refresh() {
    let stale = Outcome::<()>::Refusal(wire::Refusal::bare(
        wire::RefusalDiagnostic::new(
            "lane::topology-moved",
            Some("topology_revision".to_owned()),
            Some("7".to_owned()),
            Some("8".to_owned()),
        )
        .with_detail("read the lane table"),
    ));
    assert!(topology_mutation_requires_refresh(&stale));

    let current = Outcome::Reply(wire::Reply {
        position: wire::ReplyPosition::Direct {},
        body: (),
    });
    assert!(!topology_mutation_requires_refresh(&current));
}
