//! Independent row and byte probes for every budgeted retained-page API.

use super::*;

#[test]
fn remaining_page_apis_bound_row_and_byte_probes_independently() {
    let journal = authority_fixture();
    for position in [3, 4] {
        extend_thread(
            &journal,
            position,
            FactKind::JournalBacked,
            text_body("x".repeat(600_000)),
        );
    }
    advance_cursor(&journal, 4);
    let origin = "task:1kKRRsDPx".parse().expect("fixture origin");
    let budgets = [
        FactReadBudget::new(1, MAX_RETAINED_FACT_PAGE_BYTES)
            .expect("row-limited repository budget"),
        FactReadBudget::new(2, 700_000).expect("byte-limited repository budget"),
    ];

    for budget in budgets {
        reset_indexed_read_metrics();
        let recent = journal
            .read(|read| read.recent_origin_facts(&origin, FactKind::JournalBacked, None, budget))
            .expect("recent-origin page is bounded");
        assert_eq!(recent.rows.len(), 1);
        assert!(recent.continuation.is_some());
        assert_page_probe_metrics(budget);

        reset_indexed_read_metrics();
        let facts = journal
            .read(|read| read.fact_page(FactPageStart::At(seq(3)), seq(4), budget))
            .expect("primary-key page is bounded");
        assert_eq!(facts.rows.len(), 1);
        assert!(facts.overflow.is_some());
        assert_page_probe_metrics(budget);

        reset_indexed_read_metrics();
        let causal = journal
            .read(|read| read.causal_page(seq(4), budget))
            .expect("causal page is bounded")
            .expect("causal position exists");
        assert_eq!(causal.rows.len(), 1);
        assert!(causal.overflow.is_some());
        assert_page_probe_metrics(budget);
    }
}

fn assert_page_probe_metrics(budget: FactReadBudget) {
    let metrics = indexed_read_metrics();
    assert_eq!((metrics.queries, metrics.rows), (1, 2));
    assert!(metrics.decoded_bytes > 0);
    assert!(metrics.decoded_bytes <= budget.decoded_bytes() as u64);
}

#[test]
fn direct_task_neighbors_follow_both_current_edge_directions_across_pages() {
    let journal = authority_fixture();
    let original: TaskStamp = "1kKRRsDPx".parse().expect("fixture stamp");
    let heads = journal
        .read(|read| read.task_heads_for(&original))
        .expect("fixture reads")
        .expect("fixture task exists");
    let root = TaskStamp::for_micros(10).expect("root stamp");
    let prerequisite = TaskStamp::for_micros(11).expect("prerequisite stamp");
    let dependent = TaskStamp::for_micros(12).expect("dependent stamp");
    let unrelated = TaskStamp::for_micros(13).expect("unrelated stamp");
    for stamp in [&root, &prerequisite, &dependent, &unrelated] {
        let mut statement = heads.statement().body().clone();
        statement.born = stamp.clone();
        statement.dependencies = if stamp == &dependent {
            vec![root.clone()]
        } else {
            Vec::new()
        };
        insert_task_head(
            &journal.connection,
            stamp.as_str(),
            statement.revision,
            &statement,
        );
        journal
            .connection
            .execute(
                "INSERT INTO workflow_heads(stamp, head_seq, body) VALUES (?1, ?2, ?3)",
                params![
                    stamp.as_str(),
                    heads.workflow().position().get() as i64,
                    wire::encode(heads.workflow().body()).expect("workflow encodes")
                ],
            )
            .expect("insert neighbor workflow");
        journal
            .connection
            .execute(
                "INSERT INTO completion_heads(stamp, head_seq, body) VALUES (?1, ?2, ?3)",
                params![
                    stamp.as_str(),
                    heads.completion().position().get() as i64,
                    wire::encode(heads.completion().body()).expect("completion encodes")
                ],
            )
            .expect("insert neighbor completion");
    }
    let budget = FactReadBudget::new(1, MAX_RETAINED_FACT_PAGE_BYTES).expect("one-row budget");
    let first = journal
        .read(|read| {
            read.task_neighbor_head_page(&root, std::slice::from_ref(&prerequisite), None, budget)
        })
        .expect("first neighbor page");
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| &row.statement().body().born)
            .collect::<Vec<_>>(),
        [&prerequisite]
    );
    let second = journal
        .read(|read| {
            read.task_neighbor_head_page(
                &root,
                std::slice::from_ref(&prerequisite),
                first.continuation.as_ref(),
                budget,
            )
        })
        .expect("second neighbor page");
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| &row.statement().body().born)
            .collect::<Vec<_>>(),
        [&dependent]
    );
    assert!(second.continuation.is_none());
    journal.connection.execute(
        "UPDATE task_heads SET body = CAST(json_set(CAST(body AS TEXT), '$.dependencies', json('[]')) AS BLOB) WHERE stamp = ?1",
        params![dependent.as_str()],
    ).expect("unlink dependent");
    let removed = journal
        .read(|read| read.task_neighbor_head_page(&root, &[], None, budget))
        .expect("unlinked neighbor page");
    assert!(removed.rows.is_empty());
}
