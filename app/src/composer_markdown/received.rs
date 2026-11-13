use super::*;
/// Render received text, its resident-resolved structural quote links, and a
/// byline leading each directive context the resident quoted into it.
pub(crate) fn render_received_with_quotes(
    markdown: &str,
    directives: &[MessageDirective],
    quotes: &[wire::QuotePresentation],
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
) -> RenderedMarkdown {
    render(
        markdown,
        Annotations {
            directives,
            sends: &[],
        },
        quotes,
        quote_agents,
        quote_sources,
        &[],
        Finish::Received,
    )
}

/// Render received text with exact quote targets for its retained images.
pub(crate) fn render_received_with_media_quotes(
    markdown: &str,
    directives: &[MessageDirective],
    quotes: &[wire::QuotePresentation],
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
    media_quotes: &[wire::MediaQuote],
) -> RenderedMarkdown {
    render(
        markdown,
        Annotations {
            directives,
            sends: &[],
        },
        quotes,
        quote_agents,
        quote_sources,
        media_quotes,
        Finish::Received,
    )
}

pub(crate) struct AttributedReceivedContext<'a> {
    pub(crate) media_quotes: &'a [wire::MediaQuote],
    pub(crate) speaker: Option<wire::CanonicalUuid>,
}

/// Render received text exactly as authored, with one leading card signoff.
pub(crate) fn render_attributed_received_with_media_quotes(
    markdown: &str,
    directives: &[MessageDirective],
    quotes: &[wire::QuotePresentation],
    quote_agents: &QuoteAgents,
    quote_sources: &QuoteSources,
    context: AttributedReceivedContext<'_>,
) -> RenderedMarkdown {
    let AttributedReceivedContext {
        media_quotes,
        speaker,
    } = context;
    render(
        markdown,
        Annotations {
            directives,
            sends: &[],
        },
        quotes,
        quote_agents,
        quote_sources,
        media_quotes,
        Finish::ProseCard {
            card: &SENDLESS_CARD,
            speaker,
            punctuate: false,
        },
    )
}

#[cfg(test)]
pub(crate) fn render_attributed_received(markdown: &str) -> RenderedMarkdown {
    render_attributed_received_with_media_quotes(
        markdown,
        &[],
        &[],
        &QuoteAgents::new(),
        &BTreeMap::new(),
        AttributedReceivedContext {
            media_quotes: &[],
            speaker: None,
        },
    )
}
