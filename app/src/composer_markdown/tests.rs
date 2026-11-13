use super::*;

mod attribution;
mod quote_cards;

const OPENABLE_PNG: &str = "data:image/png;base64,iVBORw0KGgo=";

fn steering_target(key: &str, text: &str) -> wire::QuoteTarget {
    wire::QuoteTarget::Steering {
        key: key.to_owned(),
        digest: wire::QuoteTarget::text_digest(text),
    }
}

fn quote_source(card_id: &str, display_millis: i64) -> QuoteSource {
    QuoteSource {
        card_id: Some(card_id.to_owned()),
        display_millis,
        agent: None,
    }
}

/// Words `author` said to no recorded recipient, delivered as no packet.
fn attributed(author: wire::QuoteAuthor) -> wire::QuoteAttribution {
    wire::QuoteAttribution {
        author,
        recipients: Vec::new(),
        packet: false,
    }
}

// Principal directive fixture.
fn principal_fixture() -> wire::QuoteAuthor {
    wire::QuoteAuthor::Directive {
        actor: wire::Actor::TEST_PRINCIPAL,
    }
}

fn principal(principal: wire::CanonicalUuid) -> wire::QuoteAuthor {
    wire::QuoteAuthor::Directive {
        actor: wire::Actor::Principal(principal),
    }
}

fn tape_target(text: &str) -> wire::QuoteTarget {
    wire::QuoteTarget::Tape {
        lane: "018f82d4-6f59-7d06-92e3-bce9131163e6"
            .parse()
            .expect("fixture lane is canonical"),
        source: "018f82d4-6f59-7d06-92e3-bce9131163e7"
            .parse()
            .expect("fixture source is canonical"),
        locus: wire::MessageLocus {
            member: 1,
            offset: 24,
            length: 80,
            event: 2,
        },
        digest: wire::QuoteTarget::text_digest(text),
    }
}

