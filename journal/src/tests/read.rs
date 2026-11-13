//! Scoped reads over an admitted authority.
//!
//! These observe one immutable snapshot through the read capability rather than
//! the connection, so what they prove is what a caller outside this crate can
//! see and what it is told when a head is incomplete.

use super::*;
use crate::{FactCause, GuardError, JournalWrite};

#[test]
fn selected_fact_pages_use_the_kind_index_and_decode_only_matching_rows() {
    let journal = authority_fixture();
    let plan = journal
        .connection
        .query_row(
            "EXPLAIN QUERY PLAN SELECT seq, kind, envelope FROM fact_envelopes \
             WHERE seq > 0 AND seq <= 2 AND kind IN ('task::phased') \
             ORDER BY seq LIMIT 257",
            [],
            |row| row.get::<_, String>(3),
        )
        .expect("selected fact query plan");
    assert!(
        plan.contains("fact_envelopes_kind_seq"),
        "selected fact read must use its kind index: {plan}"
    );
    reset_indexed_read_metrics();
    let selected = journal
        .read(|read| {
            read.selected_fact_page(
                &[FactKind::TaskPhased],
                None,
                None,
                FactReadBudget::RETAINED,
            )
        })
        .expect("selected fact page read");
    assert_eq!(selected.rows.len(), 1);
    assert_eq!(selected.rows[0].seq, seq(2));
    assert_eq!(selected.rows[0].fact.kind(), FactKind::TaskPhased);
    assert_eq!(selected.continuation, None);
    assert_eq!(selected.horizon, Some(seq(2)));
    let metrics = indexed_read_metrics();
    assert_eq!(metrics.queries, 1);
    assert_eq!(metrics.rows, 1);
    assert!(metrics.decoded_bytes > 0);
}

#[test]
fn taste_pages_decode_only_the_named_bounded_frontier() {
    let mut journal = authority_fixture();
    for offset in 0..3 {
        append_taste_judgment(&mut journal, "target", offset);
    }
    let before = measured_taste_page(&journal);

    for offset in 3..303 {
        append_taste_judgment(&mut journal, "unrelated", offset);
    }
    let after = measured_taste_page(&journal);

    assert_eq!(after, before);
}

fn measured_taste_page(journal: &Journal) -> crate::guard::IndexedReadMetrics {
    reset_indexed_read_metrics();
    let page = journal
        .read(|read| {
            let horizon = read.cursor()?.expect("fixture has a journal cursor");
            read.taste_evidence_page("target", horizon, horizon, 3)
        })
        .expect("bounded taste page reads");
    assert_eq!((page.population, page.confirmed, page.cleared), (3, 0, 3));
    assert_eq!(page.rows.len(), 3);
    indexed_read_metrics()
}

fn append_taste_judgment(journal: &mut Journal, taste: &str, offset: u64) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        format!("taste:test-{offset}")
            .parse()
            .expect("fixture origin"),
        instant(),
        wire::Producer::Judge,
    );
    let judged: wire::body::TasteJudged = serde_json::from_value(json!({
        "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
        "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c4",
        "compaction": 1,
        "taste": taste,
        "content": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bag": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "trigger": taste,
        "vendor": "omp",
        "source": format!("fixture prose {offset}"),
        "locus": {
            "source": "018f82d4-6f59-7d06-92e3-bce9131163c5",
            "member": 0,
            "offset": offset,
            "length": 1,
            "event": 0,
            "line": 1,
            "column": 1
        },
        "attempts": [],
        "decision": {"verdict": "cleared"}
    }))
    .expect("cleared judgment fixture decodes");
    write
        .fact(judged, FactCause::Root)
        .expect("taste judgment builds");
    journal.apply(write).expect("taste judgment commits");
}

/// 300 added then 300 phased facts after the two authority facts.
fn retained_history() -> Journal {
    let journal = authority_fixture();
    for position in 3..=302 {
        insert_fact(
            &journal,
            fact(
                position,
                None,
                FactKind::TaskAdded,
                json!({
                    "task": "1kKRRsDPx",
                    "title": "unrelated retained authority",
                    "pitch": "Unrelated authority must stay readable.",
                    "topic": {"path": "/journal"},
                    "depends": [],
                    "flow": ["draft", "todo", "review"],
                    "phase": "todo",
                    "revision": seq(position),
                    "creation": {"kind": "unavailable", "reason": "principal-actor"}
                }),
            ),
        );
    }
    for position in 303..=602 {
        insert_fact(
            &journal,
            fact(
                position,
                None,
                FactKind::TaskPhased,
                json!({"task": "1kKRRsDPx", "phase": "review"}),
            ),
        );
    }
    advance_cursor(&journal, 602);
    journal
}

