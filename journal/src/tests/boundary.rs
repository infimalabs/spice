//! Scoped lifecycle boundaries fail on selected corrupt authority rather than
//! silently encoding an incomplete or empty answer.

use super::*;

use wire::body::SourceBoundary;

#[test]
fn selected_unreadable_heads_fail_their_boundaries_instead_of_disappearing() {
    {
        let journal = authority_fixture();
        let lane = transcript_source_head().lane;

        let proven = journal
            .read(|read| read.source_boundary(&lane))
            .expect("an intact journal proves the lane's boundary");
        assert_eq!(
            proven,
            SourceBoundary::Proven {
                heads: vec![transcript_source_head()],
            },
        );

        let mut renamed = transcript_source_head();
        renamed.stream = "sidecar".to_owned();
        journal
            .connection
            .execute(
                "UPDATE transcript_source_heads SET body = ?1",
                params![wire::encode(&renamed).expect("encode renamed transcript head")],
            )
            .expect("corrupt transcript head fixture");

        journal
            .read(|read| read.source_boundary(&lane))
            .expect_err("an unreadable head cannot prove a boundary");
    }

    let journal = authority_fixture();
    let lane = claim_head().lane;
    let measured = journal
        .read(|read| read.lane_source_boundaries(&[lane]))
        .expect("an intact journal measures the claimed lane");
    assert_eq!(measured.first().map(|boundary| boundary.lane), Some(lane));

    journal
        .connection
        .execute(
            "UPDATE claim_heads SET body = ?1",
            params![b"not a claim head".to_vec()],
        )
        .expect("corrupt claim head fixture");

    journal
        .read(|read| read.lane_source_boundaries(&[lane]))
        .expect_err("an unreadable claim head cannot decide which lanes are claimed");
}
