use wire::body::TranscriptLocus;

#[test]
fn sourced_locus_round_trips_its_tape_generation() {
    let locus = TranscriptLocus {
        source: Some(
            "018f82d4-6f59-7d06-92e3-bce913116399"
                .parse()
                .expect("canonical source"),
        ),
        member: 1,
        offset: 2,
        length: 3,
        event: 4,
        line: 5,
        column: 6,
    };
    let bytes = wire::encode(&locus).expect("sourced locus encodes");
    assert_eq!(
        wire::decode::<TranscriptLocus>(&bytes).expect("sourced locus decodes"),
        locus
    );
}