#[test]
fn retained_fact_pages_bound_matching_rows_and_bytes_independently_of_history() {
    let journal = retained_history();
    let row_budget =
        FactReadBudget::new(4, MAX_RETAINED_FACT_PAGE_BYTES).expect("small row budget is admitted");
    reset_indexed_read_metrics();
    let first = journal
        .read(|read| read.selected_fact_page(&[FactKind::TaskPhased], None, None, row_budget))
        .expect("bounded retained page");
    assert_eq!(
        first.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(2), seq(303), seq(304), seq(305)]
    );
    assert_eq!(first.continuation, Some(seq(305)));
    let row_metrics = indexed_read_metrics();
    assert_eq!((row_metrics.queries, row_metrics.rows), (1, 5));
    assert!(row_metrics.decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64);

    reset_indexed_read_metrics();
    let second = journal
        .read(|read| {
            read.selected_fact_page(
                &[FactKind::TaskPhased],
                first.continuation,
                None,
                row_budget,
            )
        })
        .expect("retained page resumes after its cursor");
    assert_eq!(
        second.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(306), seq(307), seq(308), seq(309)]
    );
    assert_eq!(second.continuation, Some(seq(309)));
    let resumed_metrics = indexed_read_metrics();
    assert_eq!((resumed_metrics.queries, resumed_metrics.rows), (1, 5));
    assert!(resumed_metrics.decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64);

    let first_bytes: i64 = journal
        .connection
        .query_row(
            "SELECT length(envelope) FROM fact_envelopes WHERE seq = ?1",
            [sql_seq(2)],
            |row| row.get(0),
        )
        .expect("measure first matching fact");
    let first_bytes = usize::try_from(first_bytes).expect("fact length fits this platform");
    let byte_budget = FactReadBudget::new(MAX_RETAINED_FACT_PAGE_ROWS, first_bytes)
        .expect("exact first-row byte budget is admitted");
    reset_indexed_read_metrics();
    let bytes = journal
        .read(|read| read.selected_fact_page(&[FactKind::TaskPhased], None, None, byte_budget))
        .expect("byte-bounded retained page");
    assert_eq!(bytes.rows.len(), 1);
    assert_eq!(bytes.continuation, Some(seq(2)));
    let byte_metrics = indexed_read_metrics();
    assert_eq!(byte_metrics.queries, 1);
    assert_eq!(byte_metrics.rows, 2);
    assert_eq!(byte_metrics.decoded_bytes, first_bytes as u64);

    let too_small = FactReadBudget::new(MAX_RETAINED_FACT_PAGE_ROWS, first_bytes - 1)
        .expect("smaller positive byte budget is admitted");
    reset_indexed_read_metrics();
    let indivisible = journal
        .read(|read| read.selected_fact_page(&[FactKind::TaskPhased], None, None, too_small))
        .expect("a first indivisible row advances beyond the aggregate target");
    assert_eq!(indivisible.rows.len(), 1);
    assert_eq!(indivisible.continuation, Some(seq(2)));
    assert_eq!(indexed_read_metrics().decoded_bytes, first_bytes as u64);

    reset_indexed_read_metrics();
    let recent = journal
        .read(|read| read.recent_fact_page(FactKind::TaskPhased, None, row_budget))
        .expect("bounded newest-first retained page");
    assert_eq!(
        recent.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(602), seq(601), seq(600), seq(599)]
    );
    assert_eq!(recent.continuation, Some(seq(599)));
    let recent_metrics = indexed_read_metrics();
    assert_eq!((recent_metrics.queries, recent_metrics.rows), (1, 5));
    assert!(recent_metrics.decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64);
}

