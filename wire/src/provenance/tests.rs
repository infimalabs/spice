use super::*;

const DIGEST: &str = "0bd1d1c24587b6b856cbd3172385e466e3f8a419c291e6a067b3cdce02867034";

#[test]
fn every_variant_round_trips_through_its_one_spelling() {
    for text in [
        "ack:dTKHYYzQy",
        "ack:1kQnvnHNx",
        "task:dWB423Bgc",
        &format!("doc:{DIGEST}"),
        "operator:draft",
    ] {
        let provenance: TaskProvenance = text.parse().expect("canonical provenance");
        assert_eq!(provenance.to_string(), text);
        let json = serde_json::to_string(&provenance).expect("encode");
        assert_eq!(
            serde_json::from_str::<TaskProvenance>(&json).expect("decode"),
            provenance
        );
    }
}

#[test]
fn the_strict_decoder_refuses_noncanonical_and_unshaped_spellings() {
    for text in [
        "review:CARDS-1kRYkKfcx",
        "task:CARDS-1kRYkKfcx",
        "gate:complexity",
        "ack:not-a-key",
        "operator:other",
    ] {
        assert!(
            serde_json::from_str::<TaskProvenance>(&format!("{text:?}")).is_err(),
            "{text} decoded"
        );
    }
}

#[test]
fn authored_origins_admit_only_steering_and_parent_tasks() {
    assert_eq!(
        TaskProvenance::authored("ack:dTKHYYzQy"),
        Ok(TaskProvenance::Steering("dTKHYYzQy".to_owned()))
    );
    let parent = TaskProvenance::Task("1kRYkKfcx".parse().expect("stamp"));
    assert_eq!(
        TaskProvenance::authored("task:1kRYkKfcx").as_ref(),
        Ok(&parent)
    );
    assert_eq!(
        TaskProvenance::authored("task:CARDS-1kRYkKfcx").as_ref(),
        Ok(&parent),
        "a handle names its birth stamp"
    );
    for invalid in [
        &format!("doc:{DIGEST}"),
        "operator:draft",
        "gate:complexity",
        "review:CARDS-1kRYkKfcx",
    ] {
        assert!(TaskProvenance::authored(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn every_provenance_spells_its_creation_origin() {
    for provenance in [
        TaskProvenance::Steering("dTKHYYzQy".to_owned()),
        TaskProvenance::Task("dWB423Bgc".parse().expect("stamp")),
        TaskProvenance::Document(DIGEST.parse().expect("digest")),
        TaskProvenance::OperatorDraft,
    ] {
        assert_eq!(provenance.origin().to_string(), provenance.to_string());
    }
}
