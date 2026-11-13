use super::*;

fn entries(history: &InputHistory, lane: &str) -> Vec<String> {
    history.entries(lane).to_vec()
}

#[test]
fn each_lane_keeps_its_own_sent_text_once_across_restore() {
    let mut history = InputHistory::default();
    history.record("a", ["first", "  ", "second"]);
    history.record("b", ["other"]);
    history.record("a", ["second", "first"]);

    assert_eq!(entries(&history, "a"), ["first", "second", "first"]);
    assert_eq!(entries(&history, "b"), ["other"]);
    assert_eq!(InputHistory::restore(&history.document()), history);
    assert!(InputHistory::restore("{not json").entries("a").is_empty());
}

#[test]
fn retention_keeps_each_lanes_newest_entries_and_trims_least_recent_lanes_first() {
    let mut history = InputHistory::default();
    let texts = (0..LANE_ENTRIES + 5)
        .map(|index| format!("entry {index}"))
        .collect::<Vec<_>>();
    history.record("a", texts.iter().map(String::as_str));
    assert_eq!(history.entries("a").len(), LANE_ENTRIES);
    assert_eq!(history.entries("a")[0], "entry 5");

    let large = "x".repeat(BUDGET_BYTES / 4);
    for lane in ["old", "middle", "new"] {
        history.record(lane, [large.as_str(), &format!("{large}{lane}")]);
    }
    history.record("a", ["latest"]);

    assert!(history.document().len() <= BUDGET_BYTES);
    assert!(history.entries("old").is_empty());
    assert_eq!(history.entries("new").len(), 2);
    assert_eq!(history.entries("a"), ["latest"]);
}

#[test]
fn a_restored_document_beyond_the_bounds_is_trimmed_before_use() {
    let large = "y".repeat(BUDGET_BYTES / 10);
    let mut lanes = (0..11)
        .map(|lane| LaneHistory {
            lane: format!("lane-{lane}"),
            entries: vec![large.clone()],
        })
        .collect::<Vec<_>>();
    lanes.push(LaneHistory {
        lane: "recent".to_owned(),
        entries: (0..=LANE_ENTRIES).map(|index| index.to_string()).collect(),
    });
    let oversized = InputHistory { lanes }.document();

    let restored = InputHistory::restore(&oversized);

    assert!(oversized.len() > BUDGET_BYTES);
    assert!(restored.document().len() <= BUDGET_BYTES);
    assert!(restored.entries("lane-0").is_empty());
    assert_eq!(restored.entries("lane-10"), [large]);
    assert_eq!(restored.entries("recent").len(), LANE_ENTRIES);
    assert_eq!(restored.entries("recent")[0], "1");
}

#[test]
fn recall_walks_a_snapshot_keeping_the_draft_and_edits_until_the_lane_sends() {
    let mut lane = vec!["one".to_owned(), "two".to_owned()];
    let mut recall = Recall::default();
    let older = |recall: &mut Recall, lane: &[String], current: &str| {
        recall.older(|| lane.to_vec(), current)
    };

    assert_eq!(recall.newer("draft"), None);
    assert_eq!(older(&mut recall, &lane, "draft").as_deref(), Some("two"));
    lane.push("other tab".to_owned());
    assert_eq!(
        older(&mut recall, &lane, "two edited").as_deref(),
        Some("one")
    );
    assert_eq!(older(&mut recall, &lane, "one"), None);
    assert_eq!(recall.newer("one").as_deref(), Some("two edited"));
    assert_eq!(recall.newer("two edited").as_deref(), Some("draft"));
    assert_eq!(recall.newer("draft"), None);
    assert_eq!(
        older(&mut recall, &lane, "draft changed").as_deref(),
        Some("two edited")
    );
    recall.sync(0);
    assert_eq!(recall.newer("two").as_deref(), Some("draft changed"));
    assert_eq!(
        older(&mut recall, &lane, "draft changed").as_deref(),
        Some("two")
    );

    recall.sync(1);
    assert_eq!(recall.newer("two"), None);
    assert_eq!(older(&mut recall, &lane, "").as_deref(), Some("other tab"));
}

#[test]
fn edge_handoff_requires_arrival_then_a_repeated_direction() {
    let mut recall = Recall::default();

    assert!(!recall.edge_keydown(RecallEdge::Up, 12, 12, false));
    recall.edge_selection(4, 4);
    assert!(recall.edge_keydown(RecallEdge::Up, 4, 4, true));

    recall.edge_reset();
    assert!(!recall.edge_keydown(RecallEdge::Up, 0, 0, true));
    recall.edge_keyup(RecallEdge::Up, 0, 0);
    // Chromium may deliver the native caret's select event after keyup.
    recall.edge_selection(0, 0);
    assert!(recall.edge_keydown(RecallEdge::Up, 0, 0, true));
    recall.edge_reset();
    assert!(!recall.edge_keydown(RecallEdge::Down, 2, 2, false));
    recall.edge_selection(8, 8);
    assert!(recall.edge_keydown(RecallEdge::Down, 8, 8, true));
}

#[test]
fn edge_handoff_resets_and_history_replacement_rearms_it() {
    let mut recall = Recall::default();

    assert!(!recall.edge_keydown(RecallEdge::Up, 0, 0, true));
    recall.edge_keyup(RecallEdge::Up, 0, 0);
    assert!(!recall.edge_keydown(RecallEdge::Down, 0, 0, true));
    recall.edge_keyup(RecallEdge::Down, 5, 5);
    assert!(recall.edge_keydown(RecallEdge::Down, 5, 5, true));

    recall.edge_keyup(RecallEdge::Down, 5, 5);
    recall.edge_selection(2, 2);
    assert!(!recall.edge_keydown(RecallEdge::Down, 2, 2, true));
    assert!(!recall.edge_keydown(RecallEdge::Up, 1, 2, true));

    recall.edge_reset();
    assert!(!recall.edge_keydown(RecallEdge::Up, 0, 0, true));
    recall.edge_keyup(RecallEdge::Up, 0, 0);
    assert!(recall.edge_keydown(RecallEdge::Up, 0, 0, true));
    recall.edge_replaced(RecallEdge::Up, 0);
    recall.edge_keyup(RecallEdge::Up, 0, 0);
    // Chromium may deliver the replacement's select event after keyup.
    recall.edge_selection(0, 0);
    recall.edge_selection(0, 0);
    assert!(recall.edge_keydown(RecallEdge::Up, 0, 0, true));

    recall.edge_reset();
    assert!(!recall.edge_keydown(RecallEdge::Up, 0, 0, true));
}