#[test]
fn recent_pages_of_every_kind_visit_each_fact_once_newest_first() {
    let journal = retained_history();
    // Every kind, newest first: pages over the whole history resume strictly
    // before their continuation, so following them visits each seq once.
    let mut before = None;
    let mut visited = Vec::new();
    loop {
        reset_indexed_read_metrics();
        let page = journal
            .read(|read| read.recent_facts(before, FactReadBudget::RETAINED))
            .expect("bounded newest-first page of every kind");
        assert!(page.rows.len() <= MAX_RETAINED_FACT_PAGE_ROWS);
        assert!(indexed_read_metrics().decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64);
        visited.extend(page.rows.iter().map(|fact| fact.seq));
        match page.continuation {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    assert_eq!(visited, (1..=602).rev().map(seq).collect::<Vec<_>>());
}

#[test]
fn historical_selected_pages_keep_their_upper_bound_across_appends() {
    let journal = authority_fixture();
    insert_fact(
        &journal,
        fact(
            3,
            None,
            FactKind::TaskPhased,
            json!({"task": "1kKRRsDPx", "phase": "todo"}),
        ),
    );
    advance_cursor(&journal, 3);
    let budget = FactReadBudget::new(1, MAX_RETAINED_FACT_PAGE_BYTES).expect("one-row budget");
    let first = journal
        .read(|read| read.selected_fact_page(&[FactKind::TaskPhased], None, Some(seq(3)), budget))
        .expect("historical first page");
    assert_eq!(
        first.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(2)]
    );
    assert_eq!(first.continuation, Some(seq(2)));
    assert_eq!(first.horizon, Some(seq(3)));

    insert_fact(
        &journal,
        fact(
            4,
            None,
            FactKind::TaskPhased,
            json!({"task": "1kKRRsDPx", "phase": "review"}),
        ),
    );
    advance_cursor(&journal, 4);
    let second = journal
        .read(|read| {
            read.selected_fact_page(
                &[FactKind::TaskPhased],
                first.continuation,
                Some(seq(3)),
                budget,
            )
        })
        .expect("historical continuation after an append");
    assert_eq!(
        second.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(3)]
    );
    assert_eq!(second.continuation, None);
    assert_eq!(second.horizon, Some(seq(3)));

    assert!(matches!(
        journal.read(|read| {
            read.selected_fact_page(&[FactKind::TaskPhased], None, Some(seq(5)), budget)
        }),
        Err(GuardError::Invalid(_))
    ));
    assert!(matches!(
        journal.read(|read| read.selected_fact_page(&[], Some(seq(4)), Some(seq(3)), budget)),
        Err(GuardError::Invalid(_))
    ));
}

#[test]
fn ordinary_retained_pages_keep_decoded_bytes_within_the_aggregate_target() {
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

    let assert_bounded = |read: &dyn Fn() -> Result<(), GuardError>| {
        reset_indexed_read_metrics();
        let _ = read();
        assert!(
            indexed_read_metrics().decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64,
            "an ordinary retained page decoded beyond its aggregate target"
        );
    };
    assert_bounded(&|| {
        journal.read(|read| {
            read.selected_fact_page(
                &[FactKind::JournalBacked],
                Some(seq(2)),
                None,
                FactReadBudget::RETAINED,
            )
            .map(drop)
        })
    });
    assert_bounded(&|| {
        journal.read(|read| {
            read.recent_fact_page(FactKind::JournalBacked, None, FactReadBudget::RETAINED)
                .map(drop)
        })
    });
    assert_bounded(&|| journal.read(|read| read.latest_fact(FactKind::JournalBacked).map(drop)));
    assert_bounded(&|| {
        journal.read(|read| {
            read.recent_origin_facts(
                &"task:1kKRRsDPx".parse().expect("fixture origin"),
                FactKind::JournalBacked,
                None,
                fact_budget(2),
            )
            .map(drop)
        })
    });
    assert_bounded(&|| {
        journal.read(|read| {
            read.fact_page(FactPageStart::At(seq(3)), seq(4), fact_budget(2))
                .map(drop)
        })
    });
    assert_bounded(&|| journal.read(|read| read.causal_page(seq(4), fact_budget(2)).map(drop)));
}

