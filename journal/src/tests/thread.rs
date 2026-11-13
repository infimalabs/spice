//! Bounded cause-thread pages through the thread index.

use super::*;

#[test]
fn thread_pages_resume_at_the_first_withheld_member_and_decode_only_the_page() {
    let journal = long_thread_fixture();
    let resolve = |reference: &str| {
        journal
            .read(|read| Ok(read.thread_root(reference)))
            .expect("thread index reads")
    };
    assert_eq!(
        resolve(&seq(150).to_string()).expect("descendant resolves"),
        seq(1)
    );
    assert_eq!(resolve("task:1kKRRsDPx").expect("origin resolves"), seq(1));
    assert!(matches!(
        resolve(&seq(201).to_string()),
        Err(crate::ThreadError::NotFound { .. })
    ));
    let page = |at, rows| {
        journal
            .read(|read| read.thread_page(seq(1), at, fact_budget(rows)))
            .expect("thread page reads")
    };
    assert_eq!(
        page(Some(seq(201)), 1),
        None,
        "a cursor outside the thread cannot resume it"
    );

    reset_indexed_read_metrics();
    let first = page(Some(seq(198)), 1).expect("member cursor resumes");
    assert_eq!(
        first.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(198)],
        "the cursor is the first withheld fact, so it is returned"
    );
    assert_eq!(
        first.overflow,
        Some(wire::Overflow {
            omitted: 2,
            continuation: seq(199),
        })
    );
    let metrics = indexed_read_metrics();
    assert_eq!(
        metrics.rows, 2,
        "the page decodes its row plus one overflow probe, not the thread"
    );
    assert_eq!(
        metrics.queries, 3,
        "membership, population, and bounded page are the only queries"
    );
    assert_eq!(
        metrics.decoded_bytes,
        u64::try_from(wire::encoded_len(&first.rows[0]).expect("returned envelope measures"))
            .expect("fixture envelope length fits"),
        "only the returned row is decoded; the overflow row is measured in SQLite"
    );

    let last = page(first.overflow.map(|overflow| overflow.continuation), 4)
        .expect("continuation resumes");
    assert_eq!(
        last.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(199), seq(200)]
    );
    assert_eq!(last.overflow, None);
    assert_eq!(
        page(None, 2)
            .expect("thread opens")
            .overflow
            .map(|overflow| overflow.omitted),
        Some(198)
    );
}

#[test]
fn an_origin_that_names_multiple_roots_is_ambiguous_in_root_order() {
    let journal = authority_fixture();
    journal
        .connection
        .execute(
            "UPDATE thread_index SET root = ?1 WHERE seq = ?1",
            [sql_seq(2)],
        )
        .expect("fixture creates a second root for the same origin");

    let resolved = journal
        .read(|read| Ok(read.thread_root("task:1kKRRsDPx")))
        .expect("thread index reads");
    assert!(matches!(
        resolved,
        Err(crate::ThreadError::Ambiguous { roots, .. })
            if roots == [seq(1), seq(2)]
    ));
}
