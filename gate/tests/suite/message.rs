use gate::{CommitMessagePolicy, OriginTrailer, TrailerRejection, validate_commit_message};

const VALID: &str =
    "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx";

#[test]
fn v1_round_trips_byte_identically() {
    let parsed = VALID.parse::<OriginTrailer>().expect("valid v1 trailer");

    assert_eq!(parsed.to_string(), VALID);
    assert_eq!(
        parsed.repo().to_string(),
        "018f82d4-6f59-7d06-92e3-bce9131163c7"
    );
    assert_eq!(parsed.task().as_str(), "SPICE-1kKRRsDPx");
}

#[test]
fn reordered_extra_missing_and_other_spellings_are_refused() {
    let malformed = [
        "Spice-Origin: repo=018f82d4-6f59-7d06-92e3-bce9131163c7 v1 task=SPICE-1kKRRsDPx",
        "Spice-Origin: v1 task=SPICE-1kKRRsDPx repo=018f82d4-6f59-7d06-92e3-bce9131163c7",
        "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx actor=agent",
        "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7",
        "Spice-Origin: v1 task=SPICE-1kKRRsDPx",
        "Spice-Origin: v2 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx",
        "spice-origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx",
        "Spice-Origin: v1 repo=018F82D4-6F59-7D06-92E3-BCE9131163C7 task=SPICE-1kKRRsDPx",
        "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=spice-aBcD",
        "Spice-Origin:  v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx",
        "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7  task=SPICE-1kKRRsDPx",
        "Spice-Origin: v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx ",
    ];

    for line in malformed {
        let error = line
            .parse::<OriginTrailer>()
            .expect_err("non-v1 spelling must be refused");
        assert_eq!(error.evidence(), line);
    }
}

#[test]
fn commit_gate_names_the_exact_commit_and_preserves_unresolved_evidence() {
    let malformed = "Spice-Origin: repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx";
    let message = format!("Record provenance\n\n{malformed}\n");

    let error = validate_commit_message("8ac31e7", &message)
        .expect_err("malformed origin evidence must remain unresolved");

    assert_eq!(error.commit(), "8ac31e7");
    assert_eq!(error.line(), 3);
    assert_eq!(error.evidence(), malformed);
    assert_eq!(message.lines().nth(2), Some(malformed));
    assert!(error.to_string().starts_with("commit 8ac31e7 "));
}

#[test]
fn commit_gate_validates_every_origin_trailer_and_ignores_unrelated_lines() {
    let message = format!(
        "Record provenance\n\nBody: prose\n\n{VALID}\nReviewed-By: Agent <agent@example.test>\nSpice-Originator: retained\nSpice-Origin-v0: retired\n{VALID}\n"
    );

    let trailers = validate_commit_message("f29d16c", &message).expect("valid commit message");

    assert_eq!(trailers.len(), 2);
    assert!(trailers.iter().all(|trailer| trailer.to_string() == VALID));
}

#[test]
fn commit_gate_refuses_spacing_that_only_normalization_would_accept() {
    let malformed =
        " Spice-Origin : v1 repo=018f82d4-6f59-7d06-92e3-bce9131163c7 task=SPICE-1kKRRsDPx";

    let error = validate_commit_message("d431f90", malformed)
        .expect_err("the gate must not trim a malformed trailer");

    assert_eq!(error.commit(), "d431f90");
    assert_eq!(error.evidence(), malformed);
}

#[test]
fn trailer_policy_is_case_insensitive_and_blocking_precedes_allowing() {
    let policy = toml::from_str::<CommitMessagePolicy>(
        r#"
allowed_trailers = ["Spice-Origin", "Reviewed-By", "reviewed-by"]
blocked_trailers = ["Co-Authored-By", "Reviewed-By"]
"#,
    )
    .expect("trailer policy admits");

    assert_eq!(
        policy.allowed_trailers().expect("allow set"),
        ["spice-origin", "reviewed-by"]
    );
    assert_eq!(
        policy.trailer_rejection("CO-AUTHORED-BY"),
        Some(TrailerRejection::Blocked)
    );
    assert_eq!(
        policy.trailer_rejection("Reviewed-By"),
        Some(TrailerRejection::Blocked)
    );
    assert_eq!(
        policy.trailer_rejection("Signed-Off-By"),
        Some(TrailerRejection::Disallowed)
    );
    assert_eq!(policy.trailer_rejection("spice-origin"), None);
}

#[test]
fn trailer_policy_distinguishes_absent_from_allow_nothing_and_refuses_bad_keys() {
    assert_eq!(
        CommitMessagePolicy::default().trailer_rejection("Anything-Goes"),
        None
    );
    let none = toml::from_str::<CommitMessagePolicy>("allowed_trailers = []\n")
        .expect("empty allow set admits");
    assert_eq!(
        none.trailer_rejection("Anything-Goes"),
        Some(TrailerRejection::Disallowed)
    );

    for source in [
        "allowed_trailers = [\"\"]\n",
        "allowed_trailers = [\"Signed Off By\"]\n",
        "blocked_trailers = [\"Acked_By\"]\n",
    ] {
        toml::from_str::<CommitMessagePolicy>(source)
            .expect_err("malformed trailer key must refuse");
    }
}