#[test]
fn recent_origin_facts_resume_after_a_byte_bounded_prefix_without_fault() {
    let journal = authority_fixture();
    for position in [3, 4] {
        insert_fact(
            &journal,
            fact(
                position,
                None,
                FactKind::JournalBacked,
                text_body("x".repeat(600_000)),
            ),
        );
    }
    advance_cursor(&journal, 4);
    let origin = "task:1kKRRsDPx".parse().expect("fixture origin");

    reset_indexed_read_metrics();
    let first = journal
        .read(|read| {
            read.recent_origin_facts(&origin, FactKind::JournalBacked, None, fact_budget(2))
        })
        .expect("first byte-bounded recent-origin page");
    assert_eq!(
        first.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(4)]
    );
    assert_eq!(first.continuation, Some(seq(4)));
    assert!(indexed_read_metrics().decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64);

    reset_indexed_read_metrics();
    let second = journal
        .read(|read| {
            read.recent_origin_facts(
                &origin,
                FactKind::JournalBacked,
                first.continuation,
                fact_budget(2),
            )
        })
        .expect("second byte-bounded recent-origin page");
    assert_eq!(
        second.rows.iter().map(|fact| fact.seq).collect::<Vec<_>>(),
        [seq(3)]
    );
    assert_eq!(second.continuation, None);
    assert!(indexed_read_metrics().decoded_bytes <= MAX_RETAINED_FACT_PAGE_BYTES as u64);
}

#[test]
fn retained_fact_readers_refuse_oversized_corruption_before_decoding_it() {
    let journal = authority_fixture();
    journal
        .connection
        .execute_batch(
            "DROP TRIGGER fact_envelopes_envelope_size_insert;
             DROP TRIGGER fact_envelopes_envelope_size_update;",
        )
        .expect("open a corruption fixture beneath finalized enforcement");
    insert_fact(
        &journal,
        fact(
            3,
            None,
            FactKind::TaskAdded,
            json!({
                "task": "2kKRRsDPx",
                "title": "x".repeat(MAX_DURABLE_FACT_ENVELOPE_BYTES + 1),
                "pitch": "An oversized title must refuse.",
                "topic": {"path": "/journal"},
                "depends": [],
                "flow": ["draft", "todo", "review"],
                "phase": "todo",
                "revision": 1,
                "creation": {"kind": "unavailable", "reason": "principal-actor"}
            }),
        ),
    );
    advance_cursor(&journal, 3);
    journal
        .connection
        .execute_batch(&crate::migrate::bounds_sql())
        .expect("restore finalized enforcement around planted corruption");

    reset_indexed_read_metrics();
    journal
        .read(|read| {
            read.selected_fact_page(
                &[FactKind::TaskAdded],
                Some(seq(2)),
                None,
                FactReadBudget::RETAINED,
            )
        })
        .expect_err("oversized selected fact is corruption");
    assert_eq!(indexed_read_metrics().decoded_bytes, 0);

    reset_indexed_read_metrics();
    journal
        .read(|read| read.recent_fact_page(FactKind::TaskAdded, None, FactReadBudget::RETAINED))
        .expect_err("oversized recent fact is corruption");
    assert_eq!(indexed_read_metrics().decoded_bytes, 0);

    journal
        .fact_snapshot()
        .expect_err("unbounded snapshot still rejects before allocating corruption");
}

#[test]
fn retained_fact_budget_refuses_zero_and_platform_maximum_work() {
    reset_indexed_read_metrics();
    for (rows, bytes) in [(0, 1), (1, 0), (usize::MAX, 1), (1, usize::MAX)] {
        assert!(FactReadBudget::new(rows, bytes).is_err());
    }
    assert_eq!(indexed_read_metrics(), Default::default());
}

#[test]
fn newest_fact_reads_are_indexed_and_obey_their_page_limit() {
    let journal = authority_fixture();
    let latest_plan = journal
        .connection
        .query_row(
            "EXPLAIN QUERY PLAN SELECT seq, kind, envelope FROM fact_envelopes \
             WHERE kind = 'task::phased' AND seq <= 2 ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(3),
        )
        .expect("latest fact query plan");
    assert!(
        latest_plan.contains("fact_envelopes_kind_seq"),
        "latest fact read must use its kind index: {latest_plan}"
    );
    let origin_plan = journal
        .connection
        .query_row(
            "EXPLAIN QUERY PLAN SELECT seq, kind, envelope FROM fact_envelopes \
             WHERE origin = 'task:1kKRRsDPx' AND kind = 'task::phased' AND seq <= 2 \
             ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(3),
        )
        .expect("recent origin fact query plan");
    assert!(
        origin_plan.contains("fact_envelopes_origin_kind_seq"),
        "recent origin read must use its compound index: {origin_plan}"
    );
    reset_indexed_read_metrics();
    let (latest, recent) = journal
        .read(|read| {
            Ok((
                read.latest_fact(FactKind::TaskPhased)?,
                read.recent_origin_facts(
                    &"task:1kKRRsDPx".parse().expect("fixture origin"),
                    FactKind::TaskPhased,
                    None,
                    fact_budget(1),
                )?,
            ))
        })
        .expect("bounded newest fact reads");
    assert_eq!(latest.expect("latest fact").seq, seq(2));
    assert_eq!(recent.rows.len(), 1);
    assert_eq!(recent.rows[0].seq, seq(2));
    let metrics = indexed_read_metrics();
    assert_eq!((metrics.queries, metrics.rows), (2, 2));
    assert!(metrics.decoded_bytes > 0);
}

