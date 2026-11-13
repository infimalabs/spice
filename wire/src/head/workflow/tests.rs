use super::*;

fn at() -> Instant {
    "2026-09-23T06:00:00.000Z"
        .parse()
        .expect("fixture settlement instant")
}

#[test]
fn settlement_names_each_conclusion_and_only_a_collapse_reason_parts_closed_from_collapsed() {
    let reason = Some("the premise failed");
    for (status, collapse_reason, settlement) in [
        (WorkflowStatus::completed(at()), None, Settlement::Done),
        (WorkflowStatus::reviewed(at()), reason, Settlement::Done),
        (WorkflowStatus::closed(at()), None, Settlement::Closed),
        (WorkflowStatus::closed(at()), reason, Settlement::Collapsed),
        (WorkflowStatus::deleted(at()), None, Settlement::Deleted),
        (WorkflowStatus::deleted(at()), reason, Settlement::Deleted),
    ] {
        assert_eq!(
            status.settlement(collapse_reason),
            Some(settlement),
            "{status:?} with {collapse_reason:?}"
        );
    }
    assert_eq!(WorkflowStatus::pending().settlement(reason), None);
    assert_eq!(
        [
            Settlement::Done,
            Settlement::Closed,
            Settlement::Collapsed,
            Settlement::Deleted,
        ]
        .map(Settlement::as_str),
        ["done", "closed", "collapsed", "deleted"]
    );
}
