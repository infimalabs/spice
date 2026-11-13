//! Agents attribute a principal by its immutable birth code; recorded profile
//! decoration never changes that attribution.

use std::collections::BTreeSet;

use wire::principal::{
    PrincipalCode, PrincipalCodeError, PrincipalDirectory, PrincipalHead, PrincipalProfile,
    PrincipalRole, PrincipalRoles,
};
use wire::{Actor, CanonicalUuid, Digest, Domain, Seq};

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
        .parse()
        .expect("canonical fixture UUID")
}

fn head(principal: u8, born: u64, name: Option<&str>) -> PrincipalHead {
    PrincipalHead {
        principal: uuid(principal),
        born: Seq::new(born).expect("fixture position"),
        profile: PrincipalProfile {
            name: name.map(str::to_owned),
            ..PrincipalProfile::default()
        },
        roles: BTreeSet::from([PrincipalRole::Operator]),
        capability: None,
    }
}

fn code(born: u64) -> PrincipalCode {
    PrincipalCode::of(&head(0, born, None))
}

fn named(directory: &PrincipalDirectory, actor: &Actor) -> String {
    directory.named(actor).to_string()
}

#[test]
fn each_actor_is_named_by_its_exact_principal() {
    let roles = PrincipalRoles::new([
        head(92, 18_731, Some("Second Person")),
        head(91, 18_730, Some("C Anthony Risinger")),
    ]);
    let directory = PrincipalDirectory::codes(&roles);

    assert_eq!(
        named(&directory, &Actor::Principal(uuid(91))),
        code(18_730).to_string()
    );
    assert_eq!(
        named(&directory, &Actor::Principal(uuid(92))),
        code(18_731).to_string()
    );
    for nobody in [
        Actor::TEST_PRINCIPAL,
        Actor::Principal(uuid(93)),
        Actor::Lane(uuid(91)),
        Actor::Spice,
    ] {
        assert_eq!(
            directory.identity(&nobody),
            None,
            "{nobody} named a principal"
        );
        assert_eq!(named(&directory, &nobody), nobody.to_string());
    }
    for actor in [
        Actor::Principal(uuid(91)),
        Actor::Principal(uuid(92)),
        Actor::Principal(uuid(93)),
    ] {
        assert_eq!(
            roles.head(&actor).map(PrincipalCode::of),
            directory.principal(&actor).map(|display| display.code),
            "display and authority resolved {actor} to different principals"
        );
    }

    let admitted = PrincipalRoles::new([
        head(92, 18_731, Some("Second Person")),
        head(91, 18_730, Some("C Anthony Risinger")),
        head(94, 20_000, Some("later")),
    ]);
    let directory = PrincipalDirectory::codes(&admitted);
    assert_eq!(
        named(&directory, &Actor::Principal(uuid(91))),
        code(18_730).to_string()
    );
    assert_eq!(
        named(&directory, &Actor::Principal(uuid(94))),
        code(20_000).to_string()
    );
}

#[test]
fn a_principal_reads_as_its_code_whatever_its_profile_records() {
    for name in [None, Some("  "), Some("C Anthony Risinger")] {
        let directory = PrincipalDirectory::codes(&PrincipalRoles::new([head(91, 18_730, name)]));
        let actor = Actor::Principal(uuid(91));
        assert!(directory.identity(&actor).is_some());
        assert_eq!(named(&directory, &actor), "6wB");
        let decorated = PrincipalDirectory::display(&[head(91, 18_730, name)]);
        assert_eq!(named(&decorated, &actor), "6wB", "{name:?}");
    }
}

#[test]
fn an_unadmitted_principal_keeps_its_actor_spelling() {
    let directory = PrincipalDirectory::codes(&PrincipalRoles::default());
    let actor = Actor::Principal(uuid(91));
    assert_eq!(directory.identity(&actor), None);
    assert_eq!(named(&directory, &actor), actor.to_string());
}

#[test]
fn a_code_names_one_birth_whatever_its_head_later_records() {
    let born = head(91, 18_730, Some("Owner"));
    let mut moved = born.clone();
    moved.profile = PrincipalProfile {
        name: Some("Renamed".to_owned()),
        email: Some("owner@example.invalid".to_owned()),
    };
    moved.roles.clear();
    moved.capability = Some(Digest::compute(Domain::PrincipalCapability, b"issued"));
    assert_eq!(PrincipalCode::of(&moved), PrincipalCode::of(&born));
}

#[test]
fn only_a_canonical_nonzero_spelling_is_a_code() {
    for born in [1, 51, 52, 18_730, u64::MAX] {
        let code = code(born);
        assert_eq!(code.to_string().parse(), Ok(code));
        let encoded = serde_json::to_string(&code).expect("code encodes");
        assert_eq!(encoded, format!("\"{code}\""));
        assert_eq!(
            serde_json::from_str::<PrincipalCode>(&encoded).ok(),
            Some(code)
        );
    }
    assert_eq!("".parse::<PrincipalCode>(), Err(PrincipalCodeError::Empty));
    assert_eq!("0".parse::<PrincipalCode>(), Err(PrincipalCodeError::Zero));
    assert_eq!(
        "06wB".parse::<PrincipalCode>(),
        Err(PrincipalCodeError::LeadingZero("06wB".to_owned()))
    );
    for misspelled in ["6wa", "6 wB", "-1"] {
        assert_eq!(
            misspelled.parse::<PrincipalCode>(),
            Err(PrincipalCodeError::Alphabet(misspelled.to_owned()))
        );
    }
    let widest = "z".repeat(12);
    assert_eq!(
        widest.parse::<PrincipalCode>(),
        Err(PrincipalCodeError::Overflow(widest.clone()))
    );
    assert!(serde_json::from_str::<PrincipalCode>("\"0\"").is_err());
}