fn quoted<'a>(values: &'a [(String, &'a str)]) -> Vec<QuotedReply<'a>> {
    values
        .iter()
        .map(|(target, body)| QuotedReply {
            target,
            body,
            attachments: &[],
        })
        .collect()
}

#[test]
fn tex_and_dollar_math_delimiters_reach_the_canonical_renderer_slots() {
    let rendered = render_received(
        "Inline \\(x^2 + y^2\\) and $z^2$.\n\n\
         \\[100 \\times \\frac{\\sum \\text{remaining}}{\\sum \\text{capacity}}\\]\n\n\
         $$\\sum_{i=1}^{n} i$$",
    );

    assert_eq!(
        rendered.html.matches("class=\"math math-inline\"").count(),
        2
    );
    assert_eq!(
        rendered.html.matches("class=\"math math-display\"").count(),
        2
    );
    assert!(
        rendered
            .html
            .contains(r#"100 \times \frac{\sum \text{remaining}}{\sum \text{capacity}}"#)
    );
}

#[test]
fn math_aliases_preserve_code_escaped_literals_and_authenticated_ranges() {
    let literals = render_received(
        "Prices are $5 and $10, escaped \\$9, and code is `\\(x\\) $$y$$`.\n\n\
         ```text\n\
         \\[not math\\] and $$still code$$\n\
         ```\n\n\
         Escaped \\\\[brackets\\\\] remain prose.",
    );
    assert!(!literals.html.contains("class=\"math "));
    assert!(literals.html.contains("<code>\\(x\\) $$y$$</code>"));
    assert!(literals.html.contains("\\[not math\\]"));

    let quoted = render_received("> \\[x^2\\]");
    assert!(quoted.html.contains("class=\"math math-display\""));
}

#[test]
fn composed_markdown_emits_canonical_target_links_in_visible_shard_order() {
    assert_eq!(compose::<Media>("  body only  ", &[], &[]), "body only");
    let tape = tape_target("agent source text");
    let directive = steering_target("dTVQdQfLw", "directive source text");
    let quotes = [(tape.to_string(), ""), (directive.to_string(), "reply")];
    assert_eq!(
        compose("body", &quoted(&quotes), &[]),
        format!(
            "{}\n\n{}\n\nreply\n\nbody",
            tape.markdown(),
            directive.markdown()
        )
    );
}

#[test]
fn operator_typed_block_quotes_remain_ordinary_composed_markdown() {
    let typed = "> copied words\n>\n> still operator-authored";
    assert_eq!(compose::<Media>(typed, &[], &[]), typed);
}

#[test]
fn room_card_words_render_as_markdown_exactly_as_written_after_their_signoff() {
    let html = render_room_card(
        "Plan\n\n- [ ] ship it\n- [x] review it\n\nno period",
        &[],
        &QuoteAgents::new(),
        &QuoteSources::new(),
    )
    .html;

    assert!(html.contains(SIGNOFF_SLOT), "{html}");
    assert_eq!(html.matches("type=\"checkbox\"").count(), 2, "{html}");
    assert!(html.contains("no period</p>"), "{html}");
}

#[test]
fn block_quotes_normalize_every_line_ending_before_prefixing() {
    assert_eq!(block_quote("first\r\nsecond"), "> first\n> second");
    assert_eq!(block_quote("first\rsecond"), "> first\n> second");
    assert_eq!(block_quote("first\n\nthird"), "> first\n> \n> third");
}

#[test]
fn a_quote_that_ends_on_a_blank_line_keeps_the_marker_on_every_line() {
    let quote = block_quote("first\n\n");

    assert_eq!(quote, "> first");
    assert!(
        quote.lines().all(|line| line.starts_with("> ")),
        "every line of a block quote carries the marker, got {quote:?}"
    );
}

#[test]
fn whitespace_only_operator_block_quote_text_composes_to_nothing() {
    assert_eq!(block_quote("   \n\t\n  "), "");
    assert_eq!(block_quote(""), "");
}

#[test]
fn every_task_handle_occurrence_renders_in_source_order() {
    let rendered = render_received(
        "Plain CONTROL-1kKbC3j4x, CONTROL-1kKbC3j4x, and `CONTROL-1kKbC3j4x`.\n\n\
             Then MOSAIC-1kRYqQxyx, IDENTIT-dRyDYZqJJ, and CONTROL-1kKbC3j4x!",
    );
    let handles = rendered
        .html
        .split("data-task-handle=\"")
        .skip(1)
        .filter_map(|tail| tail.split('"').next())
        .collect::<Vec<_>>();

    assert_eq!(
        handles,
        [
            "CONTROL-1kKbC3j4x",
            "CONTROL-1kKbC3j4x",
            "CONTROL-1kKbC3j4x",
            "MOSAIC-1kRYqQxyx",
            "IDENTIT-dRyDYZqJJ",
            "CONTROL-1kKbC3j4x",
        ]
    );
}

#[test]
fn task_chip_hover_escapes_supplied_context() {
    let contextual =
        render_received("CONTROL-1kKbC3j4x · Diagnose \"quoted\" rollbacks & preserve authority.");
    assert!(
        contextual
            .html
            .contains("title=\"Diagnose “quoted” rollbacks &amp; preserve authority\""),
        "{}",
        contextual.html
    );
}

#[test]
fn task_chip_hover_context_stops_at_its_unicode_boundary() {
    let handle = "CONTROL-1kKbC3j4x";
    let text = format!("{handle} · {}", "é".repeat(TASK_CHIP_CONTEXT_CHARS + 20));
    let context = task_chip_context(&text, &(0..handle.len())).expect("context remains");

    assert_eq!(context.chars().count(), TASK_CHIP_CONTEXT_CHARS);
    assert!(context.ends_with('…'));
}

#[test]
fn quoted_first_and_nested_mentions_do_not_consume_later_prose_occurrences() {
    let rendered = render_received(
        "> Quoted CONTROL-1kKbC3j4x.\n\
             > > Nested `CONTROL-1kKbC3j4x`.\n\n\
             Authored CONTROL-1kKbC3j4x and CONTROL-1kKbC3j4x.",
    );
    let marker = "data-task-handle=\"CONTROL-1kKbC3j4x\"";

    assert_eq!(
        rendered.html.matches(marker).count(),
        4,
        "{}",
        rendered.html
    );
    assert_eq!(rendered.html.matches("<blockquote>").count(), 2);
    assert!(rendered.html.contains("Quoted "));
    assert!(rendered.html.contains("Nested "));
    assert!(rendered.html.contains("Authored "));
    assert!(
        rendered.html.rfind(marker).expect("authored chip renders")
            > rendered
                .html
                .rfind("</blockquote>")
                .expect("quoted context closes"),
        "the authored occurrence follows the quoted context: {}",
        rendered.html,
    );
}

#[test]
fn authenticated_path_binding_preserves_alt_and_title() {
    let media = Media::from_base64("provider-name.png", "image/png", "iVBORw0KGgo=")
        .expect("fixture media");
    let binding = wire::MarkdownImageBinding {
        destination: "/tmp/capture_(1).png".to_owned(),
        media: media.clone(),
    };
    let markdown = "![authored alt](/tmp/capture_(1).png \"Authored title\")\n\n\
                    [ordinary link](/tmp/capture_(1).png)";

    let ExpandedMedia {
        markdown: expanded, ..
    } = expand_media_payloads(markdown, &[], std::slice::from_ref(&binding), &[], &[])
        .expect("bound image expands");
    assert!(expanded.contains(&format!(
        "![authored alt]({} \"Authored title\")",
        media.data_url
    )));
    assert!(expanded.contains("[ordinary link](/tmp/capture_(1).png)"));
    let rendered = render_received(&expanded);
    assert!(rendered.html.contains("alt=\"authored alt\""));
    assert!(rendered.html.contains("title=\"Authored title\""));
}

#[test]
fn review_and_taste_words_are_ordinary_markdown() {
    let rendered = render_received(
        "> APP-1kKRRsDRx\n>\n\
             > > missing current coverage\n\n\
             > [REVIEW] Quoted prose is not metadata.\n\n\
             > [TASTE] compatibility: Keep one tested path.",
    );
    assert!(rendered.html.contains("data-task-handle=\"APP-1kKRRsDRx\""));
    assert!(
        rendered
            .html
            .contains("[REVIEW] Quoted prose is not metadata.")
    );
    assert!(
        rendered
            .html
            .contains("[TASTE] compatibility: Keep one tested path.")
    );
    assert!(!rendered.html.contains("message-review-pill"));
    assert!(!rendered.html.contains("message-taste-pill"));
}

#[test]
fn acknowledgement_markers_remain_ordinary_markdown() {
    let rendered = render_received(
        "> [ACK] Quoted marker.\n\n\
         [ACK PENDING] Direct prose.\n\n\
         > `[NAK]` inline code.\n\n\
         > ```text\n> [ACK] fenced code\n> ```",
    );

    assert!(!rendered.html.contains("message-ack-pill"));
    assert!(rendered.html.contains("[ACK] Quoted marker."));
    assert!(rendered.html.contains("[ACK PENDING] Direct prose."));
    assert!(rendered.html.contains("<code>[NAK]</code>"));
    assert!(rendered.html.contains("[ACK] fenced code"));
}

#[test]
fn every_code_block_boxes_one_copy_control_with_its_pre() {
    let rendered = render_received(
        "Run `inline`.\n\n```sh\necho \"a\" > b\n```\n\n    indented\n\n\
         ```mermaid\ngraph TD; A-->B\n```\n\n> ```\n> quoted\n> ```",
    );

    assert_eq!(rendered.html.matches("data-code-copy").count(), 3);
    // The copy listener reads the `pre` directly inside the control's box.
    for code in [
        "<pre><code class=\"language-sh\">echo \"a\" &gt; b\n</code></pre>\n</div>",
        "<pre><code>indented\n</code></pre>\n</div>",
        "<pre><code>quoted\n</code></pre>\n</div>",
    ] {
        assert!(
            rendered.html.contains(&format!("</button>\n{code}")),
            "{code} lacks its boxed control in {}",
            rendered.html
        );
    }
    assert!(rendered.html.contains("data-mermaid-slot=\"0\""));
}

#[test]
fn noncanonical_and_block_code_handles_remain_ordinary_markdown() {
    let rendered = render_received("`APP-a1` and APP-a1.\n\n```text\nCONTROL-1kKbC3j4x\n```");

    assert!(!rendered.html.contains("message-task-chip"));
    assert!(
        rendered.diagrams.is_empty(),
        "ordinary code must not request diagram rendering"
    );
    assert!(rendered.html.contains("<code>APP-a1</code> and APP-a1."));
    assert!(rendered.html.contains("CONTROL-1kKbC3j4x"));
}

#[test]
fn every_image_only_markdown_shape_renders_one_card_wide_stack() {
    let image = |name: &str| format!("![{name}]({OPENABLE_PNG})");
    let single = render_received(&image("one"));
    assert!(single.image_only);
    assert_eq!(single.image_count, 1);

    let shapes = [
        [image("one"), image("two"), image("three")].join(" "),
        [image("one"), image("two"), image("three")].join("\n"),
        [image("one"), image("two"), image("three")].join("  \n"),
        [image("one"), image("two"), image("three")].join("\n\n"),
    ];

    for markdown in shapes {
        let rendered = render_received(&markdown);
        assert!(
            rendered.image_only,
            "shape must receive the large tier: {markdown:?}"
        );
        assert_eq!(rendered.image_count, 3, "{markdown:?}");
        assert_eq!(
            rendered
                .html
                .matches("class=\"message-image-stack\"")
                .count(),
            1,
            "all image blocks must share one stack: {}",
            rendered.html,
        );
        assert_eq!(
            rendered
                .html
                .matches("class=\"message-image message-image--openable\"")
                .count(),
            3,
            "every preview must remain present: {}",
            rendered.html,
        );
        assert!(!rendered.html.contains("<a "));
    }
}

#[test]
fn image_actions_preserve_the_stack_without_admitting_script_urls() {
    let rendered = render_received(
        "![remote](https://example.test/image.png)\n\n\
             ![vector](data:image/svg+xml;base64,PHN2Zz4=)\n\n\
             ![refused](javascript:alert(1))",
    );

    assert!(rendered.image_only);
    assert_eq!(
        rendered
            .html
            .matches("class=\"message-image-stack\"")
            .count(),
        1
    );
    assert_eq!(rendered.html.matches("<button ").count(), 2);
    assert_eq!(
        rendered
            .html
            .matches("<span class=\"message-image\"")
            .count(),
        1
    );
    assert!(!rendered.html.contains("javascript:"));
}

#[test]
fn linked_images_keep_one_image_action_without_navigation() {
    let image_only = render_received(
        "[![one](https://images.example.test/one.png)](https://links.example.test/one)\n\n\
             [![two](https://images.example.test/two.png)](https://links.example.test/two)",
    );

    assert!(image_only.image_only);
    assert_eq!(image_only.image_count, 2);
    assert_eq!(
        image_only
            .html
            .matches("class=\"message-image-stack\"")
            .count(),
        1
    );
    assert_eq!(image_only.html.matches("<button ").count(), 2);
    assert!(!image_only.html.contains("<a "));
    assert!(
        image_only
            .html
            .contains("src=\"https://images.example.test/one.png\"")
    );
    assert!(
        image_only
            .html
            .contains("src=\"https://images.example.test/two.png\"")
    );
    assert!(!image_only.html.contains("links.example.test"));

    let mixed = render_received(
        "See [![preview](https://images.example.test/mixed.png)](https://links.example.test/mixed) now.",
    );
    assert!(!mixed.image_only);
    assert_eq!(mixed.image_count, 0);
    assert_eq!(mixed.html.matches("<button ").count(), 1);
    assert!(!mixed.html.contains("<a "));
    assert!(
        mixed
            .html
            .contains("src=\"https://images.example.test/mixed.png\"")
    );
    assert!(!mixed.html.contains("links.example.test"));
}

#[test]
fn prose_and_images_keep_the_ordinary_rendering_path() {
    let rendered = render_received("ordinary prose\n\n![preview](https://example.test/image.png)");

    assert!(!rendered.image_only);
    assert!(rendered.html.starts_with("<p>ordinary prose</p>\n"));
    assert_eq!(
        rendered
            .html
            .matches("class=\"message-image-stack\"")
            .count(),
        1
    );
    assert!(!rendered.html.contains("<a "));
}

#[test]
fn every_image_preview_owns_an_explicit_initial_lifecycle() {
    let remote = render_received("![remote](https://example.test/image.png)");
    assert!(remote.html.contains("data-mosaic-image-state=\"pending\""));

    let cached = render_received(&format!("![cached]({OPENABLE_PNG})"));
    assert!(cached.html.contains("data-mosaic-image-state=\"pending\""));
    assert_eq!(cached.html.matches("data-mosaic-image-status=").count(), 1);

    let malformed = render_received("![broken](data:image/png;base64,%%%)");
    assert!(malformed.html.contains("data-mosaic-image-state=\"error\""));
}
