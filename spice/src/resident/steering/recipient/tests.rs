use super::*;

fn candidate(last: u8, cohort: Cohort, score: Option<f64>) -> Candidate {
    Candidate {
        lane: format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture UUID"),
        occupant: None,
        cohort,
        score,
    }
}

#[test]
fn cohort_outranks_score_and_score_orders_each_cohort() {
    let mut candidates = [
        candidate(1, Cohort::Tasked, Some(9.0)),
        candidate(2, Cohort::Vacant, Some(5.0)),
        candidate(3, Cohort::Idle, None),
        candidate(4, Cohort::Vacant, Some(7.0)),
        candidate(5, Cohort::Idle, Some(0.5)),
        candidate(6, Cohort::Tasked, None),
    ];
    candidates.sort_by(Candidate::order);
    let ranked = candidates
        .iter()
        .map(|candidate| candidate.lane.to_string().split_off(34))
        .collect::<Vec<_>>();
    assert_eq!(ranked, ["05", "03", "04", "02", "01", "06"]);
}
