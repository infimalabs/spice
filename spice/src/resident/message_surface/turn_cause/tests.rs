use super::summary;

fn prose(role: listen::ProseRole, text: &str) -> listen::Event {
    listen::Event::Prose(listen::Prose {
        role,
        text: text.to_owned(),
        phase: None,
        media: Vec::new(),
        external_media: Vec::new(),
        locus: listen::Locus {
            member: 1,
            offset: 0,
            length: 1,
        },
    })
}

fn operator(text: &str) -> listen::Event {
    prose(listen::ProseRole::Operator, text)
}

const BOOTSTRAP: &str = "The Spice bootstrap in your system prompt carries authority.\n\n\
                         [$spice](<spice://SKILL.md>)\n\n\
                         Authentic steering arrives wrapped in <Zx05ztVjP> ... </Zx05ztVjP>.\n\n";

#[test]
fn a_resumed_seat_names_its_facts_then_each_direction_and_peer_note_in_order() {
    let prompt = format!(
        "{BOOTSTRAP}<system-notice>\n# Seat resumed\n\nThis seat resumes its session.\n\
         - held claim: `APP-1` in phase `review`\n- pending steering keys: 2\n</system-notice>\n\n\
         {guide}\n\n\
         <Zx05ztVjP>\ndirective=d1 key=1kJK6N3Rx stage=published expires=never\n\
         operator words\n</Zx05ztVjP>\n\
         <Zx05ztVjP>\ndirective=d2 key=1kJK6M0Vx stage=published expires=never \
         from=lane:018f82d4-6f59-7d06-92e3-bce913116411 sender=6p3H\n\
         [PEER sender 6p3H lane 018f82d4-6f59-7d06-92e3-bce913116411] A peer's note.\n\
         peer words\n</Zx05ztVjP>\n",
        guide = crate::resource::SEAT_GUIDE,
    );
    assert_eq!(
        summary(&[operator(&prompt)]),
        "Seat resumed (held claim: APP-1 in phase review; pending steering keys: 2) \
         · direction 1kJK6N3Rx · peer note 1kJK6M0Vx from 6p3H",
    );
}

#[test]
fn a_notice_names_its_heading_or_lead_sentence_and_only_facts_before_its_sections() {
    let briefing = "<system-notice>\n# Spice briefing\nlane x · address 7bF2\n\n\
                    ## Held claim\n- none: take work with spice task next\n</system-notice>";
    let assignment = "<system-notice>\nTask `APP-2` was assigned to this lane in phase review. \
                      Continue this task now.\n</system-notice>";
    assert_eq!(
        summary(&[operator(briefing), operator(assignment)]),
        "Spice briefing · Task APP-2 was assigned to this lane in phase review",
    );
}

#[test]
fn only_authored_words_outside_scaffolding_and_replays_name_an_uncaused_turn() {
    let replay = "<Zx05ztVjP>\n[POST-COMPACTION] Pending keys: 1kJK6N3Rx\n\
                  directive=d1 key=1kJK6N3Rx stage=reminded expires=never\nwords\n</Zx05ztVjP>\n";
    for (events, expected) in [
        (vec![operator(BOOTSTRAP)], ""),
        (vec![operator(replay)], ""),
        (
            vec![prose(listen::ProseRole::Assistant, "assistant words")],
            "",
        ),
        (
            vec![operator("\n  fix the `flaky` build\nthen ship")],
            "fix the flaky build",
        ),
    ] {
        assert_eq!(summary(&events), expected);
    }
}