#[test]
fn bounded_current_head_pages_resume_with_exact_work_and_witnesses() {
    let journal = authority_fixture();
    let db = &journal.connection;
    crate::guard::verify_task_page_plan(db).expect("current-head plans stay engine-bounded");
    let mut second = task_head();
    second.born = "1kKRRsDQx".parse().expect("second task stamp");
    second.title = "Second journal task".to_owned();
    second.topic = "/journal/other".parse().expect("fixture topic");
    insert_task_head(db, "1kKRRsDQx", seq(1), &second);
    let workflow = workflow_head();
    insert_head(db, "workflow_heads", "stamp", "1kKRRsDQx", 2, &workflow);
    let completion = wire::CompletionHead::opened(seq(2));
    insert_head(db, "completion_heads", "stamp", "1kKRRsDQx", 2, &completion);
    let (first_digest, second_digest) = (digest("a"), digest("b"));
    let second_trust = TrustHead {
        digest: second_digest.clone(),
        state: TrustState::Granted,
        worktree: None,
    };
    let trust_key = second_digest.as_str();
    insert_head(db, "trust_heads", "digest", trust_key, 2, &second_trust);
    let bytes = |sql| {
        let value = db
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .expect("measure stored head bodies");
        usize::try_from(value).expect("stored body length fits usize")
    };
    let task_bytes = bytes(
        "SELECT length(t.body)+length(w.body)+length(p.body)+length(c.body)+length(l.body) FROM task_heads t JOIN workflow_heads w USING(stamp) JOIN completion_heads p USING(stamp) JOIN claim_heads c USING(stamp) JOIN lease_heads l USING(stamp) WHERE stamp='1kKRRsDPx'",
    );
    let trust_bytes = bytes("SELECT max(length(body)) FROM trust_heads");
    let task_budget = FactReadBudget::new(2, task_bytes).expect("task byte budget");
    let trust_budget = FactReadBudget::new(2, trust_bytes).expect("trust byte budget");
    // A task page reads its joined heads, then its claims, in two queries.
    let assert_work = |queries, rows, bytes| {
        let metrics = indexed_read_metrics();
        assert_eq!((metrics.queries, metrics.rows), (queries, rows));
        assert!((1..=bytes as u64).contains(&metrics.decoded_bytes));
    };
    reset_indexed_read_metrics();
    let task_first = journal
        .read(|read| read.task_head_page(None, task_budget))
        .expect("first task byte page reads");
    assert_eq!(
        task_first.continuation.as_ref().map(TaskStamp::as_str),
        Some("1kKRRsDPx")
    );
    assert_work(2, 3, task_bytes);
    reset_indexed_read_metrics();
    let task_second = journal
        .read(|read| read.task_head_page(task_first.continuation.as_ref(), task_budget))
        .expect("task page resumes after its stable stamp");
    assert_eq!(task_second.continuation, None);
    assert_eq!(task_second.witness, task_first.witness);
    assert_work(2, 1, task_bytes);
    assert_eq!(task_first.witness.cursor, Some(seq(2)));
    assert_eq!(
        task_first.rows[0].statement().body().born.as_str(),
        "1kKRRsDPx"
    );
    assert_eq!(
        task_second.rows[0].statement().body().born.as_str(),
        "1kKRRsDQx"
    );
    reset_indexed_read_metrics();
    let trust_first = journal
        .read(|read| read.trust_head_page(None, trust_budget))
        .expect("first trust byte page reads");
    assert_eq!(trust_first.continuation, Some(first_digest.clone()));
    assert_eq!(trust_first.rows[0].body().digest, first_digest);
    assert_work(1, 2, trust_bytes);
    reset_indexed_read_metrics();
    let trust_second = journal
        .read(|read| read.trust_head_page(trust_first.continuation.as_ref(), trust_budget))
        .expect("trust page resumes after its stable digest");
    assert_eq!(trust_second.continuation, None);
    assert_eq!(trust_second.witness, trust_first.witness);
    assert_eq!(trust_second.rows[0].body().digest, second_digest);
    assert_work(1, 1, trust_bytes);
}

