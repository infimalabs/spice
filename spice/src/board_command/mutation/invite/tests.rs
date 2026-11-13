use super::*;

fn rank(running: bool, matched: Option<f64>, last_claim: Option<u64>, lane: u64) -> Rank {
    Rank {
        running,
        matched,
        last_claim: last_claim.map(|claim| Seq::new(claim).expect("fixture position")),
        lane: format!("018f82d4-6f59-7d06-92e3-{lane:012x}")
            .parse()
            .expect("fixture lane"),
    }
}

#[test]
fn invitation_order_puts_vacancy_before_match_before_recency_before_lane() {
    let expected = [
        rank(false, Some(0.9), None, 8),
        rank(false, Some(0.5), Some(1), 2),
        // Equal everything else, the lower lane id decides.
        rank(false, Some(0.5), None, 1),
        rank(false, Some(0.5), None, 3),
        // An unscored lane follows every scored one; among them the newest claim leads.
        rank(false, None, Some(7), 4),
        rank(false, None, Some(3), 6),
        // A lane that never claimed is the coldest.
        rank(false, None, None, 0),
        // Every running lane follows every vacant one, whatever its match.
        rank(true, Some(1.0), Some(9), 7),
        rank(true, None, None, 9),
    ];
    let mut ranked = expected.iter().rev().collect::<Vec<_>>();
    ranked.sort_by(|left, right| left.order(right));
    assert_eq!(
        ranked.iter().map(|rank| rank.lane).collect::<Vec<_>>(),
        expected.iter().map(|rank| rank.lane).collect::<Vec<_>>(),
    );
}

#[test]
fn vacant_retry_stops_at_faults_outside_the_candidate_worktree() {
    assert!(retry_vacant_failure(&refusal(
        "task::candidate-moved",
        None,
        "try another vacant lane",
    )));
    assert!(retry_vacant_failure(&environment_fault(
        "git::worktree-unresolved",
        "this candidate has no usable worktree",
    )));
    assert!(!retry_vacant_failure(&environment_fault(
        "clock::unavailable",
        "the resident clock failed",
    )));
    assert!(!retry_vacant_failure(&store_fault(
        "board::commit-failed",
        "the shared journal refused the write",
    )));
}
