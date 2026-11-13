use super::*;

fn actor() -> ProcessActor {
    ProcessActor {
        vendor: "omp".to_owned(),
        identity: "current-thread".to_owned(),
    }
}

fn authority(lane: &str, claim: u64) -> AuthorityKey {
    AuthorityKey {
        handle: "CLAIM-00000001".to_owned(),
        claim,
        lane: lane.to_owned(),
        occupant: format!("occupant-{claim}"),
        epoch: format!("epoch-{claim}"),
    }
}

fn owner() -> StoredOwner {
    let actor = actor();
    StoredOwner {
        vendor: actor.vendor,
        identity: actor.identity,
        authority: authority("lane-a", 1),
    }
}

#[test]
fn unseated_resident_defers_loss_while_the_prior_claim_remains_active() {
    let observation = BoardObservation {
        state: WorkingState::default(),
        current: None,
        prior_active: Some(authority("lane-a", 2)),
        claim_reconciliation_pending: true,
    };

    assert_eq!(claim_loss(Some(owner()), &observation, &actor()), None);
}

#[test]
fn settled_cross_lane_movement_remains_claim_loss() {
    let observed = authority("lane-b", 2);
    let observation = BoardObservation {
        state: WorkingState::default(),
        current: None,
        prior_active: Some(observed.clone()),
        claim_reconciliation_pending: false,
    };

    assert_eq!(
        claim_loss(Some(owner()), &observation, &actor()),
        Some(StoredLoss {
            owner: owner(),
            observed,
        })
    );
}
