use super::*;

fn authority(text: &str) -> RoomAuthorityStamp {
    text.parse().expect("authority")
}

fn task(text: &str) -> TaskStamp {
    text.parse().expect("task")
}

fn lane(text: &str) -> CanonicalUuid {
    text.parse().expect("lane")
}
fn principal(text: &str) -> CanonicalUuid {
    text.parse().expect("principal")
}

fn message(routes: Vec<RoomRoute>, bytes: u32) -> RoomPublication {
    serde_json::from_value(serde_json::json!({
        "publication": "1kKRRsDR1",
        "author": {"kind": "lane", "lane": "018f82d4-6f59-7d06-92e3-bce913116301"},
        "source": {"kind": "journal", "fact": "1"},
        "content_bytes": bytes,
        "kind": {"verb": "message"},
        "routes": routes,
    }))
    .expect("message")
}

#[test]
fn selectors_keep_independent_qualifiers_and_reject_duplicate_or_unknown_routes() {
    let first = RoomRoute {
        destination: RoomDestination::Lane {
            lane: lane("018f82d4-6f59-7d06-92e3-bce913116302"),
        },
        context: Some(task("1kKRRsDR2")),
        direction: None,
    };
    let second = RoomRoute {
        destination: RoomDestination::Principal {
            principal: principal("018f82d4-6f59-7d06-92e3-bce913116303"),
        },
        context: None,
        direction: None,
    };
    let publication = message(vec![first.clone(), second.clone()], 40);
    assert_eq!(publication.routes, [first.clone(), second]);
    assert_eq!(
        crate::decode::<RoomPublication>(&crate::encode(&publication).unwrap()).unwrap(),
        publication
    );
    assert_eq!(
        publication.lane_routes().collect::<Vec<_>>(),
        [(
            lane("018f82d4-6f59-7d06-92e3-bce913116302"),
            Some(&task("1kKRRsDR2"))
        )]
    );
    let input = publication
        .principal_input(principal("018f82d4-6f59-7d06-92e3-bce913116303"))
        .expect("the principal route names its input");
    assert_eq!(
        input,
        PrincipalInputRef::published(
            &authority("1kKRRsDR1"),
            principal("018f82d4-6f59-7d06-92e3-bce913116303"),
        )
    );
    assert_ne!(
        input.directive,
        PrincipalInputRef::published(
            &authority("1kKRRsDR1"),
            principal("018f82d4-6f59-7d06-92e3-bce913116304"),
        )
        .directive,
        "sibling principal routes of one publication own distinct inputs"
    );
    assert_eq!(
        publication.principal_input(principal("018f82d4-6f59-7d06-92e3-bce913116304")),
        None
    );

    for routes in [
        serde_json::json!([first.clone(), first]),
        serde_json::json!([{"destination": {"kind": "group", "group": "1kKRRsDR2"}}]),
        serde_json::json!([
            {"destination": {"kind": "principal", "principal": "018f82d4-6f59-7d06-92e3-bce913116303"}},
            {"destination": {"kind": "principal", "principal": "018f82d4-6f59-7d06-92e3-bce913116303"}, "context": "1kKRRsDR2"},
        ]),
        serde_json::json!([{"destination": {"kind": "principal", "principal": "018f82d4-6f59-7d06-92e3-bce913116303"}, "direction": "dVwg6Zqlf"}]),
    ] {
        let refused = serde_json::json!({
            "publication": "1kKRRsDR1",
            "author": "principal:018f82d4-6f59-7d06-92e3-bce913116303",
            "source": {"kind": "journal", "fact": "1"},
            "content_bytes": 1,
            "kind": {"verb": "message"},
            "routes": routes,
        });
        assert!(serde_json::from_value::<RoomPublication>(refused).is_err());
    }
    let about = |destination: serde_json::Value| {
        serde_json::from_value::<RoomPublication>(serde_json::json!({
            "publication": "1kKRRsDR1",
            "author": {"kind": "lane", "lane": "018f82d4-6f59-7d06-92e3-bce913116301"},
            "source": {"kind": "journal", "fact": "1"},
            "content_bytes": 1,
            "kind": {"verb": "message"},
            "routes": [{"destination": destination, "direction": "dVwg6Zqlf"}],
        }))
    };
    assert!(about(serde_json::json!({"kind": "principal", "principal": "018f82d4-6f59-7d06-92e3-bce913116303"})).is_ok());
    assert!(
        about(serde_json::json!({"kind": "lane", "lane": "018f82d4-6f59-7d06-92e3-bce913116302"}))
            .is_err()
    );
}
