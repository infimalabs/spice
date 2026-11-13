use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use wire::body::LaneRenewalChanged;
use wire::intent::{LaneRenewal, LaneRenewalAnswer};
use wire::{CanonicalUuid, LaneRenewalRecord, LaneRenewalState, RenewalIdentity};

const TEAM: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116303";
const PREDECESSOR: &str = "018f82d4-6f59-7d06-92e3-bce913116304";
const SUCCESSOR: &str = "018f82d4-6f59-7d06-92e3-bce913116305";
const OTHER: &str = "018f82d4-6f59-7d06-92e3-bce913116306";

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("fixture UUID is canonical")
}

fn predecessor() -> RenewalIdentity {
    RenewalIdentity {
        occupant: uuid(PREDECESSOR),
        session: "session-predecessor".to_owned(),
        thread: "thread-predecessor".to_owned(),
    }
}

fn successor() -> RenewalIdentity {
    RenewalIdentity {
        occupant: uuid(SUCCESSOR),
        session: "session-successor".to_owned(),
        thread: "thread-successor".to_owned(),
    }
}

fn states() -> Vec<LaneRenewalState> {
    vec![
        LaneRenewalState::Requested {
            predecessor: predecessor(),
            team_slot: 2,
        },
        LaneRenewalState::HandoffPending {
            predecessor: predecessor(),
            team_slot: 2,
        },
        LaneRenewalState::SuccessorStarted {
            predecessor: predecessor(),
            successor: successor(),
            team_slot: 2,
        },
        LaneRenewalState::LaunchRefused {
            predecessor: predecessor(),
            team_slot: 2,
            reason: "vendor launch refused the selected model".to_owned(),
        },
    ]
}

fn record(state: LaneRenewalState) -> LaneRenewalRecord {
    LaneRenewalRecord {
        team: uuid(TEAM),
        lane: uuid(LANE),
        revision: 41,
        state,
    }
}

fn round_trip<T>(value: &T) -> T
where
    T: Serialize + DeserializeOwned,
{
    let bytes = wire::encode(value).expect("fixture encodes");
    wire::decode(&bytes).expect("encoded fixture decodes")
}

#[test]
fn every_renewal_state_round_trips_with_its_original_slot_and_identities() {
    for (state, spelling) in states().into_iter().zip([
        "requested",
        "handoff-pending",
        "successor-started",
        "launch-refused",
    ]) {
        let expected = record(state);
        let observed = round_trip(&expected);
        assert_eq!(observed, expected);

        let encoded: Value = serde_json::from_slice(&wire::encode(&observed).unwrap()).unwrap();
        assert_eq!(encoded["state"]["state"], spelling);
        assert_eq!(encoded["state"]["team_slot"], 2);
        assert_eq!(encoded["state"]["predecessor"]["occupant"], PREDECESSOR);
        assert_eq!(
            encoded["state"]["predecessor"]["session"],
            "session-predecessor"
        );
        assert_eq!(
            encoded["state"]["predecessor"]["thread"],
            "thread-predecessor"
        );
        if encoded["state"]["state"] == "successor-started" {
            assert_eq!(encoded["state"]["successor"]["occupant"], SUCCESSOR);
            assert_eq!(
                encoded["state"]["successor"]["session"],
                "session-successor"
            );
            assert_eq!(encoded["state"]["successor"]["thread"], "thread-successor");
        }
        if encoded["state"]["state"] == "launch-refused" {
            assert_eq!(
                encoded["state"]["reason"],
                "vendor launch refused the selected model"
            );
        }
        assert!(encoded.get("capabilities").is_none());
        assert!(encoded["state"].get("capabilities").is_none());
    }
}

#[test]
fn the_revision_bound_command_is_strict_and_carries_the_exact_member() {
    let command = json!({
        "topology_revision": 40,
        "team": TEAM,
        "lane": LANE,
        "requested": true
    });
    let decoded: LaneRenewal = wire::decode(&serde_json::to_vec(&command).unwrap()).unwrap();
    assert_eq!(decoded.topology_revision, 40);
    assert_eq!(decoded.team, uuid(TEAM));
    assert_eq!(decoded.lane, uuid(LANE));
    assert!(decoded.requested);

    for malformed in [
        json!({"topology_revision": 40, "team": TEAM, "lane": LANE}),
        json!({"topology_revision": 40, "team": TEAM, "lane": LANE, "requested": "true"}),
        json!({"topology_revision": 40, "team": TEAM, "lane": LANE, "requested": true, "capability": OTHER}),
    ] {
        assert!(wire::decode::<LaneRenewal>(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }
}

#[test]
fn clear_state_is_omitted_and_null_is_refused() {
    let answer = LaneRenewalAnswer {
        team: uuid(TEAM),
        lane: uuid(LANE),
        topology_revision: 41,
        renewal: None,
    };
    let encoded: Value = serde_json::from_slice(&wire::encode(&answer).unwrap()).unwrap();
    assert!(encoded.get("renewal").is_none());
    assert_eq!(round_trip(&answer), answer);

    let changed = LaneRenewalChanged {
        team: uuid(TEAM),
        lane: uuid(LANE),
        renewal: None,
    };
    let encoded: Value = serde_json::from_slice(&wire::encode(&changed).unwrap()).unwrap();
    assert!(encoded.get("renewal").is_none());
    assert_eq!(round_trip(&changed), changed);

    let answer_null = json!({"team": TEAM, "lane": LANE, "topology_revision": 41, "renewal": null});
    assert!(wire::decode::<LaneRenewalAnswer>(&serde_json::to_vec(&answer_null).unwrap()).is_err());
    let changed_null = json!({"team": TEAM, "lane": LANE, "renewal": null});
    assert!(
        wire::decode::<LaneRenewalChanged>(&serde_json::to_vec(&changed_null).unwrap()).is_err()
    );
}

#[test]
fn nested_unknown_fields_and_mismatched_addresses_are_refused() {
    let renewal = serde_json::to_value(record(states().remove(0))).unwrap();

    let mut unknown_state = renewal.clone();
    unknown_state["state"]["capabilities"] = json!([]);
    assert!(
        wire::decode::<LaneRenewalRecord>(&serde_json::to_vec(&unknown_state).unwrap()).is_err()
    );

    let mut unknown_identity = renewal.clone();
    unknown_identity["state"]["predecessor"]["capability"] = json!(OTHER);
    assert!(
        wire::decode::<LaneRenewalRecord>(&serde_json::to_vec(&unknown_identity).unwrap()).is_err()
    );

    let mismatched_transition = json!({
        "team": OTHER,
        "lane": LANE,
        "renewal": renewal
    });
    assert!(
        wire::decode::<LaneRenewalChanged>(&serde_json::to_vec(&mismatched_transition).unwrap())
            .is_err()
    );

    let mismatched_reply = json!({
        "team": TEAM,
        "lane": LANE,
        "topology_revision": 42,
        "renewal": record(states().remove(0))
    });
    assert!(
        wire::decode::<LaneRenewalAnswer>(&serde_json::to_vec(&mismatched_reply).unwrap()).is_err()
    );
}
