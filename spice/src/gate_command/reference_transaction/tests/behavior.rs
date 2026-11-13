use super::*;

#[test]
fn parser_ignores_blank_lines_and_preserves_each_update() {
    let updates = parse_updates("old new refs/heads/main\n\nzero tip refs/tags/v1\n").unwrap();
    assert_eq!(
        updates,
        [
            RefUpdate {
                old: "old",
                new: "new",
                reference: "refs/heads/main",
            },
            RefUpdate {
                old: "zero",
                new: "tip",
                reference: "refs/tags/v1",
            },
        ]
    );
}

#[test]
fn the_declared_old_value_is_retained_only_for_transaction_correlation() {
    let real = format!("{:040} new refs/heads/main\n", 1);
    let zero = format!("{:040} new refs/heads/main\n", 0);
    let real = parse_updates(&real).unwrap();
    let zero = parse_updates(&zero).unwrap();
    assert_ne!(real, zero);
    assert_ne!(
        transaction_id(TransactionState::Prepared, &real).unwrap(),
        transaction_id(TransactionState::Prepared, &zero).unwrap()
    );
    assert_eq!(real[0].new, zero[0].new);
    assert_eq!(real[0].reference, zero[0].reference);
}

#[test]
fn parser_names_the_first_malformed_line() {
    let error = parse_updates("old new refs/heads/main\nmissing field\n").unwrap_err();
    assert!(matches!(
        error,
        ReferenceTransactionError::MalformedInput { line: 2 }
    ));
}

#[test]
fn only_local_branch_updates_require_repository_observation() {
    let cleanup = parse_updates(
        "0000000000000000000000000000000000000000 \
         0000000000000000000000000000000000000000 AUTO_MERGE\n",
    )
    .unwrap();
    assert!(!requires_repository(&cleanup));

    let branch = parse_updates(
        "0000000000000000000000000000000000000000 \
         aaaabbbb refs/heads/main\n",
    )
    .unwrap();
    assert!(requires_repository(&branch));
}
