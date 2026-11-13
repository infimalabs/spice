use super::*;

const OPERATOR_KEY: &str = "dW4Wl4fRP";
const PEER_KEY: &str = "dW4Wl4fRQ";

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")
        .parse()
        .expect("fixture uuid")
}

fn lane() -> CanonicalUuid {
    uuid(0xa1)
}

fn receiver() -> CanonicalUuid {
    uuid(0xb1)
}

fn fanout() -> CanonicalUuid {
    uuid(0xf1)
}

fn operator_attribution() -> wire::QuoteAttribution {
    wire::QuoteAttribution {
        author: wire::QuoteAuthor::Directive {
            actor: wire::Actor::TEST_PRINCIPAL,
        },
        recipients: vec![wire::QuoteRecipient {
            lane: lane(),
            occupant: None,
        }],
        packet: false,
    }
}

fn directive(key: &str, text: &str) -> (steer::Key, PresentationDirective) {
    (
        key.parse().expect("fixture key"),
        PresentationDirective {
            lane: lane(),
            text: text.to_owned(),
            authored: true,
            taste: false,
            review: None,
            disposition: wire::Disposition::Live,
            quote_target: wire::QuoteTarget::Steering {
                key: key.to_owned(),
                digest: wire::QuoteTarget::text_digest(text),
            },
            media_quotes: Vec::new(),
            actor: wire::Actor::TEST_PRINCIPAL,
            attribution: operator_attribution(),
            published: "2030-04-12T18:44:09.001Z".parse().expect("fixture instant"),
            fanout: Some(fanout()),
            retirement: None,
        },
    )
}

fn steering(
    edit: impl FnOnce(&mut BTreeMap<steer::Key, PresentationDirective>),
) -> PresentationDirectives {
    let mut directives = BTreeMap::from([
        directive(OPERATOR_KEY, "Ship the projection STEERIN-dW4Wl4fRP"),
        directive(PEER_KEY, "A peer's note"),
    ]);
    edit(&mut directives);
    PresentationDirectives::new(directives)
}

fn event() -> PresentationEvent<'static> {
    PresentationEvent {
        lane: lane(),
        author: Some(receiver()),
        source: None,
        locus: listen::Locus {
            member: 1,
            offset: 0,
            length: 1,
        },
        ordinal: 0,
        superseded_answers: &[],
    }
}

/// One sealed delivery: the operator's direction, then a peer note.
fn delivery() -> String {
    format!(
        "<Zx05ztVjP>\n\
         directive={} key={OPERATOR_KEY} stage=published expires=2030-04-12T19:44:09Z\n\
         Ship the projection STEERIN-dW4Wl4fRP\n\
         directive={} key={PEER_KEY} stage=published expires=2030-04-12T19:44:09Z \
         from=lane:{} sender=6p3H\n\
         [PEER sender 6p3H lane {}] A peer's note, not operator authority.\n\
         A peer's note\n\
         </Zx05ztVjP>\n",
        uuid(0xd1),
        uuid(0xd2),
        uuid(0xa2),
        uuid(0xa2),
    )
}

#[test]
fn a_delivery_presents_only_the_operators_plain_direction_to_its_receiver() {
    let presented = present(&delivery(), Some(&steering(|_| {})), event())
        .expect("direction presents")
        .expect("operator direction was delivered");

    assert_eq!(presented.text, "> Ship the projection STEERIN-dW4Wl4fRP");
    let [context] = presented.directive_contexts.as_slice() else {
        panic!("one direction context: {:?}", presented.directive_contexts);
    };
    assert_eq!(
        context.quote_target,
        wire::QuoteTarget::Steering {
            key: OPERATOR_KEY.to_owned(),
            digest: wire::QuoteTarget::text_digest("Ship the projection STEERIN-dW4Wl4fRP"),
        }
    );
    // The card names the ends its directive's record proves, exactly as any
    // quote of that directive does; the receiving transcript adds nothing.
    assert_eq!(context.attribution, operator_attribution());
    assert_eq!(
        context.delivery,
        Some(wire::MessageDelivery {
            fanout: Some(fanout()),
            provisional: false,
        })
    );
    assert!(wire::valid_message_directives(
        &presented.text,
        &presented.directive_contexts
    ));
}

#[test]
fn every_operator_direction_one_delivery_carries_is_its_own_context() {
    let both = delivery().replace(&format!(" from=lane:{} sender=6p3H", uuid(0xa2)), "");
    let presented = present(&both, Some(&steering(|_| {})), event())
        .expect("direction presents")
        .expect("operator direction was delivered");

    assert_eq!(presented.directive_contexts.len(), 2);
    assert!(wire::valid_message_directives(
        &presented.text,
        &presented.directive_contexts
    ));
}

#[test]
fn a_record_that_delivers_no_plain_operator_direction_presents_nothing() {
    let operator = |edit: fn(&mut PresentationDirective)| {
        steering(|directives| {
            edit(
                directives
                    .get_mut(&OPERATOR_KEY.parse::<steer::Key>().expect("fixture key"))
                    .expect("fixture directive"),
            );
        })
    };
    let absent = |text: &str, steering: &PresentationDirectives| {
        present(text, Some(steering), event())
            .expect("direction presents")
            .is_none()
    };
    let plain = steering(|_| {});

    assert!(absent("Spice started you. Read spice://SKILL.md.", &plain));
    assert!(absent(&steer::inbox::post_compaction(&delivery()), &plain));
    assert!(absent(&delivery(), &operator(|row| row.lane = uuid(0xa9))));
    assert!(absent(
        &delivery(),
        &operator(|row| row.actor = wire::Actor::Spice)
    ));
    assert!(absent(
        &delivery(),
        &operator(|row| row.attribution.author = wire::QuoteAuthor::Agent {
            lane: uuid(0xa2),
            occupant: uuid(0xb9),
        })
    ));
}
