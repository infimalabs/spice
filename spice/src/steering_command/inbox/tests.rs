use super::*;
use wire::intent::LaneInbox;

/// Pending directives across both fixture lanes; well past one page.
const POPULATION: usize = 300;
/// Every `OVERDUE_EVERY`th directive is overdue.
const OVERDUE_EVERY: u128 = 7;
/// Rows per page while following continuations.
const PAGE_ROWS: usize = 40;
/// A direction far beyond any text limit.
const OVERSIZED: usize = 1 << 20;
/// The interactive default byte budget a text page must stay under.
const TERMINAL_BYTES: usize = 8 * 1024;

fn uuid(value: u128) -> CanonicalUuid {
    CanonicalUuid::new(uuid::Uuid::from_u128(value))
}

fn directive(ordinal: u128, stage: SteeringStage, text: &str) -> PendingDirective {
    PendingDirective {
        directive: uuid(0x1000 + ordinal),
        key: format!("k{ordinal:04}"),
        fanout: None,
        cc: Vec::new(),
        text: text.to_owned(),
        from: None,
        sender: None,
        principal: None,
        room: None,
        addressed: None,
        wakes: true,
        preview: None,
        control: None,
        line: None,
        attachments: Vec::new(),
        authored: format!("2026-01-01T00:{:02}:{:02}.000Z", ordinal / 60, ordinal % 60)
            .parse()
            .expect("fixture instant"),
        stage,
        escalations: Vec::new(),
        expires: "2027-01-01T00:00:00.000Z".parse().expect("fixture expiry"),
    }
}

/// Two lanes with 300 pending directives; every seventh is overdue.
fn crowded() -> InboxAnswer {
    let lane = |id, range: std::ops::Range<u128>| LaneInbox {
        lane: uuid(id),
        occupant: None,
        compaction: None,
        pending: range
            .map(|ordinal| {
                let stage = if ordinal % OVERDUE_EVERY == 0 {
                    SteeringStage::Overdue
                } else {
                    SteeringStage::Published
                };
                directive(ordinal, stage, "direction")
            })
            .collect(),
        lines: Vec::new(),
    };
    InboxAnswer {
        epoch: uuid(1),
        lanes: vec![lane(0xa, 0..150), lane(0xb, 150..POPULATION as u128)],
    }
}

fn page(arguments: &[String]) -> Page {
    let matches = crate::cli::command()
        .try_get_matches_from(std::iter::once("spice").chain(arguments.iter().map(String::as_str)))
        .expect("inbox syntax parses");
    Page::from_matches(crate::cli::selected_matches(&matches), &["inbox"]).expect("bounds admit")
}

#[test]
fn every_pending_key_is_indexed_and_continuations_page_all_detail_most_urgent_first() {
    let mut arguments = vec![
        "inbox".to_owned(),
        "--limit".to_owned(),
        PAGE_ROWS.to_string(),
    ];
    let mut shown = Vec::new();
    for _ in 0..POPULATION {
        let view = project(crowded(), page(&arguments)).expect("page admits");
        // The index never pages: every key on every lane, on every page.
        assert_eq!(
            view.lanes.iter().map(|lane| lane.keys.len()).sum::<usize>(),
            POPULATION
        );
        assert!(view.pending.rows.len() <= PAGE_ROWS);
        shown.extend(view.pending.rows.iter().map(|row| row.stage));
        let Some(omission) = view.pending.omission else {
            break;
        };
        assert_eq!(omission.withheld, POPULATION - shown.len());
        arguments = omission.continuation.arguments().to_vec();
    }
    assert_eq!(
        shown.len(),
        POPULATION,
        "every directive appears exactly once"
    );
    let overdue = shown
        .iter()
        .take_while(|stage| **stage == SteeringStage::Overdue)
        .count();
    assert_eq!(
        overdue,
        POPULATION.div_ceil(OVERDUE_EVERY as usize),
        "overdue direction leads before any published row"
    );
}

#[test]
fn oversized_direction_is_clipped_to_the_text_limit_and_text_matches_json_omission() {
    let mut inbox = crowded();
    inbox.lanes[0].pending[0].text = "x".repeat(OVERSIZED);
    let view = project(
        inbox,
        page(&["inbox", "--max-text", "64", "--limit", "5"].map(str::to_owned)),
    )
    .expect("page admits");
    let json = serde_json::to_value(&view).expect("view serializes");
    let first = &view.pending.rows[0];
    assert_eq!(first.key, "k0000");
    assert!(first.text.to_string().len() < 2 * 64, "{}", first.text);

    let text = view.to_string();
    let omission = view.pending.omission.as_ref().expect("page withholds");
    assert!(text.ends_with(&omission.to_string()), "{text}");
    assert_eq!(json["pending"]["omission"]["withheld"], omission.withheld);
    assert!(text.len() < TERMINAL_BYTES, "{} bytes", text.len());
}
