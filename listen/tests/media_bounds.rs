use listen::{Decoded, Dialect, Event, Locus, ProseRole, ResidueKind, ToolMoment, decode};

const DISTINCT_PNGS: [&str; 9] = [
    "iVBORw0KGgoA",
    "iVBORw0KGgoB",
    "iVBORw0KGgoC",
    "iVBORw0KGgoD",
    "iVBORw0KGgoE",
    "iVBORw0KGgoF",
    "iVBORw0KGgoG",
    "iVBORw0KGgoH",
    "iVBORw0KGgoI",
];

fn locus(bytes: &[u8]) -> Locus {
    Locus::new(4, 19, bytes.len() as u64)
}

fn omp_image_blocks() -> Vec<serde_json::Value> {
    DISTINCT_PNGS
        .iter()
        .map(|data| {
            serde_json::json!({
                "type": "image",
                "data": data,
                "mimeType": "image/png",
            })
        })
        .collect()
}

#[test]
fn omp_tool_results_keep_text_and_first_eight_images() {
    let mut content = vec![serde_json::json!({"type": "text", "text": "nine screenshots"})];
    content.extend(omp_image_blocks());
    let record = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "message": {
            "role": "toolResult",
            "toolCallId": "call-screenshots",
            "content": content,
            "isError": false,
        },
    }))
    .expect("OMP screenshot result fixture encodes");
    let decoded =
        decode(Dialect::Omp, &record, locus(&record)).expect("OMP screenshot result decodes");
    let [Event::Tool(tool)] = decoded.events() else {
        panic!("OMP screenshot result remains one tool event");
    };
    let ToolMoment::Result(result) = &tool.moment else {
        panic!("OMP screenshot result remains a result");
    };
    assert_eq!(tool.call_id, "call-screenshots");
    assert_eq!(result.output.as_deref(), Some("nine screenshots"));
    assert_eq!(result.media.len(), wire::media::MAX_MEDIA_ITEMS);
    assert!(result.media[0].data_url.ends_with(DISTINCT_PNGS[0]));
    assert!(
        result.media[wire::media::MAX_MEDIA_ITEMS - 1]
            .data_url
            .ends_with(DISTINCT_PNGS[wire::media::MAX_MEDIA_ITEMS - 1])
    );
}

#[test]
fn omp_user_messages_keep_prose_and_first_eight_images() {
    let mut content = vec![serde_json::json!({"type": "text", "text": "compare every capture"})];
    content.extend(omp_image_blocks());
    let record = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "message": {
            "role": "user",
            "content": content,
        },
    }))
    .expect("OMP image prompt fixture encodes");
    let decoded = decode(Dialect::Omp, &record, locus(&record)).expect("OMP image prompt decodes");
    let [Event::Prose(prose)] = decoded.events() else {
        panic!("OMP image prompt remains one prose event");
    };
    assert_eq!(prose.role, ProseRole::Operator);
    assert_eq!(prose.text, "compare every capture");
    assert_eq!(prose.media.len(), wire::media::MAX_MEDIA_ITEMS);
}

#[test]
fn omp_surplus_media_does_not_hide_a_malformed_later_block() {
    let mut content = omp_image_blocks();
    content.push(serde_json::json!({
        "type": "image",
        "data": "not-base64",
        "mimeType": "image/png",
    }));
    let record = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "message": {
            "role": "toolResult",
            "toolCallId": "call-malformed-tail",
            "content": content,
            "isError": false,
        },
    }))
    .expect("OMP malformed-tail fixture encodes");
    let decoded =
        decode(Dialect::Omp, &record, locus(&record)).expect("OMP malformed-tail locus matches");
    assert!(matches!(
        decoded,
        Decoded::Residue(ref residue)
            if matches!(residue.kind, ResidueKind::Malformed { field: "media", .. })
    ));
}
