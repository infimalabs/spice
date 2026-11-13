use super::*;

fn head_with(intent: Option<wire::TaskIntent>, revision: u64) -> TaskHead {
    TaskHead {
        intent,
        revision: seq(revision),
        ..task_head()
    }
}

fn replace_task_head(journal: &Journal, head: &TaskHead) {
    journal
        .connection
        .execute(
            "UPDATE task_heads SET head_seq = ?1, body = ?2 WHERE stamp = '1kKRRsDPx'",
            params![
                sql_position(head.revision),
                stored_task_head(&journal.connection, head.revision, head)
            ],
        )
        .expect("replace task head fixture");
}

/// Admit one explicit intent change for the fixture task at seq 3.
fn admit_intent_edit(journal: &Journal, edit: serde_json::Value) {
    insert_fact(
        journal,
        fact(
            3,
            None,
            FactKind::TaskEdited,
            json!({"task": "1kKRRsDPx", "revision": 2, "intent": edit}),
        ),
    );
    journal
        .connection
        .execute_batch(&format!(
            "INSERT INTO thread_index(seq, origin, cause, root, depth) VALUES ({third}, 'task:1kKRRsDPx', NULL, {third}, 0);
             UPDATE cursor_head SET seq = {third} WHERE singleton = 1;
             UPDATE steering_index SET indexed_seq = {third} WHERE singleton = 1;",
            third = sql_seq(3),
        ))
        .expect("index the intent edit");
}

fn intent_violations(journal: &Journal) -> Vec<String> {
    match journal.verify() {
        Ok(_) => Vec::new(),
        Err(error) => error
            .violations()
            .iter()
            .filter(|violation| {
                violation.class == ViolationClass::Head
                    && violation.subject == "task_heads:1kKRRsDPx"
            })
            .map(ToString::to_string)
            .collect(),
    }
}

#[test]
fn statement_intent_is_exactly_the_intent_its_task_facts_admitted() {
    // A statement head that declares intent no fact admitted is not one any
    // admission could have written.
    let journal = authority_fixture();
    replace_task_head(&journal, &head_with(Some(wire::TaskIntent::Plan), 1));
    let refused = intent_violations(&journal);
    assert_eq!(refused.len(), 1, "{refused:?}");

    // An admitted change is authority: the head that follows it verifies, and
    // one that ignores it does not.
    let journal = authority_fixture();
    admit_intent_edit(&journal, json!({"action": "set", "intent": "design"}));
    replace_task_head(&journal, &head_with(Some(wire::TaskIntent::Design), 3));
    journal
        .verify()
        .expect("an admitted intent change verifies");
    replace_task_head(&journal, &head_with(None, 3));
    let refused = intent_violations(&journal);
    assert_eq!(refused.len(), 1, "{refused:?}");
}