#[test]
fn task_bundle_read_names_every_incomplete_or_malformed_head_class() {
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let corrupted = |sql: &str| {
        let journal = authority_fixture();
        let db = &journal.connection;
        db.pragma_update(None, "foreign_keys", false)
            .expect("allow corrupt fixture");
        db.execute_batch(sql).expect("plant corruption");
        journal
    };
    let assert_error = |journal: &Journal, trust, expected: &str| {
        let error = journal
            .read(|read| {
                if trust {
                    read.trust_head_page(None, FactReadBudget::RETAINED)
                        .map(drop)
                } else {
                    read.task_head_page(None, FactReadBudget::RETAINED)
                        .map(drop)
                }
            })
            .expect_err("malformed heads refuse");
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, found {error}"
        );
    };
    let missing = corrupted("DELETE FROM workflow_heads WHERE stamp='1kKRRsDPx'");
    assert_error(
        &missing,
        false,
        "task 1kKRRsDPx has a statement head but no workflow head",
    );
    let loose = corrupted(
        "INSERT INTO claim_heads(stamp,claimed,head_seq,body) SELECT '1kKRRsDQx',claimed,head_seq,body FROM claim_heads WHERE stamp='1kKRRsDPx'",
    );
    assert_error(&loose, false, "task 1kKRRsDQx has loose claim head(s)");
    let mismatch = corrupted(&format!(
        "INSERT INTO task_heads(stamp,head_seq,body) SELECT '1kKRRsDQx',head_seq,body FROM task_heads WHERE stamp='1kKRRsDPx'; INSERT INTO workflow_heads(stamp,head_seq,body) SELECT '1kKRRsDQx',head_seq,body FROM workflow_heads WHERE stamp='1kKRRsDPx'; INSERT INTO completion_heads(stamp,head_seq,body) SELECT '1kKRRsDQx',head_seq,body FROM completion_heads WHERE stamp='1kKRRsDPx'; UPDATE trust_heads SET digest='{B}' WHERE digest='{A}';"
    ));
    assert_error(
        &mismatch,
        false,
        "task head indexed as 1kKRRsDQx carries body stamp 1kKRRsDPx",
    );
    assert_error(
        &mismatch,
        true,
        &format!("trust head indexed as {B} carries body digest {A}"),
    );
    let malformed = corrupted(&format!(
        "UPDATE workflow_heads SET body=X'00' WHERE stamp='1kKRRsDPx'; UPDATE trust_heads SET body=X'00' WHERE digest='{A}';"
    ));
    assert_error(
        &malformed,
        false,
        "workflow_heads stamp \"1kKRRsDPx\" is invalid",
    );
    assert_error(
        &malformed,
        true,
        &format!("trust_heads digest \"{A}\" is invalid"),
    );
    let beyond = corrupted(&format!(
        "UPDATE workflow_heads SET head_seq={third} WHERE stamp='1kKRRsDPx'; UPDATE trust_heads SET head_seq={third} WHERE digest='{A}';",
        third = sql_seq(3),
    ));
    assert_error(
        &beyond,
        false,
        &format!(
            "workflow_heads stamp \"1kKRRsDPx\" head position {} is beyond cursor Some({})",
            seq(3),
            sql_seq(2)
        ),
    );
    assert_error(
        &beyond,
        true,
        &format!(
            "trust_heads digest \"{A}\" head position {} is beyond cursor Some({})",
            seq(3),
            sql_seq(2)
        ),
    );
}

#[test]
fn cursor_storage_refuses_backward_movement() {
    let journal = authority_fixture();
    assert!(
        journal
            .connection
            .execute(
                "UPDATE cursor_head SET seq = ?1, wake = ?1 WHERE singleton = 1",
                [sql_seq(1)],
            )
            .is_err()
    );
}
