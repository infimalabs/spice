use super::*;
/// Expand a card's media and render its Markdown: the shown text, then its
/// rendering. Prose cards split into attributed hunks around every quote and
/// sent SYN, speaking for the card's lane. Provider failures keep their exact
/// received text but lead it with the same card signoff. Tool, reasoning, and
/// other boundary cards render verbatim, and a tool card shows no words.
pub(super) fn render_agent_markdown(
    props: &AgentMessageCardProps,
    is_tool: bool,
    is_prose: bool,
    signed_body: bool,
) -> (
    std::borrow::Cow<'_, str>,
    crate::composer_markdown::RenderedMarkdown,
) {
    let message = &props.message;
    let (quote_agents, quote_sources) = (&*props.quote_agents, &*props.quote_sources);
    let shown_text = if is_tool { "" } else { message.text.as_str() };
    let shown = crate::composer_markdown::expand_media_payloads(
        shown_text,
        &message.media,
        &message.markdown_images,
        &message.directive_contexts,
        &message.sends,
    )
    .expect("message media passed admission before browser rendering");
    let rendered = if is_prose {
        let card = crate::composer_markdown::QuoteSource {
            card_id: Some(agent_message_source_id(message)),
            display_millis: message.display_millis(),
            agent: None,
        };
        crate::composer_markdown::render_prose_card_with_media_quotes(
            &shown.markdown,
            &shown.directives,
            &shown.sends,
            &message.quotes,
            crate::composer_markdown::QuoteRenderContext::new(
                quote_agents,
                quote_sources,
                &card,
                Some(props.member.lane),
            ),
            &message.media_quotes,
        )
    } else if signed_body {
        crate::composer_markdown::received::render_attributed_received_with_media_quotes(
            &shown.markdown,
            &shown.directives,
            &message.quotes,
            quote_agents,
            quote_sources,
            crate::composer_markdown::received::AttributedReceivedContext {
                media_quotes: &message.media_quotes,
                speaker: Some(props.member.lane),
            },
        )
    } else {
        crate::composer_markdown::render_received_with_media_quotes(
            &shown.markdown,
            &shown.directives,
            &message.quotes,
            quote_agents,
            quote_sources,
            &message.media_quotes,
        )
    };
    (shown.markdown, rendered)
}
